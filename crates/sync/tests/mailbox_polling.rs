//! SYNC-007 信箱轮询状态机测试。
//!
//! 覆盖：有序投递与游标前进、同 item id 幂等去重（含内容冲突拒绝）、乱序暂存与提升、
//! 游标回退/无进展拒绝、ACK 失败保留进度、空页退避不空转、拉取失败退避、
//! 双预算与暂存/群上限、seed_group 对齐、非法输入有界拒绝。

use std::collections::VecDeque;

use chat_sync::clock::{Clock, FixedClock};
use chat_sync::mailbox::{
    ACK_FAILURE_BACKOFF_MS, AckSink, EMPTY_PAGE_BACKOFF_MS, MailboxCursor, MailboxItem,
    MailboxItemId, MailboxItemKind, MailboxPage, MailboxPoller, MailboxSource, MAX_HELD_ITEMS,
    MAX_MAILBOX_ITEM_BYTES, MAX_TRACKED_GROUPS, PAGE_INTERVAL_MS, PULL_FAILURE_BASE_BACKOFF_MS,
    PollStatus,
};
use chat_sync::{ErrorCode, SyncError};

const NOW: i64 = 1_700_000_000_000;
const GROUP: &str = "grp-1";

struct PageSource {
    pages: VecDeque<Result<MailboxPage, ErrorCode>>,
    pulls: usize,
}

impl PageSource {
    fn new(pages: Vec<Result<MailboxPage, ErrorCode>>) -> Self {
        Self {
            pages: pages.into(),
            pulls: 0,
        }
    }

    fn from_pages(pages: Vec<MailboxPage>) -> Self {
        Self::new(pages.into_iter().map(Ok).collect())
    }
}

impl MailboxSource for PageSource {
    fn pull(&mut self, cursor: &MailboxCursor, _limit: u32) -> Result<MailboxPage, SyncError> {
        self.pulls += 1;
        match self.pages.pop_front() {
            Some(page) => page.map_err(|code| SyncError::new(code, "source failure")),
            None => Ok(MailboxPage {
                items: Vec::new(),
                next: cursor.clone(),
            }),
        }
    }
}

struct AckRecorder {
    fail_remaining: usize,
    acked: Vec<String>,
    calls: usize,
}

impl AckRecorder {
    fn new() -> Self {
        Self {
            fail_remaining: 0,
            acked: Vec::new(),
            calls: 0,
        }
    }

    fn failing(times: usize) -> Self {
        Self {
            fail_remaining: times,
            ..Self::new()
        }
    }
}

impl AckSink for AckRecorder {
    fn ack(&mut self, item_ids: &[MailboxItemId]) -> Result<(), SyncError> {
        self.calls += 1;
        if self.fail_remaining > 0 {
            self.fail_remaining -= 1;
            return Err(SyncError::new(
                ErrorCode::NetworkUnavailable,
                "ack offline",
            ));
        }
        for id in item_ids {
            self.acked.push(id.as_str().to_owned());
        }
        Ok(())
    }
}

fn item(id: &str, position: u64, kind: MailboxItemKind) -> MailboxItem {
    MailboxItem {
        item_id: MailboxItemId::parse(id).expect("valid item id"),
        position,
        kind,
        group_id: None,
        group_seq: None,
        envelope: vec![0u8; 8],
    }
}

fn log_item(id: &str, position: u64, seq: u64) -> MailboxItem {
    MailboxItem {
        item_id: MailboxItemId::parse(id).expect("valid item id"),
        position,
        kind: MailboxItemKind::LogEntry,
        group_id: Some(GROUP.to_owned()),
        group_seq: Some(seq),
        envelope: vec![u8::try_from(seq % 251).unwrap(); 8],
    }
}

fn page(items: Vec<MailboxItem>, next_position: u64) -> MailboxPage {
    MailboxPage {
        items,
        next: MailboxCursor::from_parts(next_position, format!("c-{next_position}"))
            .expect("cursor"),
    }
}

fn poller() -> MailboxPoller {
    MailboxPoller::new(MailboxCursor::initial())
}

