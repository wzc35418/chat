//! SYNC-002 决策核心：以节点签名 head 为目标的连续拉取循环控制。
//!
//! 本模块是**纯决策函数**：输入本地已连续应用到的位置、节点签名 head、上一次拉取
//! 结果与退避状态，输出下一步动作（[`PullAction`]）。它不联网、不读库、不 sleep，
//! 也不做任何“看起来成功”的推断：
//!
//! - **缺口不静默跳过**：上一次拉到的页若不是从 `local_tail + 1` 开始、越过了节点
//!   head、或节点声称 head 前进却返回空页，一律 [`PullAction::Resync`]（交给
//!   [`crate::catchup::catchup_plan`]），绝不返回一个“跳过去”的 Pull 范围。
//! - **范围有界**：单次 Pull 的条数不超过策略的 `page_items`；落后超过
//!   `max_incremental_gap` 时不一次拉完，而是 Resync 交给追赶计划。
//! - **不忙循环**：未到 `next_attempt_at_ms` 返回 [`PullAction::Backoff`] 且不调用
//!   数据源；失败按 `base × 2^(failures-1)` 退避（封顶 `failure_max_backoff_ms`），
//!   Retry-After 是下界并按 PROTOCOL-014 夹到 1..=5 分钟；超过失败预算即
//!   [`PullAction::Stop`]，不无限重试。
//! - **可重入/幂等**：同一输入永远得到同一动作；应用一次 Pull 后重新决策只前进一页。
//!
//! 信箱页面的逐条消费/去重/ACK 记账仍在 [`crate::mailbox::MailboxPoller`]（SYNC-007）；
//! 本模块只决定“这一轮该不该拉、拉哪一段、等多久、是否需要重新对齐”。

use chat_protocol::MAX_RETRY_AFTER_MS;

use crate::error::{Result, content_too_large, corrupted, invalid_cursor, invalid_target};

/// 单次 Pull 的默认条数（与信箱单页上限一致）。
pub const DEFAULT_PULL_PAGE_ITEMS: u32 = crate::mailbox::MAX_MAILBOX_PAGE_ITEMS;
/// 单次 Pull 允许配置的最大条数。
pub const MAX_PULL_PAGE_ITEMS: u32 = 500;
/// 允许增量拉取的最大落后跨度；超过即要求 Resync（追赶计划）。
pub const DEFAULT_MAX_INCREMENTAL_GAP: u64 = 1_000;
/// 落后跨度的配置上限。
pub const MAX_INCREMENTAL_GAP_LIMIT: u64 = 100_000;
/// 连续失败预算（与 SYNC-011 的默认预算一致）。
pub const DEFAULT_PULL_FAILURE_BUDGET: u32 = crate::retry::DEFAULT_MAX_ATTEMPTS;
/// 空转/未到期的默认退避（与信箱空页退避一致）。
pub const DEFAULT_IDLE_BACKOFF_MS: i64 = crate::mailbox::EMPTY_PAGE_BACKOFF_MS;
/// 失败退避基数（与信箱拉取失败退避一致）。
pub const DEFAULT_FAILURE_BASE_BACKOFF_MS: i64 = crate::mailbox::PULL_FAILURE_BASE_BACKOFF_MS;
/// 失败退避上限（与信箱拉取失败退避一致）。
pub const DEFAULT_FAILURE_MAX_BACKOFF_MS: i64 = crate::mailbox::PULL_FAILURE_MAX_BACKOFF_MS;
/// 非空页之后的最小间隔（与信箱页间隔一致），避免一页一条时忙循环。
pub const DEFAULT_PAGE_INTERVAL_MS: i64 = crate::mailbox::PAGE_INTERVAL_MS;
/// 序号可安全写入 i64 (SQLite) 的上限。
pub const MAX_PULL_SEQUENCE: u64 = chat_protocol::MAX_CURSOR_SEQ;
/// 退避上限的配置上限。
pub const MAX_BACKOFF_CEILING_MS: i64 = 600_000;

