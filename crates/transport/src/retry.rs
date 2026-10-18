//! TRANSPORT-004 的纯逻辑子集：连接重试退避。
//!
//! 语义：
//!
//! - **指数退避**：第 `n` 次失败的基础延迟 = `min(base × 2^(n-1), max)`；
//! - **上限**：`max_delay_ms` 受 PROTOCOL-014 的 5 分钟上限约束（`MAX_RETRY_AFTER_MS`）；
//! - **有界 jitter**：在基础延迟上加 `[0, jitter_max_ms]` 的确定性偏移，最终仍不超过
//!   `max_delay_ms`；jitter 只用于打散同步重试，**不是安全随机**，测试用固定 seed
//!   完全可复现；
//! - **预算耗尽 Stop**：连续失败次数超过 `max_attempts` 后返回 `Stop`，不无限重试；
//! - 本模块不 sleep、不联网：只计算延迟与下一次尝试时间，由宿主执行等待。

use chat_protocol::MAX_RETRY_AFTER_MS;

use crate::error::{Result, TransportError};

/// 退避上限的硬上限（5 分钟）。
pub const RETRY_HARD_MAX_DELAY_MS: u64 = MAX_RETRY_AFTER_MS;
/// 重试次数硬上限。
pub const RETRY_HARD_MAX_ATTEMPTS: u32 = 100;

/// 重试策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// 首次失败的基础延迟（毫秒，≥1）。
    pub base_delay_ms: u64,
    /// 延迟上限（毫秒，≥ base 且 ≤ 300000）。
    pub max_delay_ms: u64,
    /// 连续失败预算（≥1，失败次数超过它即 Stop）。
    pub max_attempts: u32,
    /// jitter 上限（毫秒，≤ base 且 ≤ max_delay）。
    pub jitter_max_ms: u64,
    /// jitter 的确定性种子（测试固定；生产可由宿主随机生成一次）。
    pub jitter_seed: u64,
}

impl RetryPolicy {
    /// 默认策略：1s 起、60s 封顶、8 次预算、250ms jitter。
    pub const DEFAULT: RetryPolicy = RetryPolicy {
        base_delay_ms: 1_000,
        max_delay_ms: 60_000,
        max_attempts: 8,
        jitter_max_ms: 250,
        jitter_seed: 0x9e37_79b9_7f4a_7c15,
    };

