//! SYNC-011 重试/退避策略测试。
//!
//! 覆盖：指数退避序列与上限、抖动范围（确定性）、Retry-After 优先与 5 分钟上限、
//! 不可重试错误立即终态、未知结果只对账、预算耗尽、连续失败计数与唯一重置条件、
//! 畸形策略/输入的有界拒绝。

use chat_domain::OperationState;
use chat_protocol::ErrorCode;
use chat_sync::retry::{
    FixedJitter, JitterSource, RetryAction, RetryInput, RetryLedger, RetryPolicy, SeededJitter,
    apply_retry_after, jittered_delay, schedule_retry,
};

const NOW: i64 = 1_700_000_000_000;

fn input(code: ErrorCode, state: OperationState) -> RetryInput {
    RetryInput {
        now_unix_ms: NOW,
        code,
        state,
        retry_after_ms: None,
    }
}

fn resend_input() -> RetryInput {
    input(ErrorCode::NetworkUnavailable, OperationState::Prepared)
}

#[test]
fn backoff_is_exponential_and_capped() {
    let policy = RetryPolicy::DEFAULT;
    let mut jitter = FixedJitter::new(500); // 中点：无抖动偏移
    let expected = [1_000u64, 2_000, 4_000, 8_000, 16_000, 32_000, 60_000, 60_000];
    for (index, want) in expected.iter().enumerate() {
        let attempt = u32::try_from(index).unwrap() + 1;
        let schedule =
            schedule_retry(policy, attempt, resend_input(), &mut jitter).expect("schedule");
        assert_eq!(
            schedule.action,
            RetryAction::RetrySameBytes,
            "attempt {attempt}"
        );
        assert_eq!(schedule.delay_ms, Some(*want), "attempt {attempt}");
        assert_eq!(
            schedule.next_attempt_at_ms,
            Some(NOW + i64::try_from(*want).unwrap())
        );
        assert_eq!(schedule.code, ErrorCode::NetworkUnavailable);
    }
}

#[test]
fn jitter_window_is_fixed_and_bounded() {
    // 窗口 = base ±20%：抽样 0/500/1000 对应下界/中点/上界。
    assert_eq!(jittered_delay(1_000, 20, 0), 800);
    assert_eq!(jittered_delay(1_000, 20, 500), 1_000);
    assert_eq!(jittered_delay(1_000, 20, 1_000), 1_200);
    // 抽样越界被夹到 1000；抖动 0% 表示不抖动。
    assert_eq!(jittered_delay(1_000, 20, 5_000), 1_200);
    assert_eq!(jittered_delay(1_000, 0, 0), 1_000);
    // 100% 抖动时下界为 0，不会出现负数。
    assert_eq!(jittered_delay(1_000, 100, 0), 0);
    assert_eq!(jittered_delay(1_000, 100, 1_000), 2_000);

    let policy = RetryPolicy {
        jitter_percent: 20,
        ..RetryPolicy::DEFAULT
    };
    let mut low = FixedJitter::new(0);
    let mut high = FixedJitter::new(1_000);
    let low_delay = schedule_retry(policy, 1, resend_input(), &mut low)
        .unwrap()
        .delay_ms;
    let high_delay = schedule_retry(policy, 1, resend_input(), &mut high)
        .unwrap()
        .delay_ms;
    assert_eq!(low_delay, Some(800));
    assert_eq!(high_delay, Some(1_200));
}

