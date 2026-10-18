//! SYNC-011 `schedule_retry`：有抖动的指数退避、Retry-After 与重试预算。
//!
//! 硬性语义：
//!
//! - **分类优先**：错误先经 [`chat_domain::classify_retry`]（DOMAIN-010）分类，
//!   本模块不自行发明新的错误分类。
//! - **指数退避固定可测**：`base × multiplier^(attempt-1)`，上限 `max_delay_ms`；
//!   抖动窗口是基础退避的 ±`jitter_percent`%，由注入的 [`JitterSource`] 抽样，
//!   抽样值 0/500/1000 对应窗口下界/中点/上界（确定性可断言）。
//! - **Retry-After 优先**：服务器建议是**下界**，取
//!   `max(退避, clamp(retryAfter, 1, MAX_RETRY_AFTER_MS))`；上限固定为
//!   PROTOCOL-014 的 [`chat_protocol::MAX_RETRY_AFTER_MS`]（5 分钟）。
//! - **不可重试立即终态**：永久失败/等用户/需同步重建都不产生退避时刻；
//!   结果未知只做 `QueryExisting`（禁止换新 ID 重发）。
//! - **预算**：`attempt > max_attempts` 时返回 [`RetryAction::RetryBudgetExhausted`]
//!   （终态，保留根因码），不会无限重试。
//! - 本模块不 sleep、不读系统时间、不写数据库。

use chat_domain::{OperationState, RetryDecision, classify_retry};
use chat_protocol::MAX_RETRY_AFTER_MS;

use crate::error::{ErrorCode, Result, invalid_sequence, invalid_target};

pub use chat_domain::{OperationState as RetryOperationState, RetryDecision as RetryClassification};

/// 默认基础退避：1 秒。
pub const DEFAULT_BASE_DELAY_MS: u64 = 1_000;
/// 默认倍数。
pub const DEFAULT_MULTIPLIER: u32 = 2;
/// 默认自动重试上限：60 秒（服务器 Retry-After 可突破到 5 分钟）。
pub const DEFAULT_MAX_DELAY_MS: u64 = 60_000;
/// 默认抖动幅度：±20%。
pub const DEFAULT_JITTER_PERCENT: u32 = 20;
/// 默认连续失败预算：8 次。
pub const DEFAULT_MAX_ATTEMPTS: u32 = 8;
/// 抖动幅度上限（百分比）。
pub const MAX_JITTER_PERCENT: u32 = 100;
/// 抖动抽样空间：0..=1000（千分数）。
pub const JITTER_SAMPLE_MAX: u32 = 1_000;

/// 退避策略（全部参数固定且可测）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// 第 1 次失败的基础退避（毫秒）。
    pub base_delay_ms: u64,
    /// 每次失败的倍数（≥1）。
    pub multiplier: u32,
    /// 自动退避上限（毫秒），Retry-After 可突破但不超过 5 分钟。
    pub max_delay_ms: u64,
    /// 抖动幅度（百分比，0..=100）。
    pub jitter_percent: u32,
    /// 连续失败预算。
    pub max_attempts: u32,
}

impl RetryPolicy {
    /// 默认策略。
    pub const DEFAULT: Self = Self {
        base_delay_ms: DEFAULT_BASE_DELAY_MS,
        multiplier: DEFAULT_MULTIPLIER,
        max_delay_ms: DEFAULT_MAX_DELAY_MS,
        jitter_percent: DEFAULT_JITTER_PERCENT,
        max_attempts: DEFAULT_MAX_ATTEMPTS,
    };

    /// 有界校验（畸形或越界参数必须显式拒绝）。
    pub const fn validate(self) -> Result<()> {
        if self.base_delay_ms == 0 {
            return Err(invalid_target("retry base delay must be positive"));
        }
        if self.multiplier == 0 {
            return Err(invalid_target("retry multiplier must be at least 1"));
        }
        if self.max_delay_ms < self.base_delay_ms {
            return Err(invalid_target("retry max delay must not be below the base"));
        }
        if self.jitter_percent > MAX_JITTER_PERCENT {
            return Err(invalid_target("retry jitter must be within 0..=100 percent"));
        }
        if self.max_attempts == 0 {
            return Err(invalid_target("retry budget must be at least 1"));
        }
        Ok(())
    }

