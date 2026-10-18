//! SYNC-008 决策核心：追赶计划。
//!
//! 输入本机已验证 head（[`VerifiedHead`]）与节点签名 head 的公开字段，输出**按序执行**
//! 的追赶步骤；越界或超限一律显式失败，绝不截断成“看起来补齐了”的计划。
//!
//! 硬性语义：
//!
//! - **不跳过、不猜 epoch**：日志范围从 `local.log_sequence + 1` 起按页递增；需要重建
//!   epoch 时按 epoch 逐个列出（不允许从落后 epoch 直接跳到目标 epoch）。
//! - **缺控制日志不能直接解密新 epoch**：若节点 head 声称了更新的 epoch/控制头，但
//!   没有任何新日志条目承载该控制结果（`span == 0`），计划只给出
//!   [`CatchupStep::AwaitControl`]——必须等待控制结果可达，绝不猜测重建。
//! - **本地策略已更新但节点还没包含**时同样只等待（[`AwaitReason::PendingControlResult`]），
//!   不回退本地策略去拉旧状态。
//! - **冲突显式失败**：本地日志序号/epoch 超过节点 head → `LOG_CONFLICT`；计划跨度超过
//!   策略上限 → `CONTENT_TOO_LARGE`；跨 epoch 数超过上限 → `MISSING_LOG_ENTRIES`。
//! - **幂等**：已完全追平时返回空计划（[`CatchupPlan::is_caught_up`]），重复调用得到同一结果。
//!
//! 本函数不联网、不验签、不写库：节点 head 的签名验证属于 TRANSPORT-007/PROTOCOL-007，
//! 逐条应用属于 SYNC-009。

use chat_protocol::{ControlHead, Digest, GroupId, MAX_CURSOR_SEQ, VerifiedHead};

use crate::error::{
    Result, content_too_large, invalid_target, log_conflict, missing_log_entries,
};

/// 单步拉取的默认条数（与信箱单页上限一致）。
pub const DEFAULT_CATCHUP_PAGE_ITEMS: u32 = crate::mailbox::MAX_MAILBOX_PAGE_ITEMS;
/// 单步拉取允许配置的最大条数。
pub const MAX_CATCHUP_PAGE_ITEMS: u32 = 500;
/// 单次计划允许拉取的默认条目总数。
pub const DEFAULT_MAX_CATCHUP_ENTRIES: u64 = 5_000;
/// 单次计划条目总数的配置上限。
pub const MAX_CATCHUP_ENTRIES_LIMIT: u64 = 100_000;
/// 单次计划允许的默认步数。
pub const DEFAULT_MAX_CATCHUP_STEPS: u32 = 64;
/// 单次计划步数的配置上限。
pub const MAX_CATCHUP_STEPS_LIMIT: u32 = 256;
/// 单次计划允许跨越的默认 epoch 数。
pub const DEFAULT_MAX_EPOCH_JUMPS: u64 = 64;
/// 跨 epoch 数的配置上限。
pub const MAX_EPOCH_JUMPS_LIMIT: u64 = 1_024;

/// 节点签名 head 的公开字段。
///
/// 调用方必须先按 TRANSPORT-007/PROTOCOL-007 用固定节点公钥验证签名，再把字段传进来；
/// 本函数只做计划，不验签也不信任额外内容。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeLogHead {
    /// 节点分配的日志序号。
    pub log_sequence: u64,
    /// 节点日志链尾摘要。
    pub log_hash: Digest,
    /// 节点当前控制头。
    pub control_head: ControlHead,
    /// 节点当前策略版本。
    pub policy_version: u64,
    /// 节点当前 MLS epoch。
    pub mls_epoch: u64,
}

/// 追赶策略（全部取值固定且可测）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatchupPolicy {
    /// 单步拉取条数（≥1）。
    pub page_items: u32,
    /// 单次计划允许的条目总数（≥ page_items）。
    pub max_entries: u64,
    /// 单次计划允许的步数（≥2：至少一页 + 提交 head）。
    pub max_steps: u32,
    /// 单次计划允许跨越的 epoch 数（≥1）。
    pub max_epoch_jumps: u64,
}

impl CatchupPolicy {
    /// 默认策略。
    pub const DEFAULT: Self = Self {
        page_items: DEFAULT_CATCHUP_PAGE_ITEMS,
        max_entries: DEFAULT_MAX_CATCHUP_ENTRIES,
        max_steps: DEFAULT_MAX_CATCHUP_STEPS,
        max_epoch_jumps: DEFAULT_MAX_EPOCH_JUMPS,
    };