#[test]
fn retry_after_takes_priority_and_is_capped_at_five_minutes() {
    let policy = RetryPolicy::DEFAULT;
    let mut jitter = FixedJitter::new(500);
    let mut with_retry_after = resend_input();
    with_retry_after.retry_after_ms = Some(30_000);
    let schedule = schedule_retry(policy, 1, with_retry_after, &mut jitter).unwrap();
    assert_eq!(
        schedule.delay_ms,
        Some(30_000),
        "server hint must beat the 1s backoff"
    );

    let mut huge = resend_input();
    huge.retry_after_ms = Some(u64::MAX);
    assert_eq!(
        schedule_retry(policy, 1, huge, &mut jitter).unwrap().delay_ms,
        Some(chat_protocol::MAX_RETRY_AFTER_MS)
    );
    // 0 被夹到 1ms，不会让退避变成“立即重试”。
    let mut zero = resend_input();
    zero.retry_after_ms = Some(0);
    assert_eq!(apply_retry_after(1_000, Some(0)), 1_000);
    assert_eq!(
        schedule_retry(policy, 1, zero, &mut jitter).unwrap().delay_ms,
        Some(1_000)
    );
}

#[test]
fn non_retryable_errors_are_terminal_without_a_delay() {
    let policy = RetryPolicy::DEFAULT;
    let mut jitter = FixedJitter::new(500);
    let cases = [
        (
            ErrorCode::PermissionDenied,
            RetryAction::WaitForUser,
            "muted or not authorized",
        ),
        (ErrorCode::DeviceRevoked, RetryAction::WaitForUser, "revoked"),
        (
            ErrorCode::StateCorrupted,
            RetryAction::PermanentFailure,
            "corrupt state",
        ),
        (
            ErrorCode::InvalidCredential,
            RetryAction::PermanentFailure,
            "invalid credential",
        ),
        (
            ErrorCode::InvalidCandidate,
            RetryAction::PermanentFailure,
            "candidate rejected",
        ),
        (
            ErrorCode::StaleEpoch,
            RetryAction::SyncThenRebuild,
            "needs sync then rebuild",
        ),
    ];
    for (code, want_action, reason) in cases {
        let schedule = schedule_retry(
            policy,
            1,
            input(code, OperationState::Prepared),
            &mut jitter,
        )
        .unwrap();
        assert_eq!(schedule.action, want_action, "{code:?}: {reason}");
        assert_eq!(schedule.delay_ms, None, "{code:?} must not schedule a retry");
        assert_eq!(schedule.next_attempt_at_ms, None);
        assert_eq!(schedule.code, code, "root cause code is preserved");
    }
}

#[test]
fn unknown_results_only_reconcile_the_existing_submission() {
    let policy = RetryPolicy::DEFAULT;
    let mut jitter = FixedJitter::new(500);
    for state in [OperationState::Prepared, OperationState::InFlight] {
        let schedule = schedule_retry(
            policy,
            1,
            input(ErrorCode::UnknownResult, state),
            &mut jitter,
        )
        .unwrap();
        assert_eq!(schedule.action, RetryAction::QueryExisting);
        assert!(!schedule.action.retries_automatically());
        assert!(
            schedule.delay_ms.is_some(),
            "reconciliation polling still needs a bounded delay"
        );
    }
    // 已接受/已失败的操作不再重试。
    for state in [OperationState::Accepted, OperationState::Failed] {
        let schedule = schedule_retry(
            policy,
            1,
            input(ErrorCode::NetworkUnavailable, state),
            &mut jitter,
        )
        .unwrap();
        assert_eq!(schedule.action, RetryAction::PermanentFailure);
        assert_eq!(schedule.delay_ms, None);
    }
}

#[test]
fn retry_budget_exhaustion_stops_automatic_retries() {
    let policy = RetryPolicy::DEFAULT;
    let mut jitter = FixedJitter::new(500);
    let allowed = schedule_retry(policy, policy.max_attempts, resend_input(), &mut jitter).unwrap();
    assert_eq!(allowed.action, RetryAction::RetrySameBytes);
    let exhausted =
        schedule_retry(policy, policy.max_attempts + 1, resend_input(), &mut jitter).unwrap();
    assert_eq!(exhausted.action, RetryAction::RetryBudgetExhausted);
    assert!(exhausted.action.is_terminal());
    assert_eq!(exhausted.delay_ms, None);
    assert_eq!(exhausted.code, ErrorCode::NetworkUnavailable);
}