/// 拉取循环策略（全部取值固定且可测）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullPolicy {
    /// 单次 Pull 的条数上限（≥1）。
    pub page_items: u32,
    /// 允许增量拉取的最大落后跨度；超过走 Resync（≥ page_items）。
    pub max_incremental_gap: u64,
    /// 连续失败预算（≥1）。
    pub failure_budget: u32,
    /// 已到 head / 无需拉取时的退避（≥1 ms）。
    pub idle_backoff_ms: i64,
    /// 失败退避基数（≥1 ms）。
    pub failure_base_backoff_ms: i64,
    /// 失败退避上限（≥ 基数）。
    pub failure_max_backoff_ms: i64,
    /// 非空页之后的最小间隔（≥0 ms）。
    pub page_interval_ms: i64,
}

impl PullPolicy {
    /// 默认策略。
    pub const DEFAULT: Self = Self {
        page_items: DEFAULT_PULL_PAGE_ITEMS,
        max_incremental_gap: DEFAULT_MAX_INCREMENTAL_GAP,
        failure_budget: DEFAULT_PULL_FAILURE_BUDGET,
        idle_backoff_ms: DEFAULT_IDLE_BACKOFF_MS,
        failure_base_backoff_ms: DEFAULT_FAILURE_BASE_BACKOFF_MS,
        failure_max_backoff_ms: DEFAULT_FAILURE_MAX_BACKOFF_MS,
        page_interval_ms: DEFAULT_PAGE_INTERVAL_MS,
    };

    /// 有界校验：畸形或越界参数必须显式拒绝，不接受“夹到合法范围”。
    pub const fn validate(self) -> Result<()> {
        if self.page_items == 0 || self.page_items > MAX_PULL_PAGE_ITEMS {
            return Err(invalid_target("pull page items must be within 1..=500"));
        }
        if self.max_incremental_gap < self.page_items as u64
            || self.max_incremental_gap > MAX_INCREMENTAL_GAP_LIMIT
        {
            return Err(invalid_target(
                "pull max incremental gap must be within page items..=100000",
            ));
        }
        if self.failure_budget == 0 {
            return Err(invalid_target("pull failure budget must be at least 1"));
        }
        if self.idle_backoff_ms < 1 || self.idle_backoff_ms > MAX_BACKOFF_CEILING_MS {
            return Err(invalid_target("pull idle backoff must be within 1..=600000 ms"));
        }
        if self.failure_base_backoff_ms < 1
            || self.failure_max_backoff_ms < self.failure_base_backoff_ms
            || self.failure_max_backoff_ms > MAX_BACKOFF_CEILING_MS
        {
            return Err(invalid_target(
                "pull failure backoff must satisfy 1 <= base <= max <= 600000 ms",
            ));
        }
        if self.failure_max_backoff_ms > MAX_RETRY_AFTER_MS as i64 {
            return Err(invalid_target("pull failure backoff must not exceed 5 minutes"));
        }
        if self.page_interval_ms < 0 || self.page_interval_ms > MAX_BACKOFF_CEILING_MS {
            return Err(invalid_target("pull page interval must be within 0..=600000 ms"));
        }
        Ok(())
    }

    /// 第 `failures` 次连续失败的基础退避（不含 Retry-After）。
    pub const fn failure_delay_for(self, failures: u32) -> i64 {
        let exponent = failures.saturating_sub(1);
        let mut delay = self.failure_base_backoff_ms;
        let mut step = 0u32;
        while step < exponent {
            delay = match delay.checked_mul(2) {
                Some(next) => next,
                None => self.failure_max_backoff_ms,
            };
            if delay >= self.failure_max_backoff_ms {
                return self.failure_max_backoff_ms;
            }
            step += 1;
        }
        if delay > self.failure_max_backoff_ms {
            self.failure_max_backoff_ms
        } else {
            delay
        }
    }
}

/// 上一次拉取的观察结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LastPull {
    /// 还没有拉过（启动/刚 Resync 之后）。
    NotPolled,
    /// 拿到一页连续条目 `[from, to]`（含端点）。
    Page {
        /// 本页第一条的位置。
        from: u64,
        /// 本页最后一条的位置。
        to: u64,
        /// 条目数。
        items: u32,
    },
    /// 节点返回空页（没有可用条目）。
    Empty,
    /// 拉取失败（可带服务器 Retry-After）。
    Failed {
        /// 服务器建议的重试间隔（毫秒）。
        retry_after_ms: Option<u64>,
    },
}

