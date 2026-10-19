//! TRANSPORT-009：连接恢复策略（纯逻辑，**不依赖会话对象**）。
//!
//! 本模块只做两件事：
//!
//! 1. **重试策略**：指数退避 `min(base × 2^(n-1), max)` + **确定性抖动**——抖动上限是
//!    `base × jitter_permille / 1000`，实际取值用 splitmix64 按 `attempt` 取模，
//!    **不读系统时间**，因此同一策略 + 同一 attempt 永远得到同一延迟（测试可精确断言）；
//! 2. **重连尝试状态机**：记录 `attempts` / `last_error_code` / `next_attempt_ms`，
//!    在预算内给出下一次尝试的延迟，超过 `max_attempts` 时返回显式错误码
//!    `RECONNECT_BUDGET_EXHAUSTED`（绝不无限重试、绝不静默成功）。
//!
//! 设计约束：不持有连接、不做网络 I/O、不 sleep；真实拨号由宿主完成，本层只给调度结论。

/// 重连次数硬上限。
pub const RECONNECT_HARD_MAX_ATTEMPTS: u32 = 100;
/// 延迟上限（毫秒）：与协议 5 分钟上限一致。
pub const RECONNECT_HARD_MAX_DELAY_MS: u64 = 300_000;
/// 抖动比例的千分比上限。
pub const RECONNECT_MAX_JITTER_PERMILLE: u32 = 1_000;
/// 预算耗尽时的稳定错误码。
pub const RECONNECT_BUDGET_EXHAUSTED: &str = "RECONNECT_BUDGET_EXHAUSTED";

/// 重连策略（确定性抖动）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// 连续失败预算（≥1；失败次数超过它即显式失败）。
    pub max_attempts: u32,
    /// 首次失败的基础延迟（毫秒，≥1）。
    pub base_delay_ms: u64,
    /// 延迟上限（毫秒，≥ base 且 ≤ 300000）。
    pub max_delay_ms: u64,
    /// 抖动比例（千分比，0..=1000）：抖动上限 = base × permille / 1000。
    pub jitter_permille: u32,
}

impl RetryPolicy {
    /// 默认策略：500ms 起、30s 封顶、5 次预算、200‰ 抖动。
    pub const DEFAULT: RetryPolicy = RetryPolicy {
        max_attempts: 5,
        base_delay_ms: 500,
        max_delay_ms: 30_000,
        jitter_permille: 200,
    };

    /// 有界校验；非法策略显式拒绝（不夹取、不静默回退）。
    pub fn validate(&self) -> Result<(), ReconnectError> {
        if self.max_attempts == 0 || self.max_attempts > RECONNECT_HARD_MAX_ATTEMPTS {
            return Err(ReconnectError::InvalidPolicy {
                detail: "maxAttempts must be within 1..=100",
            });
        }
        if self.base_delay_ms == 0 || self.base_delay_ms > RECONNECT_HARD_MAX_DELAY_MS {
            return Err(ReconnectError::InvalidPolicy {
                detail: "baseDelayMs must be within 1..=300000",
            });
        }
        if self.max_delay_ms < self.base_delay_ms || self.max_delay_ms > RECONNECT_HARD_MAX_DELAY_MS
        {
            return Err(ReconnectError::InvalidPolicy {
                detail: "maxDelayMs must be within baseDelayMs..=300000",
            });
        }
        if self.jitter_permille > RECONNECT_MAX_JITTER_PERMILLE {
            return Err(ReconnectError::InvalidPolicy {
                detail: "jitterPermille must be within 0..=1000",
            });
        }
        Ok(())
    }

    /// 第 `attempt` 次失败（从 1 开始）的指数基础延迟，封顶 `max_delay_ms`。
    pub fn base_delay_for(&self, attempt: u32) -> u64 {
        let exponent = attempt.saturating_sub(1);
        let mut delay = self.base_delay_ms;
        let mut step = 0u32;
        while step < exponent {
            delay = match delay.checked_mul(2) {
                Some(next) => next,
                None => return self.max_delay_ms,
            };
            if delay >= self.max_delay_ms {
                return self.max_delay_ms;
            }
            step += 1;
        }
        delay.min(self.max_delay_ms)
    }

