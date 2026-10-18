//! CORE-003/009：取消三态与事件总线（序号单调、幂等、溢出 resync、关闭事件）。
//!
//! 全部走真实加密库 + 真实 UNiFFI 前置的 `ConversationCore`：
//! - 取消语义完全由 `SendService::cancel_draft` 决定，本层不重写三态；
//! - 事件只含 ID 与稳定状态文本（绝不含正文）；
//! - 失败（未知草稿/畸形 ID）后同一个 Store 仍可用。

use chat_core::core_events::{CoreEvent, MAX_EVENT_BATCH};
use chat_core::send_orchestrator::CancelOutcome;
use chat_core::service::{ConversationCore, error_code};
use chat_protocol::{ConversationId, OperationId, SubmissionId};
use chat_storage::formal::{
    KeyProvenance, PreparedWrite, ProtectedStorageKey, StoreOptions, WriteScope,
    commit_transaction, open_store, persist_prepared_attempt,
};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MASTER_KEY_HEX: &str = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "chat-core-cancel-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture directory");
        Self { dir }
    }

    fn database(&self) -> String {
        self.dir.join("chat.db").to_string_lossy().into_owned()
    }

    fn open(&self) -> ConversationCore {
        ConversationCore::open(&self.database(), MASTER_KEY_HEX).expect("open core")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn conversation(seed: u8) -> String {
    ConversationId::from_bytes([seed; 32]).encode()
}

fn operation(seed: u8) -> String {
    OperationId::from_bytes([seed; 32]).encode()
}

fn operation_numbered(index: u32) -> String {
    let mut bytes = [0u8; 32];
    bytes[..4].copy_from_slice(&index.to_be_bytes());
    OperationId::from_bytes(bytes).encode()
}

fn decode_hex(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().as_chunks::<2>().0 {
        let high = (pair[0] as char).to_digit(16).expect("hex");
        let low = (pair[1] as char).to_digit(16).expect("hex");
        bytes.push(u8::try_from((high << 4) | low).expect("byte"));
    }
    bytes
}

fn master_key() -> ProtectedStorageKey {
    ProtectedStorageKey::try_from_slice(&decode_hex(MASTER_KEY_HEX)).expect("master key")
}

/// 直接用正式存储写入一条已提交尝试，并把状态改成 `state`（accepted / unknown）。
///
/// 调用前必须已关闭 `ConversationCore`（单写者独占句柄）。
fn seed_attempt_state(fixture: &Fixture, draft_id: &str, state: &str) -> String {
    let mut store = open_store(
        StoreOptions::new(fixture.database(), KeyProvenance::Raw32),
        master_key(),
    )
    .expect("open raw formal store");
    let draft = store
        .read_draft(draft_id)
        .expect("read draft")
        .expect("draft exists");
    let submission_id = SubmissionId::from_bytes([0x5a; 32]).encode();
    let mut transaction = store
        .begin_transaction(WriteScope::attempt(&draft.conversation_id))
        .expect("begin attempt transaction");
    persist_prepared_attempt(
        &mut transaction,
        PreparedWrite {
            draft_id: draft_id.to_owned(),
            conversation_id: draft.conversation_id.clone(),
            submission_id: submission_id.clone(),
            mls_epoch: 1,
            immutable_ciphertext: vec![7u8; 32],
            request_hash: draft.request_hash,
            crypto_changes: Vec::new(),
        },
    )
    .expect("persist prepared attempt");
    commit_transaction(transaction).expect("commit attempt");
    store
        .connection()
        .execute(
            "UPDATE message_attempts SET state = ?1 WHERE submission_id = ?2",
            (state, submission_id.as_str()),
        )
        .expect("set attempt state");
    submission_id
}

#[test]
fn cancel_queued_draft_requests_and_publishes_events() {
    let fixture = Fixture::new();
    let mut core = fixture.open();
    let draft = core
        .create_draft(&conversation(1), &operation(1), "机密正文 body-secret")
        .expect("create draft");

    let outcome = core.cancel_draft(&draft.draft_id).expect("cancel");
    match outcome {
        CancelOutcome::CancelRequested {
            draft_id,
            submission_id,
        } => {
            assert_eq!(draft_id, draft.draft_id);
            assert!(submission_id.is_none(), "no attempt yet");
        }
        other => panic!("expected CancelRequested, got {other:?}"),
    }
    let status = core
        .get_draft_status(&draft.draft_id)
        .expect("status")
        .expect("present");
    assert_eq!(status.status, "cancelled");
    assert!(!status.can_cancel, "already cancelled");
    assert!(!status.can_retry, "cancelled drafts cannot be sent");

    let batch = core.poll_events(0, MAX_EVENT_BATCH).expect("poll");
    assert!(!batch.resync_required);
    assert_eq!(batch.events.len(), 2);
    assert_eq!(batch.next_seq, 2);
    assert!(matches!(
        &batch.events[0],
        CoreEvent::DraftChanged { status, .. } if status == "cancelled"
    ));
    assert!(matches!(
        &batch.events[1],
        CoreEvent::ConversationChanged { .. }
    ));
    // 事件不含正文。
    for event in &batch.events {
        assert!(
            !format!("{event:?}").contains("body-secret"),
            "event must not carry the body: {event:?}"
        );
    }
    // 只读：同参数重复 poll 幂等。
    assert_eq!(core.poll_events(0, MAX_EVENT_BATCH).expect("poll"), batch);
    // 用返回游标继续读：没有新事件。
    let empty = core
        .poll_events(batch.next_seq, MAX_EVENT_BATCH)
        .expect("poll after cursor");
    assert!(empty.events.is_empty());
    assert_eq!(empty.next_seq, batch.next_seq);
    assert!(!empty.resync_required);
}

#[test]
fn cancel_accepted_draft_is_too_late() {
    let fixture = Fixture::new();
    let draft_id = {
        let mut core = fixture.open();
        core.create_draft(&conversation(2), &operation(2), "已接受正文")
            .expect("create draft")
            .draft_id
    };
    let seeded = seed_attempt_state(&fixture, &draft_id, "accepted");
    let mut core = fixture.open();
    match core.cancel_draft(&draft_id).expect("cancel") {
        CancelOutcome::CancelTooLate {
            draft_id: returned,
            submission_id: returned_submission,
        } => {
            assert_eq!(returned, draft_id);
            assert_eq!(returned_submission, seeded);
        }
        other => panic!("expected CancelTooLate, got {other:?}"),
    }
    // 已接受：状态保持 accepted，不会被取消。
    let status = core
        .get_draft_status(&draft_id)
        .expect("status")
        .expect("present");
    assert_eq!(status.status, "accepted");
    assert!(!status.can_cancel);
}

#[test]
fn cancel_unknown_draft_reports_unknown() {
    let fixture = Fixture::new();
    let draft_id = {
        let mut core = fixture.open();
        core.create_draft(&conversation(3), &operation(3), "结果未知正文")
            .expect("create draft")
            .draft_id
    };
    let seeded = seed_attempt_state(&fixture, &draft_id, "unknown");
    let mut core = fixture.open();
    match core.cancel_draft(&draft_id).expect("cancel") {
        CancelOutcome::CancelUnknown {
            draft_id: returned,
            submission_id,
        } => {
            assert_eq!(returned, draft_id);
            assert_eq!(submission_id, seeded);
        }
        other => panic!("expected CancelUnknown, got {other:?}"),
    }
}

#[test]
fn cancel_failure_keeps_the_store_usable() {
    let fixture = Fixture::new();
    let mut core = fixture.open();

    // 未知草稿：显式失败，但句柄已经放回。
    let error = core.cancel_draft("draft-missing").expect_err("missing draft");
    assert!(!error_code(&error).is_empty());
    // 畸形 ID：显式 INVALID_INPUT（不进入存储）。
    let error = core.cancel_draft("bad id!").expect_err("malformed draft id");
    assert_eq!(error_code(&error), "INVALID_INPUT");

    // 失败之后同一个 Store 仍然可用。
    let draft = core
        .create_draft(&conversation(4), &operation(4), "失败后仍可写入")
        .expect("create draft after failed cancel");
    assert_eq!(
        core.get_draft_status(&draft.draft_id)
            .expect("status")
            .expect("present")
            .status,
        "queued"
    );
    assert!(matches!(
        core.cancel_draft(&draft.draft_id).expect("cancel works"),
        CancelOutcome::CancelRequested { .. }
    ));
    // 事件里应包含这次成功取消的状态变化。
    let batch = core.poll_events(0, MAX_EVENT_BATCH).expect("poll");
    assert!(batch.events.iter().any(|event| matches!(
        event,
        CoreEvent::DraftChanged { status, .. } if status == "cancelled"
    )));
}

#[test]
fn event_bus_overflow_requires_resync() {
    let fixture = Fixture::new();
    let mut core = fixture.open();
    // 每次取消产生 2 条事件；容量 256 → 129 次取消（258 条）必然溢出。
    for index in 0..129u32 {
        let draft = core
            .create_draft(&conversation(5), &operation_numbered(index), "溢出测试")
            .expect("create draft");
        core.cancel_draft(&draft.draft_id)
            .expect("cancel for overflow");
    }
    let batch = core.poll_events(0, MAX_EVENT_BATCH).expect("poll");
    assert!(batch.resync_required, "cursor older than the buffer");
    assert_eq!(batch.events.len(), 1);
    assert!(matches!(
        &batch.events[0],
        CoreEvent::ResyncRequired { .. }
    ));
    // resync 批次幂等。
    assert_eq!(core.poll_events(0, MAX_EVENT_BATCH).expect("poll"), batch);
    // 用最新游标可以继续增量读取（resync 不是永久失败）。
    let up_to_date = core
        .poll_events(batch.next_seq, MAX_EVENT_BATCH)
        .expect("poll latest");
    assert!(!up_to_date.resync_required);
    assert!(up_to_date.events.is_empty());

    // 游标超前同样要求 resync（绝不静默返回空批）。
    let ahead = core
        .poll_events(batch.next_seq + 10, MAX_EVENT_BATCH)
        .expect("poll ahead");
    assert!(ahead.resync_required);
}

#[test]
fn poll_events_rejects_invalid_limit() {
    let fixture = Fixture::new();
    let core = fixture.open();
    assert_eq!(
        error_code(&core.poll_events(0, 0).expect_err("limit 0")),
        "INVALID_EVENT_LIMIT"
    );
    assert_eq!(
        error_code(
            &core
                .poll_events(0, MAX_EVENT_BATCH + 1)
                .expect_err("limit too large")
        ),
        "INVALID_EVENT_LIMIT"
    );
}

#[test]
fn close_publishes_core_closed_exactly_once() {
    let fixture = Fixture::new();
    let core = fixture.open();
    core.close();
    let batch = core.poll_events(0, MAX_EVENT_BATCH).expect("poll");
    assert_eq!(batch.events, vec![CoreEvent::CoreClosed]);
    // 重复 close 幂等：不会重复发布。
    core.close();
    let again = core.poll_events(0, MAX_EVENT_BATCH).expect("poll");
    assert_eq!(again.events, vec![CoreEvent::CoreClosed]);
    // 关闭后事件仍可读（事件总线不依赖存储句柄）。
    assert!(!core.is_open());
}