    /// 有界校验：越界参数显式拒绝。
    pub const fn validate(self) -> Result<()> {
        if self.page_items == 0 || self.page_items > MAX_CATCHUP_PAGE_ITEMS {
            return Err(invalid_target("catch-up page items must be within 1..=500"));
        }
        if self.max_entries < self.page_items as u64
            || self.max_entries > MAX_CATCHUP_ENTRIES_LIMIT
        {
            return Err(invalid_target(
                "catch-up max entries must be within page items..=100000",
            ));
        }
        if self.max_steps < 2 || self.max_steps > MAX_CATCHUP_STEPS_LIMIT {
            return Err(invalid_target("catch-up max steps must be within 2..=256"));
        }
        if self.max_epoch_jumps == 0 || self.max_epoch_jumps > MAX_EPOCH_JUMPS_LIMIT {
            return Err(invalid_target(
                "catch-up max epoch jumps must be within 1..=1024",
            ));
        }
        Ok(())
    }
}

/// 必须等待的控制结果原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AwaitReason {
    /// 节点 head 声称的控制状态/epoch 没有任何新日志条目承载：不得猜测重建。
    MissingEpochCommit,
    /// 本地已提交更新的控制结果，节点 head 还没包含：不回退本地策略。
    PendingControlResult,
}

/// 追赶步骤（按序执行）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatchupStep {
    /// 拉取并逐条应用日志范围 `[from, to]`（含端点，条数 ≤ `page_items`）。
    PullLog {
        /// 起始序号（含）。
        from: u64,
        /// 结束序号（含）。
        to: u64,
    },
    /// 应用控制条目后重建/推进 MLS epoch；水位锚定在本次拉取末端。
    RebuildEpoch {
        /// 允许重建的水位序号（本次拉取末端）。
        at_sequence: u64,
        /// 重建前 epoch。
        from_epoch: u64,
        /// 重建后 epoch（每次只推进 1，不跳级）。
        to_epoch: u64,
    },
    /// 必须等待控制结果可达/被节点接受；本轮不推进该群。
    AwaitControl {
        /// 期望的控制头。
        expected_control_head: ControlHead,
        /// 期望的策略版本。
        expected_policy_version: u64,
        /// 等待原因。
        reason: AwaitReason,
    },
    /// 应用完成后把已验证 head 持久化为节点签名 head 的值。
    CommitHead {
        /// 已验证日志序号。
        log_sequence: u64,
        /// 已验证 epoch。
        mls_epoch: u64,
    },
}

/// 追赶计划。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatchupPlan {
    /// 群。
    pub group_id: GroupId,
    /// 按序执行的步骤；空表示已经追平。
    pub steps: Vec<CatchupStep>,
    /// 计划目标日志序号。
    pub target_log_sequence: u64,
    /// 计划目标 epoch。
    pub target_mls_epoch: u64,
    /// 计划目标策略版本。
    pub target_policy_version: u64,
}

impl CatchupPlan {
    /// 是否已经完全追平（无任何步骤）。
    pub fn is_caught_up(&self) -> bool {
        self.steps.is_empty()
    }

    /// 步数。
    pub fn step_count(&self) -> usize {
        self.steps.len()
    }

    /// 需要等待控制结果时返回原因。
    pub fn awaiting(&self) -> Option<AwaitReason> {
        self.steps.iter().find_map(|step| match step {
            CatchupStep::AwaitControl { reason, .. } => Some(*reason),
            _ => None,
        })
    }

    /// 计划要拉取的条目总数。
    pub fn entries_to_pull(&self) -> u64 {
        self.steps
            .iter()
            .filter_map(|step| match step {
                CatchupStep::PullLog { from, to } => {
                    Some(to.saturating_sub(*from).saturating_add(1))
                }
                _ => None,
            })
            .fold(0, u64::saturating_add)
    }

    /// 计划要重建的目标 epoch 序列（升序）。
    pub fn rebuild_targets(&self) -> Vec<u64> {
        self.steps
            .iter()
            .filter_map(|step| match step {
                CatchupStep::RebuildEpoch { to_epoch, .. } => Some(*to_epoch),
                _ => None,
            })
            .collect()
    }
}