    /// 第 `attempt` 次失败的抖动上限（毫秒）= `base × permille / 1000`。
    pub fn jitter_cap_for(&self, attempt: u32) -> u64 {
        let base = self.base_delay_for(attempt.max(1));
        base.saturating_mul(u64::from(self.jitter_permille)) / 1_000
    }

    /// 第 `attempt` 次失败的抖动（毫秒，确定性；不读系统时间）。
    pub fn jitter_for(&self, attempt: u32) -> u64 {
        let cap = self.jitter_cap_for(attempt.max(1));
        if cap == 0 {
            return 0;
        }
        let mixed = mix64(u64::from(attempt.max(1)));
        mixed % (cap.saturating_add(1))
    }

    /// 第 `attempt` 次失败的最终延迟：基础 + 抖动，仍封顶 `max_delay_ms`。
    pub fn delay_for(&self, attempt: u32) -> u64 {
        let attempt = attempt.max(1);
        self.base_delay_for(attempt)
            .saturating_add(self.jitter_for(attempt))
            .min(self.max_delay_ms)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// splitmix64 混淆：只用于调度抖动，**不是**安全随机，不用于密钥/令牌。
const fn mix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

/// 重连策略/调度的稳定错误。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconnectError {
    /// 策略参数越界（配置错误，不夹取）。
    InvalidPolicy { detail: &'static str },
    /// 重连预算耗尽（显式失败，等待用户/上层处置）。
    BudgetExhausted { attempts: u32 },
}

impl ReconnectError {
    /// 稳定错误码文本。
    pub const fn code(&self) -> &'static str {
        match self {
            ReconnectError::InvalidPolicy { .. } => "INVALID_RECONNECT_POLICY",
            ReconnectError::BudgetExhausted { .. } => RECONNECT_BUDGET_EXHAUSTED,
        }
    }
}

impl core::fmt::Display for ReconnectError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ReconnectError::InvalidPolicy { detail } => {
                write!(formatter, "invalid reconnect policy: {detail}")
            }
            ReconnectError::BudgetExhausted { attempts } => {
                write!(formatter, "reconnect budget exhausted after {attempts} attempts")
            }
        }
    }
}

impl std::error::Error for ReconnectError {}

/// 重连调度状态快照（供界面/诊断轮询）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReconnectState {
    /// 已消耗的重连尝试次数（成功重连后清零）。
    pub attempts: u32,
    /// 最近一次断线/失败的稳定错误码。
    pub last_error_code: Option<&'static str>,
    /// 下一次允许尝试的时间（Unix 毫秒；0 表示立刻可试）。
    pub next_attempt_ms: u64,
}

impl ReconnectState {
    /// 初始状态：没有尝试、没有错误、立即可试。
    pub const INITIAL: ReconnectState = ReconnectState {
        attempts: 0,
        last_error_code: None,
        next_attempt_ms: 0,
    };
}

/// 一次重连的调度结论。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReconnectSchedule {
    /// 本次是第几次尝试（从 1 开始）。
    pub attempt: u32,
    /// 需要等待的延迟（毫秒）。
    pub delay_ms: u64,
    /// 下一次允许尝试的时间（Unix 毫秒）。
    pub next_attempt_ms: u64,
}

/// 重连尝试状态机：只记计数与调度，不持有连接、不 sleep。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReconnectAttempts {
    policy: RetryPolicy,
    state: ReconnectState,
}

impl ReconnectAttempts {
    /// 以策略创建（越界策略显式拒绝）。
    pub fn new(policy: RetryPolicy) -> Result<Self, ReconnectError> {
        policy.validate()?;
        Ok(Self {
            policy,
            state: ReconnectState::INITIAL,
        })
    }

    /// 当前策略。
    pub const fn policy(&self) -> RetryPolicy {
        self.policy
    }

    /// 当前状态快照。
    pub const fn state(&self) -> ReconnectState {
        self.state
    }

