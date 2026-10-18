//! SYNC-002 拉取循环决策核心的验收用例（正常增量/空洞/乱序/超限/幂等/退避）。
use chat_protocol::ErrorCode;
use chat_sync::mailbox::MAX_MAILBOX_PAGE_ITEMS;
use chat_sync::pull::{
    BackoffReason, LastPull, PullAction, PullInput, PullPolicy, ResyncReason, StopReason,
    mailbox_pull_loop,
};

const NOW: i64 = 1_700_000_000_000;

fn input(local_tail: u64, node_head: u64, last: LastPull) -> PullInput {
    PullInput {
        local_tail,
        node_head,
        last,
        consecutive_failures: 0,
        now_ms: NOW,
        last_pull_at_ms: None,
        next_attempt_at_ms: 0,
    }
}

#[test]
fn incremental_pull_is_bounded_and_advances_one_page_at_a_time() {
    let policy = PullPolicy::DEFAULT;
    assert_eq!(policy.page_items, MAX_MAILBOX_PAGE_ITEMS);

    // 首轮：从 1 开始，最多一页。
    let action = mailbox_pull_loop(policy, input(0, 250, LastPull::NotPolled)).unwrap();
    assert_eq!(action, PullAction::Pull { from: 1, to: 100 });
    assert_eq!(action.pull_len(), Some(100));

    // 应用首页后（本地 100、上一页连续）继续下一页。
    let mut next = input(100, 250, LastPull::Page { from: 1, to: 100, items: 100 });
    next.last_pull_at_ms = Some(NOW - 2_000);
    assert_eq!(
        mailbox_pull_loop(policy, next).unwrap(),
        PullAction::Pull { from: 101, to: 200 }
    );

    // 尾页只拉到节点 head，不越界。
    let mut tail = input(200, 250, LastPull::Page { from: 101, to: 200, items: 100 });
    tail.last_pull_at_ms = Some(NOW - 2_000);
    assert_eq!(
        mailbox_pull_loop(policy, tail).unwrap(),
        PullAction::Pull { from: 201, to: 250 }
    );

    // 已到 head：退避而不是空转。
    let caught_up = input(250, 250, LastPull::Page { from: 201, to: 250, items: 50 });
    match mailbox_pull_loop(policy, caught_up).unwrap() {
        PullAction::Backoff { reason, delay_ms, .. } => {
            assert_eq!(reason, BackoffReason::IdleAtHead);
            assert_eq!(delay_ms as i64, policy.idle_backoff_ms);
        }
        other => panic!("expected idle backoff, got {other:?}"),
    }
}

#[test]
fn planning_is_deterministic_and_replay_is_idempotent() {
    let policy = PullPolicy::DEFAULT;
    let input = input(40, 90, LastPull::NotPolled);
    let first = mailbox_pull_loop(policy, input).unwrap();
    let second = mailbox_pull_loop(policy, input).unwrap();
    assert_eq!(first, second, "same input must yield the same action");
    // 重复计算同一状态不会“推进”范围。
    assert_eq!(first, PullAction::Pull { from: 41, to: 90 });
    assert_eq!(mailbox_pull_loop(policy, input).unwrap(), first);
}

#[test]
fn gap_in_the_last_page_requires_a_resync_instead_of_skipping() {
    let policy = PullPolicy::DEFAULT;
    // 本地 40，上一页却从 42 开始：41 缺失，绝不跳过。
    let action = mailbox_pull_loop(
        policy,
        input(40, 90, LastPull::Page { from: 42, to: 50, items: 9 }),
    )
    .unwrap();
    assert_eq!(
        action,
        PullAction::Resync { reason: ResyncReason::NonContiguousPage }
    );
    // 上一页整体落在本地游标之前（过期页）：同样不能当作进展。
    let action = mailbox_pull_loop(
        policy,
        input(40, 90, LastPull::Page { from: 1, to: 10, items: 10 }),
    )
    .unwrap();
    assert_eq!(
        action,
        PullAction::Resync { reason: ResyncReason::NonContiguousPage }
    );
    assert!(!action.is_pull());
}

