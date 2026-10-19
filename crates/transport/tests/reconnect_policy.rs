//! TRANSPORT-009 策略层验收：确定性抖动、退避序列与封顶、越界拒绝、预算耗尽。
//!
//! 全程纯逻辑：不联网、不 sleep、不读系统时间。

use chat_transport::reconnect::{
    RECONNECT_BUDGET_EXHAUSTED, RECONNECT_HARD_MAX_DELAY_MS, ReconnectAttempts, ReconnectState,
    RetryPolicy,
};

fn policy(jitter_permille: u32) -> RetryPolicy {
    RetryPolicy {
        max_attempts: 5,
        base_delay_ms: 1_000,
        max_delay_ms: 10_000,
        jitter_permille,
    }
}

#[test]
fn jitter_is_deterministic_and_stays_within_the_permille_band() {
    let base = policy(300);
    base.validate().expect("policy");
    for attempt in 1..=6u32 {
        let cap = base.jitter_cap_for(attempt);
        let jitter = base.jitter_for(attempt);
        assert!(
            jitter <= cap,
            "attempt {attempt}: jitter {jitter} must not exceed cap {cap}"
        );
        // 抖动上限 = base × permille / 1000。
        let expected_cap = base.base_delay_for(attempt) * 300u64 / 1_000;
        assert_eq!(cap, expected_cap);
        // 确定性：同策略同 attempt 反复取值必须相同（不读系统时间）。
        assert_eq!(jitter, base.jitter_for(attempt));
        assert_eq!(jitter, policy(300).jitter_for(attempt));
        // 最终延迟落在 [base, base + cap] 内且不超过封顶。
        let delay = base.delay_for(attempt);
        let floor = base.base_delay_for(attempt);
        assert!(delay >= floor, "delay {delay} must include the base backoff");
        assert!(delay <= floor + cap);
        assert!(delay <= base.max_delay_ms);
    }
    // permille = 0 → 没有抖动。
    let no_jitter = policy(0);
    for attempt in 1..=4u32 {
        assert_eq!(no_jitter.jitter_for(attempt), 0);
        assert_eq!(no_jitter.delay_for(attempt), no_jitter.base_delay_for(attempt));
    }
    // permille = 1000（上限）也仍在合法区间内。
    let full = policy(1_000);
    full.validate().expect("permille 1000 is allowed");
    for attempt in 1..=4u32 {
        assert!(full.jitter_for(attempt) <= full.jitter_cap_for(attempt));
    }
}

#[test]
fn backoff_doubles_and_caps_at_max_delay() {
    let policy = RetryPolicy {
        max_attempts: 10,
        base_delay_ms: 1_000,
        max_delay_ms: 10_000,
        jitter_permille: 0,
    };
    policy.validate().expect("policy");
    assert_eq!(policy.base_delay_for(1), 1_000);
    assert_eq!(policy.base_delay_for(2), 2_000);
    assert_eq!(policy.base_delay_for(3), 4_000);
    assert_eq!(policy.base_delay_for(4), 8_000);
    // 第 5 次会到 16_000，被 max_delay_ms 封顶。
    assert_eq!(policy.base_delay_for(5), 10_000);
    assert_eq!(policy.base_delay_for(50), 10_000);
    // 极端 attempt 不溢出（饱和到封顶）。
    assert_eq!(policy.base_delay_for(u32::MAX), 10_000);
    // 延迟上限受协议 5 分钟硬约束。
    let too_large = RetryPolicy {
        max_delay_ms: RECONNECT_HARD_MAX_DELAY_MS + 1,
        ..policy
    };
    assert_eq!(
        too_large.validate().expect_err("too large").code(),
        "INVALID_RECONNECT_POLICY"
    );
}

#[test]
fn out_of_range_permille_and_attempts_are_rejected() {
    let bad_permille = RetryPolicy {
        jitter_permille: 1_001,
        ..policy(200)
    };
    assert_eq!(
        bad_permille.validate().expect_err("permille").code(),
        "INVALID_RECONNECT_POLICY"
    );
    assert!(ReconnectAttempts::new(bad_permille).is_err());
    let zero_attempts = RetryPolicy {
        max_attempts: 0,
        ..policy(200)
    };
    assert_eq!(
        zero_attempts.validate().expect_err("attempts").code(),
        "INVALID_RECONNECT_POLICY"
    );
    let zero_base = RetryPolicy {
        base_delay_ms: 0,
        ..policy(200)
    };
    assert!(zero_base.validate().is_err());
    let cap_below_base = RetryPolicy {
        base_delay_ms: 2_000,
        max_delay_ms: 1_000,
        ..policy(200)
    };
    assert!(cap_below_base.validate().is_err());
}

#[test]
fn exhaustion_is_explicit_and_success_resets_the_budget() {
    let policy = RetryPolicy {
        max_attempts: 3,
        base_delay_ms: 100,
        max_delay_ms: 1_000,
        jitter_permille: 0,
    };
    let mut attempts = ReconnectAttempts::new(policy).expect("policy");
    assert_eq!(attempts.state(), ReconnectState::INITIAL);
    // 断线只排期，不消耗预算。
    let state = attempts.on_disconnected("PEER_UNREACHABLE", 1_000);
    assert_eq!(state.attempts, 0);
    assert_eq!(state.last_error_code, Some("PEER_UNREACHABLE"));
    assert!(state.next_attempt_ms > 1_000, "a retry is scheduled");
    assert!(!attempts.ready_at(1_000), "not ready before the scheduled time");
    assert!(attempts.ready_at(state.next_attempt_ms));
    // 三次预算内成功。
    for attempt in 1..=3u32 {
        let schedule = attempts
            .next_attempt(u64::from(attempt) * 10_000)
            .expect("within budget");
        assert_eq!(schedule.attempt, attempt);
        assert_eq!(schedule.delay_ms, 100 << (attempt - 1));
    }
    // 第四次显式失败，状态带稳定码与下次尝试时间。
    let error = attempts.next_attempt(40_000).expect_err("budget exhausted");
    assert_eq!(error.code(), RECONNECT_BUDGET_EXHAUSTED);
    let state = attempts.state();
    assert_eq!(state.attempts, 3, "the rejected attempt does not consume budget");
    assert_eq!(state.last_error_code, Some(RECONNECT_BUDGET_EXHAUSTED));
    assert!(state.next_attempt_ms > 40_000);
    // 成功重连后预算复位。
    attempts.record_success();
    assert_eq!(attempts.state(), ReconnectState::INITIAL);
    assert_eq!(attempts.next_attempt(50_000).expect("restart").attempt, 1);
}