#[test]
fn ordered_items_are_delivered_and_the_cursor_advances() {
    let mut poller = poller();
    let mut clock = FixedClock::new(NOW);
    let mut source = PageSource::from_pages(vec![page(
        vec![
            item("m-1", 1, MailboxItemKind::Welcome),
            log_item("m-2", 2, 1),
            log_item("m-3", 3, 2),
            item("m-4", 4, MailboxItemKind::Receipt),
        ],
        4,
    )]);
    let mut ack = AckRecorder::new();

    let outcome = poller.tick(&clock, &mut source, &mut ack).expect("tick");
    assert_eq!(outcome.status, PollStatus::Polled);
    assert_eq!(outcome.received, 4);
    assert_eq!(outcome.held, 0);
    assert_eq!(outcome.duplicates, 0);
    assert_eq!(outcome.delivered.len(), 4);
    assert_eq!(outcome.cursor.position(), 4);
    assert_eq!(outcome.pending_ack, 4);
    assert_eq!(outcome.acked, 0, "ACK happens before the next pull");
    assert_eq!(outcome.next_poll_at_ms, NOW + PAGE_INTERVAL_MS);
    assert_eq!(source.pulls, 1);

    // 下一次 tick：先补 ACK，再拉空页（空页退避）。
    clock.advance(PAGE_INTERVAL_MS);
    let outcome = poller.tick(&clock, &mut source, &mut ack).expect("tick");
    assert_eq!(outcome.status, PollStatus::Polled);
    assert_eq!(outcome.received, 0);
    assert_eq!(outcome.acked, 4);
    assert_eq!(outcome.pending_ack, 0);
    assert_eq!(
        outcome.next_poll_at_ms,
        clock.now_unix_ms() + EMPTY_PAGE_BACKOFF_MS
    );
    assert_eq!(ack.acked.len(), 4);
}

#[test]
fn duplicate_pages_are_idempotent_and_not_redelivered() {
    let mut poller = poller();
    let mut clock = FixedClock::new(NOW);
    let first = page(vec![log_item("m-1", 1, 1), log_item("m-2", 2, 2)], 2);
    let replay = page(vec![log_item("m-1", 1, 1), log_item("m-2", 2, 2)], 3);
    let mut source = PageSource::from_pages(vec![first, replay]);
    let mut ack = AckRecorder::new();

    let first_outcome = poller.tick(&clock, &mut source, &mut ack).expect("first page");
    assert_eq!(first_outcome.delivered.len(), 2);
    // 间隔未到：不拉取。
    let idle = poller.tick(&clock, &mut source, &mut ack).expect("idle tick");
    assert_eq!(idle.status, PollStatus::Idle);

    clock.advance(PAGE_INTERVAL_MS);
    let replay_outcome = poller.tick(&clock, &mut source, &mut ack).expect("replay page");
    assert_eq!(replay_outcome.status, PollStatus::Polled);
    assert!(replay_outcome.delivered.is_empty(), "no double delivery");
    assert_eq!(replay_outcome.duplicates, 2);
    assert_eq!(replay_outcome.cursor.position(), 3);
}

#[test]
fn duplicate_item_id_with_different_content_is_rejected() {
    let mut poller = poller();
    let clock = FixedClock::new(NOW);
    let first = page(vec![log_item("m-1", 1, 1)], 1);
    let mut conflicting = log_item("m-1", 2, 2);
    conflicting.envelope = vec![0xff; 16];
    let second = page(vec![conflicting], 2);
    let mut source = PageSource::from_pages(vec![first, second]);
    let mut ack = AckRecorder::new();
    poller.tick(&clock, &mut source, &mut ack).expect("first page");
    let cursor_before = poller.cursor().position();
    let held_before = poller.held_len();

    let mut clock = FixedClock::new(NOW + PAGE_INTERVAL_MS);
    let error = poller
        .tick(&clock, &mut source, &mut ack)
        .expect_err("conflicting duplicate must fail");
    assert_eq!(error.code(), ErrorCode::InvalidCandidate);
    assert_eq!(poller.cursor().position(), cursor_before);
    assert_eq!(poller.held_len(), held_before);
    clock.advance(1);
}

#[test]
fn out_of_order_log_entries_are_held_then_promoted_in_order() {
    let mut poller = poller();
    let mut clock = FixedClock::new(NOW);
    let mut source = PageSource::from_pages(vec![
        page(vec![log_item("m-2", 1, 2), log_item("m-3", 2, 3)], 2),
        page(vec![log_item("m-1", 3, 1)], 3),
    ]);
    let mut ack = AckRecorder::new();

    let outcome = poller.tick(&clock, &mut source, &mut ack).expect("first page");
    assert_eq!(outcome.held, 2);
    assert!(outcome.delivered.is_empty(), "gap must not be delivered");
    assert_eq!(poller.held_len(), 2);
    assert_eq!(
        outcome.pending_ack, 2,
        "held items are durable, so they are acknowledged to stop resends"
    );

    clock.advance(PAGE_INTERVAL_MS);
    let outcome = poller.tick(&clock, &mut source, &mut ack).expect("second page");
    let delivered: Vec<u64> = outcome
        .delivered
        .iter()
        .map(|item| item.group_seq.unwrap())
        .collect();
    assert_eq!(delivered, vec![1, 2, 3], "promoted strictly in order");
    assert_eq!(poller.held_len(), 0);
}