    /// 断线：记录错误码并按「下一次尝试」算好退避时间（不消耗预算）。
    pub fn on_disconnected(&mut self, code: &'static str, now_ms: u64) -> ReconnectState {
        let attempt = self.state.attempts.saturating_add(1);
        let delay = self.policy.delay_for(attempt);
        self.state = ReconnectState {
            attempts: self.state.attempts,
            last_error_code: Some(code),
            next_attempt_ms: now_ms.saturating_add(delay),
        };
        self.state
    }

    /// 现在是否可以尝试重连（`now_ms` 已到达计划时间）。
    pub fn ready_at(&self, now_ms: u64) -> bool {
        now_ms >= self.state.next_attempt_ms
    }

    /// 申请一次重连：预算内返回调度（消耗一次尝试），超预算返回显式错误码。
    pub fn next_attempt(&mut self, now_ms: u64) -> Result<ReconnectSchedule, ReconnectError> {
        let attempt = self.state.attempts.saturating_add(1);
        if attempt > self.policy.max_attempts {
            let delay = self.policy.delay_for(attempt);
            self.state = ReconnectState {
                attempts: self.state.attempts,
                last_error_code: Some(RECONNECT_BUDGET_EXHAUSTED),
                next_attempt_ms: now_ms.saturating_add(delay),
            };
            return Err(ReconnectError::BudgetExhausted {
                attempts: self.state.attempts,
            });
        }
        let delay = self.policy.delay_for(attempt);
        self.state = ReconnectState {
            attempts: attempt,
            last_error_code: None,
            next_attempt_ms: now_ms.saturating_add(delay),
        };
        Ok(ReconnectSchedule {
            attempt,
            delay_ms: delay,
            next_attempt_ms: self.state.next_attempt_ms,
        })
    }

    /// 记录一次成功重连：清空计数与错误（下一次断线从 1 重新算）。
    pub fn record_success(&mut self) {
        self.state = ReconnectState::INITIAL;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_validation_rejects_out_of_range_values() {
        assert!(RetryPolicy::DEFAULT.validate().is_ok());
        let bad_attempts = RetryPolicy {
            max_attempts: 0,
            ..RetryPolicy::DEFAULT
        };
        assert_eq!(
            bad_attempts.validate().expect_err("attempts").code(),
            "INVALID_RECONNECT_POLICY"
        );
        let bad_permille = RetryPolicy {
            jitter_permille: 1_001,
            ..RetryPolicy::DEFAULT
        };
        assert_eq!(
            bad_permille.validate().expect_err("permille").code(),
            "INVALID_RECONNECT_POLICY"
        );
        let bad_cap = RetryPolicy {
            base_delay_ms: 2_000,
            max_delay_ms: 1_000,
            ..RetryPolicy::DEFAULT
        };
        assert!(bad_cap.validate().is_err());
    }

    #[test]
    fn attempts_state_machine_climbs_within_the_budget_and_reports_exhaustion() {
        let policy = RetryPolicy {
            max_attempts: 2,
            base_delay_ms: 100,
            max_delay_ms: 1_000,
            jitter_permille: 0,
        };
        let mut attempts = ReconnectAttempts::new(policy).expect("policy");
        let first = attempts.next_attempt(1_000).expect("first");
        assert_eq!(first.attempt, 1);
        assert_eq!(first.delay_ms, 100);
        assert_eq!(attempts.state().attempts, 1);
        let second = attempts.next_attempt(2_000).expect("second");
        assert_eq!(second.attempt, 2);
        assert_eq!(second.delay_ms, 200);
        let error = attempts.next_attempt(3_000).expect_err("budget exhausted");
        assert_eq!(error.code(), "RECONNECT_BUDGET_EXHAUSTED");
        let state = attempts.state();
        assert_eq!(state.last_error_code, Some("RECONNECT_BUDGET_EXHAUSTED"));
        assert_eq!(state.attempts, 2, "the rejected attempt is not counted");
        // 成功后计数清零，可重新从 1 开始。
        attempts.record_success();
        assert_eq!(attempts.state(), ReconnectState::INITIAL);
        assert_eq!(attempts.next_attempt(9_000).expect("restart").attempt, 1);
    }
}