/// SYNC-008 决策核心：生成本群日志追赶计划。
///
/// 顺序：策略/序号校验 → 冲突判定（本地越过节点 → `LOG_CONFLICT`）→ 本地策略更新 →
/// 等待控制结果 → 跨度/步数上限 → 分页 PullLog → 逐 epoch RebuildEpoch → CommitHead。
pub fn catchup_plan(
    local: VerifiedHead,
    node: NodeLogHead,
    policy: CatchupPolicy,
) -> Result<CatchupPlan> {
    policy.validate()?;
    if local.log_sequence > MAX_CURSOR_SEQ || node.log_sequence > MAX_CURSOR_SEQ {
        return Err(invalid_target("catch-up sequence exceeds the storable range"));
    }
    if local.mls_epoch > MAX_CURSOR_SEQ || node.mls_epoch > MAX_CURSOR_SEQ {
        return Err(invalid_target("catch-up epoch exceeds the storable range"));
    }
    // 冲突：本地已经验证到节点声称之外的日志/epoch。这不可能是“落后”，必须显式失败。
    if local.log_sequence > node.log_sequence {
        return Err(log_conflict("local log sequence is ahead of the node head"));
    }
    if local.mls_epoch > node.mls_epoch {
        return Err(log_conflict("local mls epoch is ahead of the node head"));
    }
    let mut plan = CatchupPlan {
        group_id: local.group_id,
        steps: Vec::new(),
        target_log_sequence: node.log_sequence,
        target_mls_epoch: node.mls_epoch,
        target_policy_version: node.policy_version,
    };
    // 本地策略更新（例如本机提交的控制结果节点还没接受）：只等待，不回退策略。
    if local.policy_version > node.policy_version {
        plan.steps.push(CatchupStep::AwaitControl {
            expected_control_head: node.control_head,
            expected_policy_version: local.policy_version,
            reason: AwaitReason::PendingControlResult,
        });
        return Ok(plan);
    }
    let span = node.log_sequence - local.log_sequence;
    let epoch_jump = node.mls_epoch - local.mls_epoch;
    // 已完全一致：幂等空计划，不需要再提交 head。
    let control_changed = local.control_head != node.control_head;
    let policy_changed = local.policy_version != node.policy_version;
    if span == 0 && epoch_jump == 0 && !control_changed && !policy_changed {
        return Ok(plan);
    }
    // 控制状态变了却没有新条目承载：不能猜，必须等待控制结果。
    if span == 0 {
        plan.steps.push(CatchupStep::AwaitControl {
            expected_control_head: node.control_head,
            expected_policy_version: node.policy_version,
            reason: AwaitReason::MissingEpochCommit,
        });
        return Ok(plan);
    }
    // 超限显式失败，不截断范围。
    if span > policy.max_entries {
        return Err(content_too_large(
            "catch-up span exceeds the per-plan entry limit",
        ));
    }
    if epoch_jump > policy.max_epoch_jumps {
        return Err(missing_log_entries(
            "catch-up epoch jump exceeds the per-plan limit",
        ));
    }
    // 分页拉取：从 local+1 到节点 head，每页 ≤ page_items。
    let mut from = local.log_sequence.saturating_add(1);
    while from <= node.log_sequence {
        let max_to = from.saturating_add(u64::from(policy.page_items) - 1);
        let to = node.log_sequence.min(max_to);
        plan.steps.push(CatchupStep::PullLog { from, to });
        if to == node.log_sequence {
            break;
        }
        from = to.saturating_add(1);
    }
    // 逐个 epoch 重建（不跳级）；锚点为本次拉取末端，逐条应用时可提前但不得更早。
    let mut epoch = local.mls_epoch;
    while epoch < node.mls_epoch {
        plan.steps.push(CatchupStep::RebuildEpoch {
            at_sequence: node.log_sequence,
            from_epoch: epoch,
            to_epoch: epoch.saturating_add(1),
        });
        epoch = epoch.saturating_add(1);
    }
    plan.steps.push(CatchupStep::CommitHead {
        log_sequence: node.log_sequence,
        mls_epoch: node.mls_epoch,
    });
    if plan.steps.len() > usize::try_from(policy.max_steps).unwrap_or(usize::MAX) {
        return Err(content_too_large(
            "catch-up plan exceeds the per-plan step limit",
        ));
    }
    let planned_entries = plan.entries_to_pull();
    if planned_entries > policy.max_entries {
        return Err(content_too_large(
            "catch-up plan exceeds the per-plan entry limit",
        ));
    }
    Ok(plan)
}