    /// 有界校验；非法策略显式拒绝，不夹取。
    pub fn validate(&self) -> Result<()> {
        if self.base_delay_ms == 0 || self.base_delay_ms > RETRY_HARD_MAX_DELAY_MS {
            return Err(TransportError::InvalidRetryPolicy {
                detail: "baseDelayMs must be within 1..=300000",
            });
        }
        if self.max_delay_ms < self.base_delay_ms || self.max_delay_ms > RETRY_HARD_MAX_DELAY_MS {
            return Err(TransportError::InvalidRetryPolicy {
                detail: "maxDelayMs must be within baseDelayMs..=300000",
            });
        }
        if self.max_attempts == 0 || self.max_attempts > RETRY_HARD_MAX_ATTEMPTS {
            return Err(TransportError::InvalidRetryPolicy {
                detail: "maxAttempts must be within 1..=100",
            });
        }
        if self.jitter_max_ms > self.base_delay_ms || self.jitter_max_ms > self.max_delay_ms {
            return Err(TransportError::InvalidRetryPolicy {
                detail: "jitterMaxMs must not exceed baseDelayMs or maxDelayMs",
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

    /// 第 `attempt` 次失败的有界 jitter（毫秒，确定性）。
    pub fn jitter_for(&self, attempt: u32) -> u64 {
        if self.jitter_max_ms == 0 {
            return 0;
        }
        let mixed =
            mix64(self.jitter_seed ^ u64::from(attempt).wrapping_mul(0x9e37_79b9_7f4a_7c15));
        mixed % (self.jitter_max_ms + 1)
    }

    /// 第 `attempt` 次失败的最终延迟：基础 + jitter，仍封顶。
    pub fn delay_for(&self, attempt: u32) -> u64 {
        self.base_delay_for(attempt.max(1))
            .saturating_add(self.jitter_for(attempt.max(1)))
            .min(self.max_delay_ms)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// splitmix64 混淆：仅用于调度 jitter，不是安全随机，不用于密钥/令牌。
const fn mix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

/// 停止原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryStopReason {
    /// 连续失败次数超过预算。
    BudgetExhausted,
}

/// 一次失败后的决定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryOutcome {
    /// 等待 `delay_ms` 后重试。
    Retry {
        /// 本次失败次数（从 1 开始）。
        attempt: u32,
        /// 等待时长（毫秒）。
        delay_ms: u64,
        /// 下一次允许尝试的时间（Unix 毫秒）。
        next_attempt_at_ms: u64,
    },
    /// 停止自动重试，等待用户/上层处置。
    Stop {
        /// 停止原因。
        reason: RetryStopReason,
    },
}

/// 有状态的重试跟踪器（只持有计数）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryTracker {
    policy: RetryPolicy,
    attempts: u32,
}

impl RetryTracker {
    /// 以策略创建跟踪器。
    pub fn new(policy: RetryPolicy) -> Result<Self> {
        policy.validate()?;
        Ok(Self {
            policy,
            attempts: 0,
        })
    }

    /// 当前连续失败次数。
    pub const fn attempts(&self) -> u32 {
        self.attempts
    }

    /// 当前策略。
    pub const fn policy(&self) -> RetryPolicy {
        self.policy
    }

    /// 成功后清零预算。
    pub fn reset(&mut self) {
        self.attempts = 0;
    }

    /// 记录一次失败并给出重试决定。
    ///
    /// `now_ms` 是本次失败的时间；返回的 `next_attempt_at_ms` 会饱和到 u64 上限。
    pub fn record_failure(&mut self, now_ms: u64) -> RetryOutcome {
        self.attempts = self.attempts.saturating_add(1);
        if self.attempts > self.policy.max_attempts {
            return RetryOutcome::Stop {
                reason: RetryStopReason::BudgetExhausted,
            };
        }
        let delay_ms = self.policy.delay_for(self.attempts);
        RetryOutcome::Retry {
            attempt: self.attempts,
            delay_ms,
            next_attempt_at_ms: now_ms.saturating_add(delay_ms),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deterministic(policy: RetryPolicy) -> RetryTracker {
        RetryTracker::new(policy).unwrap()
    }

    #[test]
    fn exponential_base_delay_is_capped() {
        let policy = RetryPolicy {
            jitter_max_ms: 0,
            ..RetryPolicy::DEFAULT
        };
        assert_eq!(policy.base_delay_for(1), 1_000);
        assert_eq!(policy.base_delay_for(2), 2_000);
        assert_eq!(policy.base_delay_for(3), 4_000);
        assert_eq!(policy.base_delay_for(4), 8_000);
        assert_eq!(policy.base_delay_for(5), 16_000);
        assert_eq!(policy.base_delay_for(6), 32_000);
        assert_eq!(policy.base_delay_for(7), 60_000);
        assert_eq!(policy.base_delay_for(8), 60_000);
        assert_eq!(policy.base_delay_for(u32::MAX), 60_000);
        // attempt 0 按 1 处理，不放大也不 panic。
        assert_eq!(policy.base_delay_for(0), 1_000);
    }

    #[test]
    fn jitter_is_bounded_deterministic_and_capped() {
        let policy = RetryPolicy {
            base_delay_ms: 1_000,
            max_delay_ms: 2_000,
            max_attempts: 100,
            jitter_max_ms: 500,
            jitter_seed: 7,
        };
        for attempt in 1..=20u32 {
            let jitter = policy.jitter_for(attempt);
            assert!(jitter <= 500, "jitter {jitter} out of bounds");
            let delay = policy.delay_for(attempt);
            assert!(delay <= 2_000, "delay {delay} exceeds cap");
            assert!(delay >= policy.base_delay_for(attempt));
            // 确定性。
            assert_eq!(delay, policy.delay_for(attempt));
        }
        // 不同 seed 至少在某个尝试上不同（避免“无 jitter”假实现）。
        let mut other = policy;
        other.jitter_seed = 11;
        assert!((1..=20u32).any(|attempt| policy.delay_for(attempt) != other.delay_for(attempt)));
        // jitter 为 0 时与基础延迟完全一致。
        let mut no_jitter = policy;
        no_jitter.jitter_max_ms = 0;
        assert_eq!(no_jitter.jitter_for(1), 0);
        assert_eq!(no_jitter.delay_for(3), no_jitter.base_delay_for(3));
    }

    #[test]
    fn budget_exhaustion_stops_retrying() {
        let policy = RetryPolicy {
            jitter_max_ms: 0,
            max_attempts: 3,
            ..RetryPolicy::DEFAULT
        };
        let mut tracker = deterministic(policy);
        assert!(matches!(
            tracker.record_failure(1_000),
            RetryOutcome::Retry {
                attempt: 1,
                delay_ms: 1_000,
                ..
            }
        ));
        assert!(matches!(
            tracker.record_failure(2_000),
            RetryOutcome::Retry { attempt: 2, .. }
        ));
        assert!(matches!(
            tracker.record_failure(3_000),
            RetryOutcome::Retry { attempt: 3, .. }
        ));
        assert_eq!(
            tracker.record_failure(4_000),
            RetryOutcome::Stop {
                reason: RetryStopReason::BudgetExhausted
            }
        );
        // 继续失败仍是 Stop，不产生新的自动重试。
        assert!(matches!(
            tracker.record_failure(5_000),
            RetryOutcome::Stop { .. }
        ));
        assert_eq!(tracker.attempts(), 5);
        // 成功重置后重新计数。
        tracker.reset();
        assert_eq!(tracker.attempts(), 0);
        assert!(matches!(
            tracker.record_failure(10_000),
            RetryOutcome::Retry { attempt: 1, .. }
        ));
    }

    #[test]
    fn next_attempt_time_is_saturating() {
        let policy = RetryPolicy {
            jitter_max_ms: 0,
            ..RetryPolicy::DEFAULT
        };
        let mut tracker = deterministic(policy);
        match tracker.record_failure(u64::MAX - 1) {
            RetryOutcome::Retry {
                next_attempt_at_ms, ..
            } => assert_eq!(next_attempt_at_ms, u64::MAX),
            other => panic!("expected retry, got {other:?}"),
        }
    }

    #[test]
    fn invalid_policies_are_rejected() {
        for policy in [
            RetryPolicy {
                base_delay_ms: 0,
                ..RetryPolicy::DEFAULT
            },
            RetryPolicy {
                base_delay_ms: RETRY_HARD_MAX_DELAY_MS + 1,
                ..RetryPolicy::DEFAULT
            },
            RetryPolicy {
                max_delay_ms: 999,
                ..RetryPolicy::DEFAULT
            },
            RetryPolicy {
                max_delay_ms: RETRY_HARD_MAX_DELAY_MS + 1,
                ..RetryPolicy::DEFAULT
            },
            RetryPolicy {
                max_attempts: 0,
                ..RetryPolicy::DEFAULT
            },
            RetryPolicy {
                max_attempts: RETRY_HARD_MAX_ATTEMPTS + 1,
                ..RetryPolicy::DEFAULT
            },
            RetryPolicy {
                jitter_max_ms: 2_000,
                ..RetryPolicy::DEFAULT
            },
        ] {
            let error = RetryTracker::new(policy).unwrap_err();
            assert_eq!(error.code(), "INVALID_RETRY_POLICY");
            assert_eq!(
                policy.validate().unwrap_err().code(),
                "INVALID_RETRY_POLICY"
            );
        }
        RetryPolicy::DEFAULT.validate().unwrap();
    }
}