    /// 第 `attempt` 次失败的基础退避（不含抖动与 Retry-After）。`attempt` 从 1 起。
    pub const fn base_delay_for(self, attempt: u32) -> u64 {
        let exponent = attempt.saturating_sub(1);
        let mut delay = self.base_delay_ms;
        let mut step = 0u32;
        while step < exponent {
            if self.multiplier <= 1 {
                break;
            }
            match delay.checked_mul(self.multiplier as u64) {
                Some(next) => delay = next,
                None => {
                    delay = self.max_delay_ms;
                    break;
                }
            }
            if delay >= self.max_delay_ms {
                break;
            }
            step += 1;
        }
        if delay > self.max_delay_ms {
            self.max_delay_ms
        } else {
            delay
        }
    }
}

/// 抖动来源：返回 `0..=1000` 的千分抽样。
///
/// 生产实现由平台提供系统 RNG（P0 的 `SecureJitter` 在桥接层注入）；测试使用
/// [`FixedJitter`] 或 [`SeededJitter`] 保证确定性。抽到 0 表示窗口下界，
/// 500 表示中点（无偏移），1000 表示上界。
pub trait JitterSource {
    /// 抽样千分值。
    fn sample_permille(&mut self) -> u32;
}

/// 固定抽样（测试用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedJitter {
    sample_permille: u32,
}

impl FixedJitter {
    /// 由千分值构造（超过 1000 会被压到 1000）。
    pub const fn new(sample_permille: u32) -> Self {
        Self {
            sample_permille: if sample_permille > JITTER_SAMPLE_MAX {
                JITTER_SAMPLE_MAX
            } else {
                sample_permille
            },
        }
    }
}

impl JitterSource for FixedJitter {
    fn sample_permille(&mut self) -> u32 {
        self.sample_permille
    }
}

/// 确定性伪随机抽样（xorshift64*）：同种子得到同序列，用于可复现验收。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeededJitter {
    state: u64,
}

impl SeededJitter {
    /// 由种子构造（0 会被替换为非零常量，避免退化状态）。
    pub const fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0x9e37_79b9_7f4a_7c15 } else { seed },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}

impl JitterSource for SeededJitter {
    fn sample_permille(&mut self) -> u32 {
        (self.next_u64() % u64::from(JITTER_SAMPLE_MAX + 1)) as u32
    }
}

/// 对基础退避施加有界抖动。
///
/// 抖动窗口 = `base × jitter_percent / 100`；抽样 0/500/1000 分别得到
/// `base - window` / `base` / `base + window`，结果不会为负。
pub fn jittered_delay(base_delay_ms: u64, jitter_percent: u32, sample_permille: u32) -> u64 {
    let percent = jitter_percent.min(MAX_JITTER_PERCENT);
    let window = base_delay_ms.saturating_mul(u64::from(percent)) / 100;
    if window == 0 {
        return base_delay_ms;
    }
    let sample = u64::from(sample_permille.min(JITTER_SAMPLE_MAX));
    let scaled = window.saturating_mul(sample) / u64::from(JITTER_SAMPLE_MAX);
    let centered = i128::from(scaled) * 2 - i128::from(window);
    let result = i128::from(base_delay_ms) + centered;
    u64::try_from(result.max(0)).unwrap_or(0)
}

/// 尊重服务器 Retry-After：它是退避的**下界**，并被夹到 `1..=300000`。
pub fn apply_retry_after(delay_ms: u64, retry_after_ms: Option<u64>) -> u64 {
    match retry_after_ms {
        Some(raw) => delay_ms.max(raw.clamp(1, MAX_RETRY_AFTER_MS)),
        None => delay_ms,
    }
}

/// 一次失败的输入。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryInput {
    /// 当前时间（Unix 毫秒，由可信时钟提供）。
    pub now_unix_ms: i64,
    /// 根因冻结码（PROTOCOL-014）。
    pub code: ErrorCode,
    /// 操作当前正式状态（DOMAIN-010 分类输入）。
    pub state: OperationState,
    /// 服务器建议的 Retry-After（毫秒）。
    pub retry_after_ms: Option<u64>,
}

/// 重试策略结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryAction {
    /// 同 SubmissionId、同字节自动重发。
    RetrySameBytes,
    /// 结果未知：只按原 SubmissionId 查询/对账。
    QueryExisting,
    /// 先同步补齐再重建（新 SubmissionId），由同步循环立即安排。
    SyncThenRebuild,
    /// 停止自动重试，等用户处置。
    WaitForUser,
    /// 永久失败（终态）。
    PermanentFailure,
    /// 自动重试预算耗尽（终态，保留根因码）。
    RetryBudgetExhausted,
}

impl RetryAction {
    /// 是否会自动再发一次（同字节）。
    pub const fn retries_automatically(self) -> bool {
        matches!(self, Self::RetrySameBytes)
    }

    /// 是否终态（不再自动调度）。
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::PermanentFailure | Self::RetryBudgetExhausted | Self::WaitForUser
        )
    }
}