#[test]
fn cursor_regression_and_no_progress_are_rejected() {
    let mut poller = poller();
    let mut clock = FixedClock::new(NOW);
    let mut ack = AckRecorder::new();
    let mut source = PageSource::from_pages(vec![page(vec![log_item("m-1", 1, 1)], 1)]);
    poller.tick(&clock, &mut source, &mut ack).expect("first page");

    // 游标回退。
    let mut regressed = PageSource::from_pages(vec![MailboxPage {
        items: Vec::new(),
        next: MailboxCursor::from_parts(0, "c-0").unwrap(),
    }]);
    clock.advance(PAGE_INTERVAL_MS);
    let error = poller
        .tick(&clock, &mut regressed, &mut ack)
        .expect_err("regressed cursor must fail");
    assert_eq!(error.code(), ErrorCode::InvalidCursor);
    assert_eq!(poller.cursor().position(), 1);

    // 非空页但没有推进游标。
    let mut stuck = PageSource::from_pages(vec![page(vec![log_item("m-2", 1, 1)], 1)]);
    clock.advance(EMPTY_PAGE_BACKOFF_MS);
    let error = poller
        .tick(&clock, &mut stuck, &mut ack)
        .expect_err("non-progressing page must fail");
    assert_eq!(error.code(), ErrorCode::InvalidCursor);

    // 页内条目位置必须严格递增。
    let mut unordered = PageSource::from_pages(vec![page(
        vec![log_item("m-9", 2, 2), log_item("m-8", 2, 3)],
        3,
    )]);
    clock.advance(EMPTY_PAGE_BACKOFF_MS);
    let error = poller
        .tick(&clock, &mut unordered, &mut ack)
        .expect_err("unordered positions must fail");
    assert_eq!(error.code(), ErrorCode::InvalidCursor);
}

#[test]
fn ack_failure_keeps_progress_and_is_retried() {
    let mut poller = poller();
    let mut clock = FixedClock::new(NOW);
    let mut source = PageSource::from_pages(vec![page(vec![log_item("m-1", 1, 1)], 1)]);
    let mut ack = AckRecorder::failing(1);
    let first = poller.tick(&clock, &mut source, &mut ack).expect("first page");
    assert_eq!(first.pending_ack, 1);
    let cursor_after_consume = poller.cursor().position();

    clock.advance(PAGE_INTERVAL_MS);
    let deferred = poller.tick(&clock, &mut source, &mut ack).expect("ack retry");
    assert_eq!(deferred.status, PollStatus::AckDeferred);
    assert_eq!(deferred.error, Some(ErrorCode::NetworkUnavailable));
    assert_eq!(deferred.acked, 0);
    assert_eq!(
        poller.cursor().position(),
        cursor_after_consume,
        "cursor progress must be kept"
    );
    assert_eq!(poller.pending_ack_len(), 1, "ACK set must be retained");
    assert_eq!(
        deferred.next_poll_at_ms,
        clock.now_unix_ms() + ACK_FAILURE_BACKOFF_MS
    );
    assert_eq!(source.pulls, 1, "ACK failure must not pull a new page");

    clock.advance(ACK_FAILURE_BACKOFF_MS);
    let recovered = poller.tick(&clock, &mut source, &mut ack).expect("ack retry");
    assert_eq!(recovered.status, PollStatus::Polled);
    assert_eq!(recovered.acked, 1);
    assert_eq!(poller.pending_ack_len(), 0);
    assert_eq!(ack.acked, vec!["m-1".to_owned()]);
}

#[test]
fn empty_pages_back_off_and_do_not_busy_loop() {
    let mut poller = poller();
    let mut clock = FixedClock::new(NOW);
    let mut source = PageSource::new(Vec::new());
    let mut ack = AckRecorder::new();

    let outcome = poller.tick(&clock, &mut source, &mut ack).expect("empty page");
    assert_eq!(outcome.status, PollStatus::Polled);
    assert_eq!(outcome.received, 0);
    assert_eq!(source.pulls, 1);
    assert_eq!(outcome.next_poll_at_ms, NOW + EMPTY_PAGE_BACKOFF_MS);

    clock.advance(EMPTY_PAGE_BACKOFF_MS - 1);
    let idle = poller.tick(&clock, &mut source, &mut ack).expect("idle tick");
    assert_eq!(idle.status, PollStatus::Idle);
    assert_eq!(source.pulls, 1, "must not poll before the backoff expires");

    clock.advance(1);
    let outcome = poller.tick(&clock, &mut source, &mut ack).expect("poll again");
    assert_eq!(outcome.status, PollStatus::Polled);
    assert_eq!(source.pulls, 2);
}