#[test]
fn pages_beyond_the_head_or_reversed_are_rejected_explicitly() {
    let policy = PullPolicy::DEFAULT;
    // 页越过节点 head：不能当成正常页。
    let action = mailbox_pull_loop(
        policy,
        input(10, 20, LastPull::Page { from: 11, to: 25, items: 15 }),
    )
    .unwrap();
    assert_eq!(
        action,
        PullAction::Resync { reason: ResyncReason::PageBeyondHead }
    );
    // 页尾小于页首：畸形输入显式失败（不静默修正）。
    let error = mailbox_pull_loop(
        policy,
        input(10, 20, LastPull::Page { from: 15, to: 14, items: 1 }),
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidCursor);
    // 页条数与范围不一致：状态不可信。
    let error = mailbox_pull_loop(
        policy,
        input(10, 20, LastPull::Page { from: 11, to: 15, items: 99 }),
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::StateCorrupted);
}

#[test]
fn empty_page_with_head_ahead_requires_a_resync() {
    let policy = PullPolicy::DEFAULT;
    let action = mailbox_pull_loop(policy, input(10, 20, LastPull::Empty)).unwrap();
    assert_eq!(
        action,
        PullAction::Resync { reason: ResyncReason::EmptyPageWithHeadAhead }
    );
    // 真正到 head 的空页只退避。
    let action = mailbox_pull_loop(policy, input(20, 20, LastPull::Empty)).unwrap();
    assert!(matches!(
        action,
        PullAction::Backoff { reason: BackoffReason::IdleAtHead, .. }
    ));
}

#[test]
fn oversized_pages_and_gaps_are_refused_instead_of_truncated() {
    let policy = PullPolicy::DEFAULT;
    // 数据源给了超大一页：显式失败。
    let error = mailbox_pull_loop(
        policy,
        input(
            0,
            1_000,
            LastPull::Page { from: 1, to: 600, items: 600 },
        ),
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ContentTooLarge);

    // 落后超过增量上限：交给追赶计划，不一次拉超大范围。
    let far_behind = input(0, policy.max_incremental_gap + 1, LastPull::NotPolled);
    assert_eq!(
        mailbox_pull_loop(policy, far_behind).unwrap(),
        PullAction::Resync { reason: ResyncReason::GapTooLarge }
    );
    // 恰好等于上限时仍可增量拉取（边界不误判）。
    let at_limit = input(0, policy.max_incremental_gap, LastPull::NotPolled);
    assert_eq!(
        mailbox_pull_loop(policy, at_limit).unwrap(),
        PullAction::Pull { from: 1, to: policy.page_items as u64 }
    );

    // 本地越过节点 head（节点回退）：必须重新对齐。
    assert_eq!(
        mailbox_pull_loop(policy, input(50, 40, LastPull::NotPolled)).unwrap(),
        PullAction::Resync { reason: ResyncReason::LocalAheadOfNode }
    );
}

#[test]
fn failure_backoff_is_exponential_capped_and_budgeted() {
    let policy = PullPolicy::DEFAULT;
    let mut failures_input = input(10, 20, LastPull::Failed { retry_after_ms: None });
    let expected = [1_000_u64, 2_000, 4_000, 8_000, 16_000, 32_000, 60_000, 60_000];
    for (index, expected_delay) in expected.iter().enumerate() {
        failures_input.consecutive_failures = (index as u32) + 1;
        match mailbox_pull_loop(policy, failures_input).unwrap() {
            PullAction::Backoff { delay_ms, reason, next_attempt_at_ms } => {
                assert_eq!(reason, BackoffReason::Failure);
                assert_eq!(delay_ms, *expected_delay);
                assert_eq!(next_attempt_at_ms, NOW + *expected_delay as i64);
            }
            other => panic!("expected failure backoff, got {other:?}"),
        }
    }

    // Retry-After 是下界，但不超过 5 分钟。
    let mut retry_after = input(10, 20, LastPull::Failed { retry_after_ms: Some(300_000) });
    retry_after.consecutive_failures = 1;
    match mailbox_pull_loop(policy, retry_after).unwrap() {
        PullAction::Backoff { delay_ms, .. } => assert_eq!(delay_ms, 300_000),
        other => panic!("expected retry-after backoff, got {other:?}"),
    }
    let mut huge_retry_after = input(10, 20, LastPull::Failed { retry_after_ms: Some(u64::MAX) });
    huge_retry_after.consecutive_failures = 1;
    match mailbox_pull_loop(policy, huge_retry_after).unwrap() {
        PullAction::Backoff { delay_ms, .. } => {
            assert_eq!(delay_ms, chat_protocol::MAX_RETRY_AFTER_MS)
        }
        other => panic!("expected clamped retry-after, got {other:?}"),
    }

    // 超过失败预算：停止自动重试。
    let mut exhausted = input(10, 20, LastPull::Failed { retry_after_ms: None });
    exhausted.consecutive_failures = policy.failure_budget + 1;
    assert_eq!(
        mailbox_pull_loop(policy, exhausted).unwrap(),
        PullAction::Stop { reason: StopReason::FailureBudgetExhausted }
    );
}