/// 一次决策的输入。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullInput {
    /// 本地已连续应用到的位置（0 = 尚未应用任何条目）。
    pub local_tail: u64,
    /// 节点签名 head 的序号（0 = 空群）。
    pub node_head: u64,
    /// 上一次拉取结果。
    pub last: LastPull,
    /// 连续失败次数（持久化计数，重启不清零）。
    pub consecutive_failures: u32,
    /// 当前时间（Unix 毫秒）。
    pub now_ms: i64,
    /// 上一次成功拉取的时间（用于页间隔；从未拉过为 None）。
    pub last_pull_at_ms: Option<i64>,
    /// 下一次允许拉取的时间（由上一次决策写出并持久化）。
    pub next_attempt_at_ms: i64,
}

/// 退避原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackoffReason {
    /// 还没到 `next_attempt_at_ms`：本轮不调用数据源。
    NotDue,
    /// 已到节点 head，没有新条目。
    IdleAtHead,
    /// 上一页之后未满页间隔（避免一页一条时忙循环）。
    PageInterval,
    /// 拉取失败，按退避重试。
    Failure,
}

/// 需要重新对齐（交给追赶计划）的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResyncReason {
    /// 本地位置超过节点 head（节点回退/换节点）：不能“往前拉”，必须重新对齐。
    LocalAheadOfNode,
    /// 上一页不是从 `local_tail + 1` 开始：中间有缺口，绝不跳过。
    NonContiguousPage,
    /// 上一页越过了节点 head。
    PageBeyondHead,
    /// 节点声称 head 前进却返回空页：条目不可达。
    EmptyPageWithHeadAhead,
    /// 落后超过增量拉取上限：改为追赶计划，不一次拉超大范围。
    GapTooLarge,
}

/// 停止该群拉取循环的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// 连续失败超过预算：停止自动重试，等用户处置。
    FailureBudgetExhausted,
}

/// 下一步动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullAction {
    /// 拉取 `[from, to]`（含端点，条数 ≤ `page_items`）。
    Pull {
        /// 起始位置。
        from: u64,
        /// 结束位置。
        to: u64,
    },
    /// 等待 `delay_ms` 后重新决策；本轮不调用数据源。
    Backoff {
        /// 等待时长（毫秒）。
        delay_ms: u64,
        /// 下一次允许拉取的时间。
        next_attempt_at_ms: i64,
        /// 退避原因。
        reason: BackoffReason,
    },
    /// 本地状态与节点不连续：必须重新对齐（SYNC-008 追赶计划）。
    Resync {
        /// 原因。
        reason: ResyncReason,
    },
    /// 停止该群拉取循环。
    Stop {
        /// 原因。
        reason: StopReason,
    },
}

impl PullAction {
    /// 是否是一次拉取。
    pub const fn is_pull(self) -> bool {
        matches!(self, Self::Pull { .. })
    }

    /// 本次拉取的条数（非 Pull 为 None）。
    pub const fn pull_len(self) -> Option<u64> {
        match self {
            Self::Pull { from, to } => Some(to.saturating_sub(from).saturating_add(1)),
            _ => None,
        }
    }
}