#[test]
fn pull_failures_back_off_exponentially_and_keep_the_cursor() {
    let mut poller = poller();
    let mut clock = FixedClock::new(NOW);
    let mut source = PageSource::new(vec![
        Err(ErrorCode::NodeUnavailable),
        Err(ErrorCode::NodeUnavailable),
    ]);
    let mut ack = AckRecorder::new();

    let first = poller.tick(&clock, &mut source, &mut ack).expect("failure tick");
    assert_eq!(first.status, PollStatus::PullFailed);
    assert_eq!(first.error, Some(ErrorCode::NodeUnavailable));
    assert_eq!(first.next_poll_at_ms, NOW + PULL_FAILURE_BASE_BACKOFF_MS);
    assert_eq!(poller.cursor().position(), 0);

    clock.advance(PULL_FAILURE_BASE_BACKOFF_MS);
    let second = poller.tick(&clock, &mut source, &mut ack).expect("failure tick");
    assert_eq!(second.status, PollStatus::PullFailed);
    assert_eq!(
        second.next_poll_at_ms,
        clock.now_unix_ms() + PULL_FAILURE_BASE_BACKOFF_MS * 2,
        "backoff doubles while offline"
    );
}

#[test]
fn page_and_item_budgets_are_enforced_without_state_change() {
    let mut poller = poller();
    let clock = FixedClock::new(NOW);
    let mut ack = AckRecorder::new();

    let mut oversized = log_item("m-1", 1, 1);
    oversized.envelope = vec![0u8; MAX_MAILBOX_ITEM_BYTES + 1];
    let mut source = PageSource::from_pages(vec![page(vec![oversized], 1)]);
    let error = poller
        .tick(&clock, &mut source, &mut ack)
        .expect_err("oversized item must fail");
    assert_eq!(error.code(), ErrorCode::ContentTooLarge);

    // 单页条目数超界。
    let too_many: Vec<MailboxItem> = (1..=101)
        .map(|index| log_item(&format!("m-{index}"), index, index))
        .collect();
    let mut source = PageSource::from_pages(vec![page(too_many, 101)]);
    let mut clock = FixedClock::new(NOW + EMPTY_PAGE_BACKOFF_MS);
    let error = poller
        .tick(&clock, &mut source, &mut ack)
        .expect_err("too many items must fail");
    assert_eq!(error.code(), ErrorCode::ContentTooLarge);

    // 单页字节总量超界（20 × 64 KiB > 1 MiB）。
    let mut big_items = Vec::new();
    for index in 1..=20u64 {
        let mut item = log_item(&format!("big-{index}"), index, index);
        item.envelope = vec![0u8; MAX_MAILBOX_ITEM_BYTES];
        big_items.push(item);
    }
    let mut source = PageSource::from_pages(vec![page(big_items, 20)]);
    clock.advance(EMPTY_PAGE_BACKOFF_MS);
    let error = poller
        .tick(&clock, &mut source, &mut ack)
        .expect_err("page byte budget must be enforced");
    assert_eq!(error.code(), ErrorCode::ContentTooLarge);
    assert_eq!(poller.cursor().position(), 0);
    assert_eq!(poller.held_len(), 0);
    assert_eq!(poller.pending_ack_len(), 0);
}

#[test]
fn hold_buffer_is_bounded() {
    let mut poller = poller();
    let mut clock = FixedClock::new(NOW);
    let mut ack = AckRecorder::new();
    let mut sequence = 2u64;
    let mut position = 1u64;
    let mut pages = Vec::new();
    for _ in 0..6 {
        let mut items = Vec::new();
        for _ in 0..100 {
            items.push(log_item(&format!("h-{sequence}"), position, sequence));
            position += 1;
            sequence += 1;
        }
        pages.push(page(items, position - 1));
    }
    let mut source = PageSource::from_pages(pages);
    let mut overflow = None;
    for _ in 0..6 {
        clock.advance(PAGE_INTERVAL_MS);
        match poller.tick(&clock, &mut source, &mut ack) {
            Ok(outcome) => assert_eq!(outcome.held, 100),
            Err(error) => {
                overflow = Some(error);
                break;
            }
        }
    }
    let error = overflow.expect("hold buffer must overflow explicitly");
    assert_eq!(error.code(), ErrorCode::MissingLogEntries);
    assert_eq!(
        poller.held_len(),
        5 * 100,
        "overflow page must roll back completely"
    );
    assert!(poller.held_len() < MAX_HELD_ITEMS);
}