#[test]
fn not_due_and_page_interval_avoid_busy_looping() {
    let policy = PullPolicy::DEFAULT;
    // 未到 next_attempt：不调用数据源。
    let mut waiting = input(10, 20, LastPull::NotPolled);
    waiting.next_attempt_at_ms = NOW + 5_000;
    match mailbox_pull_loop(policy, waiting).unwrap() {
        PullAction::Backoff { delay_ms, reason, next_attempt_at_ms } => {
            assert_eq!(reason, BackoffReason::NotDue);
            assert_eq!(delay_ms, 5_000);
            assert_eq!(next_attempt_at_ms, NOW + 5_000);
        }
        other => panic!("expected not-due backoff, got {other:?}"),
    }
    // 非空页之后未满页间隔：不立刻再拉。
    let mut just_polled = input(10, 20, LastPull::Page { from: 1, to: 10, items: 10 });
    just_polled.last_pull_at_ms = Some(NOW - 100);
    match mailbox_pull_loop(policy, just_polled).unwrap() {
        PullAction::Backoff { delay_ms, reason, .. } => {
            assert_eq!(reason, BackoffReason::PageInterval);
            assert_eq!(delay_ms as i64, policy.page_interval_ms - 100);
        }
        other => panic!("expected page-interval backoff, got {other:?}"),
    }
    // 间隔已满：继续拉取。
    let mut due = input(10, 20, LastPull::Page { from: 1, to: 10, items: 10 });
    due.last_pull_at_ms = Some(NOW - policy.page_interval_ms);
    assert_eq!(
        mailbox_pull_loop(policy, due).unwrap(),
        PullAction::Pull { from: 11, to: 20 }
    );
}

#[test]
fn malformed_policy_and_clock_inputs_are_rejected() {
    let mut policy = PullPolicy::DEFAULT;
    policy.page_items = 0;
    assert_eq!(
        mailbox_pull_loop(policy, input(0, 1, LastPull::NotPolled))
            .unwrap_err()
            .code(),
        ErrorCode::InvalidTarget
    );

    let policy = PullPolicy::DEFAULT;
    let mut negative_clock = input(0, 1, LastPull::NotPolled);
    negative_clock.now_ms = -1;
    assert_eq!(
        mailbox_pull_loop(policy, negative_clock).unwrap_err().code(),
        ErrorCode::InvalidTarget
    );
    let mut negative_next = input(0, 1, LastPull::NotPolled);
    negative_next.next_attempt_at_ms = -1;
    assert_eq!(
        mailbox_pull_loop(policy, negative_next).unwrap_err().code(),
        ErrorCode::InvalidTarget
    );
    let mut backwards_clock = input(0, 1, LastPull::NotPolled);
    backwards_clock.last_pull_at_ms = Some(NOW + 1);
    assert_eq!(
        mailbox_pull_loop(policy, backwards_clock).unwrap_err().code(),
        ErrorCode::InvalidTarget
    );
    let mut uncounted_failure = input(0, 1, LastPull::Failed { retry_after_ms: None });
    uncounted_failure.consecutive_failures = 0;
    assert_eq!(
        mailbox_pull_loop(policy, uncounted_failure).unwrap_err().code(),
        ErrorCode::StateCorrupted
    );
    let mut huge_sequence = input(0, 0, LastPull::NotPolled);
    huge_sequence.node_head = chat_protocol::MAX_CURSOR_SEQ + 1;
    assert_eq!(
        mailbox_pull_loop(policy, huge_sequence).unwrap_err().code(),
        ErrorCode::InvalidTarget
    );
}