/// SYNC-002 决策核心：计算拉取循环的下一步动作。
///
/// 顺序（任一失败都显式返回冻结错误码，不继续推进）：
/// 策略/时间校验 → 失败预算 → 未到期 → 失败退避 → 本地越过 head →
/// 上一页连续性 → 页间隔 → 空页但 head 前进 → 已到 head → 跨度上限 → 有界 Pull。
pub fn mailbox_pull_loop(policy: PullPolicy, input: PullInput) -> Result<PullAction> {
    policy.validate()?;
    if input.now_ms < 0 {
        return Err(invalid_target("pull clock must be non-negative"));
    }
    if input.next_attempt_at_ms < 0 {
        return Err(invalid_target("pull next attempt must be non-negative"));
    }
    if input.local_tail > MAX_PULL_SEQUENCE || input.node_head > MAX_PULL_SEQUENCE {
        return Err(invalid_target("pull sequence exceeds the storable range"));
    }
    if let Some(last_pull_at_ms) = input.last_pull_at_ms {
        if last_pull_at_ms < 0 {
            return Err(invalid_target("pull last poll time must be non-negative"));
        }
        if last_pull_at_ms > input.now_ms {
            return Err(invalid_target("pull clock moved backwards"));
        }
    }
    if let LastPull::Page { from, to, items } = input.last {
        if items == 0 {
            return Err(corrupted("empty page must be reported as LastPull::Empty"));
        }
        if to < from {
            return Err(invalid_cursor("pull page end precedes its start"));
        }
        let page_len = to.saturating_sub(from).saturating_add(1);
        if page_len > u64::from(policy.page_items) {
            return Err(content_too_large("pull page exceeds the configured page items"));
        }
        if u64::from(items) != page_len {
            return Err(corrupted("pull page item count does not match its range"));
        }
    }
    if matches!(input.last, LastPull::Failed { .. }) && input.consecutive_failures == 0 {
        return Err(corrupted("pull failure must be counted before backoff"));
    }

    // 1. 失败预算：超过即终态停止，不无限重试。
    if input.consecutive_failures > policy.failure_budget {
        return Ok(PullAction::Stop {
            reason: StopReason::FailureBudgetExhausted,
        });
    }

    // 2. 未到期：本轮不调用数据源。
    if input.now_ms < input.next_attempt_at_ms {
        let delay_ms = input.next_attempt_at_ms.saturating_sub(input.now_ms);
        return Ok(PullAction::Backoff {
            delay_ms: u64::try_from(delay_ms).unwrap_or(u64::MAX),
            next_attempt_at_ms: input.next_attempt_at_ms,
            reason: BackoffReason::NotDue,
        });
    }

    // 3. 失败退避：指数增长封顶，Retry-After 是下界。
    if let LastPull::Failed { retry_after_ms } = input.last {
        let delay_ms = failure_backoff(policy, input.consecutive_failures, retry_after_ms);
        let next_attempt_at_ms = input
            .now_ms
            .saturating_add(i64::try_from(delay_ms).unwrap_or(i64::MAX));
        return Ok(PullAction::Backoff {
            delay_ms,
            next_attempt_at_ms,
            reason: BackoffReason::Failure,
        });
    }

    // 4. 本地越过节点 head：不能靠“继续拉”自愈。
    if input.local_tail > input.node_head {
        return Ok(PullAction::Resync {
            reason: ResyncReason::LocalAheadOfNode,
        });
    }

    // 5. 上一页连续性：缺口绝不静默跳过。
    //
    // 允许两种合法形态：页已应用（`to == local_tail`）或页已拉到但尚未应用
    // （`from == local_tail + 1`）；用覆盖式判定「`from <= local_tail + 1 && to >= local_tail`」
    // 同时接受这两种形态，而把任何空洞（`from > local_tail + 1`）与过期页
    // （`to < local_tail`）判为需要重新对齐。
    if let LastPull::Page { from, to, .. } = input.last {
        let next_needed = input.local_tail.saturating_add(1);
        let covers_next = from <= next_needed;
        let reaches_local = to >= input.local_tail;
        if !covers_next || !reaches_local {
            return Ok(PullAction::Resync {
                reason: ResyncReason::NonContiguousPage,
            });
        }
        if to > input.node_head {
            return Ok(PullAction::Resync {
                reason: ResyncReason::PageBeyondHead,
            });
        }
        // 6. 页间隔：避免一页一条时忙循环。
        if policy.page_interval_ms > 0
            && let Some(last_pull_at_ms) = input.last_pull_at_ms
        {
            let due_at = last_pull_at_ms.saturating_add(policy.page_interval_ms);
            if input.now_ms < due_at {
                let delay_ms = due_at.saturating_sub(input.now_ms);
                return Ok(PullAction::Backoff {
                    delay_ms: u64::try_from(delay_ms).unwrap_or(u64::MAX),
                    next_attempt_at_ms: due_at,
                    reason: BackoffReason::PageInterval,
                });
            }
        }
    }

    // 7. 空页但 head 前进：条目不可达，必须重新对齐。
    if matches!(input.last, LastPull::Empty) && input.node_head > input.local_tail {
        return Ok(PullAction::Resync {
            reason: ResyncReason::EmptyPageWithHeadAhead,
        });
    }

    // 8. 已到 head：不空转。
    if input.local_tail == input.node_head {
        let next_attempt_at_ms = input.now_ms.saturating_add(policy.idle_backoff_ms);
        return Ok(PullAction::Backoff {
            delay_ms: u64::try_from(policy.idle_backoff_ms).unwrap_or(u64::MAX),
            next_attempt_at_ms,
            reason: BackoffReason::IdleAtHead,
        });
    }

    // 9. 落后超过增量拉取上限：交给追赶计划，不一次拉超大范围。
    let gap = input.node_head - input.local_tail;
    if gap > policy.max_incremental_gap {
        return Ok(PullAction::Resync {
            reason: ResyncReason::GapTooLarge,
        });
    }

    // 10. 有界 Pull：一次最多 page_items 条，且不越过节点 head。
    let from = input.local_tail.saturating_add(1);
    let max_to = from.saturating_add(u64::from(policy.page_items) - 1);
    let to = input.node_head.min(max_to);
    if to < from {
        return Err(invalid_cursor("pull range is empty"));
    }
    let span = to.saturating_sub(from).saturating_add(1);
    if span > u64::from(policy.page_items) {
        return Err(content_too_large("pull range exceeds the configured page items"));
    }
    Ok(PullAction::Pull { from, to })
}