#[test]
fn tracked_group_slots_are_bounded() {
    let mut poller = poller();
    let mut clock = FixedClock::new(NOW);
    let mut ack = AckRecorder::new();
    let mut position = 1u64;
    let mut pages = Vec::new();
    let mut group_index = 0usize;
    while group_index < MAX_TRACKED_GROUPS {
        let mut items = Vec::new();
        for _ in 0..100 {
            if group_index >= MAX_TRACKED_GROUPS {
                break;
            }
            let mut item = log_item(&format!("item-{group_index}"), position, 2);
            item.group_id = Some(format!("group-{group_index}"));
            items.push(item);
            position += 1;
            group_index += 1;
        }
        pages.push(page(items, position - 1));
    }
    let mut overflow_item = log_item("item-overflow", position, 2);
    overflow_item.group_id = Some("group-overflow".to_owned());
    pages.push(page(vec![overflow_item], position));
    let mut source = PageSource::from_pages(pages);

    let tick_count = MAX_TRACKED_GROUPS / 100 + 2;
    let mut overflow = None;
    for _ in 0..tick_count {
        clock.advance(PAGE_INTERVAL_MS);
        if let Err(error) = poller.tick(&clock, &mut source, &mut ack) {
            overflow = Some(error);
            break;
        }
    }
    let error = overflow.expect("group slot budget must be enforced");
    assert_eq!(error.code(), ErrorCode::ContentTooLarge);
}

#[test]
fn seed_group_aligns_the_expected_sequence() {
    let mut poller = poller();
    poller.seed_group("grp-seeded", 5).expect("seed");
    assert_eq!(
        poller.seed_group("grp-seeded", 0).unwrap_err().code(),
        ErrorCode::InvalidTarget
    );
    let clock = FixedClock::new(NOW);
    let mut items = Vec::new();
    for (index, seq) in [5u64, 6].iter().enumerate() {
        let mut item = log_item(&format!("m-{seq}"), index as u64 + 1, *seq);
        item.group_id = Some("grp-seeded".to_owned());
        items.push(item);
    }
    let mut source = PageSource::from_pages(vec![page(items, 2)]);
    let mut ack = AckRecorder::new();
    let outcome = poller.tick(&clock, &mut source, &mut ack).expect("tick");
    let delivered: Vec<u64> = outcome
        .delivered
        .iter()
        .map(|item| item.group_seq.unwrap())
        .collect();
    assert_eq!(delivered, vec![5, 6]);
}

#[test]
fn unknown_item_kinds_are_delivered_without_interpretation() {
    let mut poller = poller();
    let clock = FixedClock::new(NOW);
    let mut source = PageSource::from_pages(vec![page(
        vec![item("m-1", 1, MailboxItemKind::Unknown)],
        1,
    )]);
    let mut ack = AckRecorder::new();
    let outcome = poller.tick(&clock, &mut source, &mut ack).expect("tick");
    assert_eq!(outcome.delivered.len(), 1);
    assert_eq!(outcome.delivered[0].kind, MailboxItemKind::Unknown);
    assert_eq!(outcome.held, 0);
}

#[test]
fn malformed_ids_and_cursors_are_rejected() {
    assert_eq!(
        MailboxItemId::parse("bad item id!").unwrap_err().code(),
        ErrorCode::InvalidId
    );
    assert_eq!(
        MailboxItemId::parse("x".repeat(129)).unwrap_err().code(),
        ErrorCode::InvalidId
    );
    assert_eq!(
        MailboxCursor::from_parts(1, "t".repeat(1025))
            .unwrap_err()
            .code(),
        ErrorCode::InvalidCursor
    );
    assert_eq!(
        MailboxCursor::from_parts(1, "bad\ntoken").unwrap_err().code(),
        ErrorCode::InvalidCursor
    );
    // tick 拒绝负时钟。
    let mut poller = poller();
    let clock = FixedClock::new(-1);
    let mut source = PageSource::new(Vec::new());
    let mut ack = AckRecorder::new();
    assert_eq!(
        poller
            .tick(&clock, &mut source, &mut ack)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidTarget
    );
}