#[test]
fn ledger_counts_failures_and_only_success_resets() {
    let policy = RetryPolicy::DEFAULT;
    let mut jitter = FixedJitter::new(500);
    let mut ledger = RetryLedger::new(policy).expect("valid policy");
    assert_eq!(ledger.consecutive_failures(), 0);
    for expected in 1..=3 {
        let schedule = ledger.record_failure(resend_input(), &mut jitter).unwrap();
        assert_eq!(schedule.attempt, expected);
        assert_eq!(ledger.consecutive_failures(), expected);
        assert_eq!(
            schedule.delay_ms,
            Some(policy.base_delay_for(expected)),
            "jitter midpoint keeps the base delay"
        );
    }
    // 重启：从持久化计数恢复，不归零。
    let mut restored = RetryLedger::restore(policy, ledger.consecutive_failures()).unwrap();
    assert_eq!(restored.consecutive_failures(), 3);
    assert_eq!(
        restored
            .record_failure(resend_input(), &mut jitter)
            .unwrap()
            .attempt,
        4
    );
    // 唯一重置条件：正式成功证据。
    assert_eq!(restored.record_success(), 4);
    assert_eq!(restored.consecutive_failures(), 0);
    assert!(!restored.is_exhausted());

    let mut exhausted = RetryLedger::restore(policy, policy.max_attempts + 1).unwrap();
    assert!(exhausted.is_exhausted());
    assert_eq!(
        exhausted
            .record_failure(resend_input(), &mut jitter)
            .unwrap()
            .action,
        RetryAction::RetryBudgetExhausted
    );
}

#[test]
fn malformed_policies_and_inputs_are_rejected() {
    let mut jitter = FixedJitter::new(500);
    let bad = [
        RetryPolicy {
            base_delay_ms: 0,
            ..RetryPolicy::DEFAULT
        },
        RetryPolicy {
            multiplier: 0,
            ..RetryPolicy::DEFAULT
        },
        RetryPolicy {
            max_delay_ms: 500,
            ..RetryPolicy::DEFAULT
        },
        RetryPolicy {
            jitter_percent: 101,
            ..RetryPolicy::DEFAULT
        },
        RetryPolicy {
            max_attempts: 0,
            ..RetryPolicy::DEFAULT
        },
    ];
    for policy in bad {
        let error = schedule_retry(policy, 1, resend_input(), &mut jitter).unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidTarget, "{policy:?}");
        assert!(RetryLedger::new(policy).is_err());
    }
    assert_eq!(
        schedule_retry(RetryPolicy::DEFAULT, 0, resend_input(), &mut jitter)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidSequence
    );
    let mut negative_clock = resend_input();
    negative_clock.now_unix_ms = -1;
    assert_eq!(
        schedule_retry(RetryPolicy::DEFAULT, 1, negative_clock, &mut jitter)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidTarget
    );
}

#[test]
fn seeded_jitter_is_deterministic_and_stays_in_the_window() {
    let mut first = SeededJitter::new(42);
    let mut second = SeededJitter::new(42);
    for _ in 0..64 {
        let a = first.sample_permille();
        let b = second.sample_permille();
        assert_eq!(a, b, "same seed must produce the same sequence");
        assert!(a <= 1_000);
    }
    let mut jitter = SeededJitter::new(7);
    let policy = RetryPolicy::DEFAULT;
    for attempt in 1..=6 {
        let delay = schedule_retry(policy, attempt, resend_input(), &mut jitter)
            .unwrap()
            .delay_ms
            .unwrap();
        let base = policy.base_delay_for(attempt);
        let window = base * 20 / 100;
        assert!(
            (base - window..=base + window).contains(&delay),
            "attempt {attempt}: {delay} outside [{}, {}]",
            base - window,
            base + window
        );
    }
}