/// 失败退避 = max(指数退避(封顶 failure_max), clamp(Retry-After, 1, 5 分钟))。
///
/// Retry-After 是服务器建议的**下界**，因此不把它再压回自动退避上限；两者都受
/// PROTOCOL-014 的 5 分钟上限约束（策略已校验 failure_max ≤ 5 分钟）。
fn failure_backoff(policy: PullPolicy, failures: u32, retry_after_ms: Option<u64>) -> u64 {
    let base = policy.failure_delay_for(failures.max(1));
    let base = u64::try_from(base).unwrap_or(u64::MAX);
    match retry_after_ms {
        Some(raw) => base.max(raw.clamp(1, MAX_RETRY_AFTER_MS)),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_delay_is_exponential_and_capped() {
        let policy = PullPolicy::DEFAULT;
        assert_eq!(policy.failure_delay_for(1), 1_000);
        assert_eq!(policy.failure_delay_for(2), 2_000);
        assert_eq!(policy.failure_delay_for(3), 4_000);
        assert_eq!(policy.failure_delay_for(7), 60_000);
        assert_eq!(policy.failure_delay_for(31), 60_000);
        assert_eq!(policy.failure_delay_for(u32::MAX), 60_000);
    }

    #[test]
    fn policy_rejects_out_of_range_values() {
        let mut policy = PullPolicy::DEFAULT;
        assert!(policy.validate().is_ok());
        policy.page_items = 0;
        assert!(policy.validate().is_err());
        policy = PullPolicy::DEFAULT;
        policy.page_items = MAX_PULL_PAGE_ITEMS + 1;
        assert!(policy.validate().is_err());
        policy = PullPolicy::DEFAULT;
        policy.max_incremental_gap = 1;
        assert!(policy.validate().is_err());
        policy = PullPolicy::DEFAULT;
        policy.failure_budget = 0;
        assert!(policy.validate().is_err());
        policy = PullPolicy::DEFAULT;
        policy.failure_base_backoff_ms = 0;
        assert!(policy.validate().is_err());
        policy = PullPolicy::DEFAULT;
        policy.failure_max_backoff_ms = MAX_RETRY_AFTER_MS as i64 + 1;
        assert!(policy.validate().is_err());
        policy = PullPolicy::DEFAULT;
        policy.page_interval_ms = -1;
        assert!(policy.validate().is_err());
    }
}