/// 排期结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetrySchedule {
    /// 策略结果。
    pub action: RetryAction,
    /// 根因冻结码（原样保留，不替换）。
    pub code: ErrorCode,
    /// DOMAIN-010 的分类。
    pub classification: RetryDecision,
    /// 本次失败序号（从 1 起）。
    pub attempt: u32,
    /// 需要等待的毫秒数；终态/立即动作是 `None`。
    pub delay_ms: Option<u64>,
    /// 下一次可尝试时间。
    pub next_attempt_at_ms: Option<i64>,
}

/// SYNC-011：计算一次失败后的退避与下一步动作。
///
/// `attempt` 是**本次失败**的连续序号（从 1 起）；`jitter` 只被抽样一次。
pub fn schedule_retry(
    policy: RetryPolicy,
    attempt: u32,
    input: RetryInput,
    jitter: &mut dyn JitterSource,
) -> Result<RetrySchedule> {
    policy.validate()?;
    if attempt == 0 {
        return Err(invalid_sequence("retry attempt must be at least 1"));
    }
    if input.now_unix_ms < 0 {
        return Err(invalid_target("retry clock must be non-negative"));
    }
    let classification = classify_retry(input.code, input.state);
    let action = match classification {
        RetryDecision::ResendSameBytes => RetryAction::RetrySameBytes,
        RetryDecision::QueryExisting => RetryAction::QueryExisting,
        RetryDecision::SyncThenRebuild => RetryAction::SyncThenRebuild,
        RetryDecision::WaitForUser => RetryAction::WaitForUser,
        RetryDecision::PermanentFailure => RetryAction::PermanentFailure,
    };

    if action == RetryAction::RetrySameBytes && attempt > policy.max_attempts {
        return Ok(RetrySchedule {
            action: RetryAction::RetryBudgetExhausted,
            code: input.code,
            classification,
            attempt,
            delay_ms: None,
            next_attempt_at_ms: None,
        });
    }

    let delay_ms = match action {
        RetryAction::RetrySameBytes | RetryAction::QueryExisting => {
            let base = policy.base_delay_for(attempt);
            let jittered = jittered_delay(base, policy.jitter_percent, jitter.sample_permille());
            Some(apply_retry_after(jittered, input.retry_after_ms))
        }
        RetryAction::SyncThenRebuild
        | RetryAction::WaitForUser
        | RetryAction::PermanentFailure
        | RetryAction::RetryBudgetExhausted => None,
    };
    let next_attempt_at_ms = delay_ms.map(|delay| {
        let offset = i64::try_from(delay).unwrap_or(i64::MAX);
        input.now_unix_ms.saturating_add(offset)
    });
    Ok(RetrySchedule {
        action,
        code: input.code,
        classification,
        attempt,
        delay_ms,
        next_attempt_at_ms,
    })
}

/// 连续失败计数账本。
///
/// 重置条件**只有一个**：收到正式成功证据（`record_success`）。断网/恢复连接、
/// 进程重启都不重置，因此重启后必须用 [`RetryLedger::restore`] 恢复计数，
/// 不允许把失败计数归零后重新无界重试。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryLedger {
    policy: RetryPolicy,
    consecutive_failures: u32,
}

impl RetryLedger {
    /// 新建账本（计数 0）。
    pub fn new(policy: RetryPolicy) -> Result<Self> {
        policy.validate()?;
        Ok(Self {
            policy,
            consecutive_failures: 0,
        })
    }

    /// 从持久化的连续失败计数恢复（重启路径）。
    pub fn restore(policy: RetryPolicy, consecutive_failures: u32) -> Result<Self> {
        policy.validate()?;
        Ok(Self {
            policy,
            consecutive_failures,
        })
    }

    /// 当前策略。
    pub const fn policy(&self) -> RetryPolicy {
        self.policy
    }

    /// 连续失败次数。
    pub const fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// 预算是否已耗尽。
    pub const fn is_exhausted(&self) -> bool {
        self.consecutive_failures > self.policy.max_attempts
    }

    /// 记录一次失败并排期；校验失败不改变计数。
    pub fn record_failure(
        &mut self,
        input: RetryInput,
        jitter: &mut dyn JitterSource,
    ) -> Result<RetrySchedule> {
        let next = self.consecutive_failures.saturating_add(1);
        let schedule = schedule_retry(self.policy, next, input, jitter)?;
        self.consecutive_failures = next;
        Ok(schedule)
    }

    /// 记录正式成功并重置计数，返回重置前的连续失败次数。
    pub fn record_success(&mut self) -> u32 {
        let previous = self.consecutive_failures;
        self.consecutive_failures = 0;
        previous
    }
}
