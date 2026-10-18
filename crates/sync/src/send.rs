//! SYNC-003 `prepare_draft_job` / SYNC-004 `submit_attempt_job` 的调度状态机。
//!
//! 本模块只做**调度决策**，不加密、不写数据库、不发网络：持久化与加密由 core 在
//! 事务里执行（T2 + `persist_prepared_attempt` + `claim_outbox_batch`/`mark_outbox_result`，
//! 见 storage.md）。决策与 storage 的 outbox 租约语义严格一致：
//!
//! - **同草稿同 SubmissionId**：[`SubmissionId`] 由
//!   `(draft_id, mls_epoch, attempt_index)` 确定性派生，因此进程重启、事务重试或
//!   二次调度都得到同一个 ID，不会触发第二次加密（[`SubmissionPlan::Prepare`]
//!   只会出现一次）。
//! - **unknown 只对账**：结果未知时返回 [`SubmissionPlan::QueryExisting`]——
//!   既不重发也不重新加密，即使租约已经过期；必须按原 SubmissionId 查询节点状态。
//! - **租约门**：尝试仍在租约期内（`lease_until_ms > now`）时返回
//!   [`SubmissionPlan::WaitForLease`]，禁止并发重发；租约过期或从未租用才允许
//!   [`SubmissionPlan::Resubmit`]（同 ID 同字节）。
//! - **成功才释放**：只有正式接受（[`AttemptState::Accepted`]）返回
//!   [`SubmissionPlan::Release`]；失败/未知都不会被当成成功。
//! - **重建**：明确拒绝后返回 [`SubmissionPlan::Rebuild`]，`attempt_index + 1`
//!   得到新的 SubmissionId；重建前必须先同步补齐（SYNC-008），本函数不猜新 epoch。
//! - 草稿已取消 → [`SubmissionPlan::Abandoned`]（不发送、不重建）。
//!
//! 所有冲突都显式失败：投影不一致 → `STATE_CORRUPTED`；调用方强行重发未知结果 →
//! `UNKNOWN_RESULT`（[`reject_unknown_resend`]）。

use chat_protocol::SubmissionId;

use crate::error::{Result, corrupted, invalid_target, unknown_result};
use crate::ids::{derive_submission_id, validate_id};

/// 允许的最大重建序号（有界拒绝）。
pub const MAX_ATTEMPT_INDEX: u32 = 64;

/// 草稿的正式状态（来自 storage `drafts.state`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftState {
    /// 已排队待发。
    Queued,
    /// 用户取消。
    Cancelled,
    /// 已正式接受。
    Accepted,
}

/// 尝试的正式状态（来自 storage `message_attempts.state`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptState {
    /// 已加密并耐久，尚未提交。
    Prepared,
    /// 已提交，等待正式结果。
    Submitted,
    /// 已正式接受。
    Accepted,
    /// 明确拒绝。
    Rejected,
    /// 结果未知：只能对账。
    Unknown,
}

/// 只读草稿投影。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftSnapshot {
    /// 草稿 ID（存储侧文本 ID）。
    pub draft_id: String,
    /// 草稿状态。
    pub state: DraftState,
    /// 加密时使用的 MLS epoch。
    pub mls_epoch: u64,
    /// 重建序号（0 = 首次尝试）。
    pub attempt_index: u32,
}

/// 只读尝试投影（含 outbox 租约字段，语义同 STORAGE-014/015）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptSnapshot {
    /// 提交 ID。
    pub submission_id: SubmissionId,
    /// 关联草稿。
    pub draft_id: String,
    /// 尝试状态。
    pub state: AttemptState,
    /// 加密时使用的 MLS epoch。
    pub mls_epoch: u64,
    /// 当前租约持有者（无租约为 `None`）。
    pub lease_owner: Option<String>,
    /// 租约到期时间（Unix 毫秒）。
    pub lease_until_ms: Option<i64>,
    /// 租约版本（旧 worker 不能更新新租约）。
    pub lease_version: u64,
}

/// 下一个调度动作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmissionPlan {
    /// 需要恰好一次加密 + 耐久持久化（新尝试）。
    Prepare {
        /// 草稿 ID。
        draft_id: String,
        /// 确定性派生的 SubmissionId（幂等键）。
        submission_id: SubmissionId,
        /// 加密使用的 epoch。
        mls_epoch: u64,
        /// 重建序号。
        attempt_index: u32,
    },
    /// 已有耐久尝试：同 SubmissionId、同字节重发。
    Resubmit {
        /// 原提交 ID。
        submission_id: SubmissionId,
        /// 当前租约版本。
        lease_version: u64,
        /// 之前是否已经提交过（用于诊断）。
        was_submitted: bool,
    },
    /// 结果未知：只按原 SubmissionId 查询，禁止重发与重新加密。
    QueryExisting {
        /// 原提交 ID。
        submission_id: SubmissionId,
    },
    /// 已正式接受：释放，不再提交。
    Release {
        /// 已接受的提交 ID。
        submission_id: SubmissionId,
    },
    /// 明确拒绝：先同步，再用新序号重建（新的 SubmissionId）。
    Rebuild {
        /// 草稿 ID。
        draft_id: String,
        /// 被拒绝的提交 ID。
        rejected_submission_id: SubmissionId,
        /// 新的提交 ID。
        submission_id: SubmissionId,
        /// 重建时使用的 epoch（同步后由调用方更新草稿投影）。
        mls_epoch: u64,
        /// 新的重建序号。
        attempt_index: u32,
    },
    /// 租约仍被持有：等待到期，禁止并发重发。
    WaitForLease {
        /// 提交 ID。
        submission_id: SubmissionId,
        /// 租约到期时间。
        lease_until_ms: i64,
        /// 是否由本 worker 持有（诊断用）。
        held_by_current_worker: bool,
    },
    /// 草稿已取消：不发送、不重建。
    Abandoned {
        /// 草稿 ID。
        draft_id: String,
    },
}

impl SubmissionPlan {
    /// 是否需要执行一次加密（只有 `Prepare` 为真）。
    pub const fn requires_encryption(&self) -> bool {
        matches!(self, Self::Prepare { .. })
    }

    /// 是否允许同字节重发（只有 `Resubmit` 为真）。
    pub const fn allows_resend(&self) -> bool {
        matches!(self, Self::Resubmit { .. })
    }

    /// 是否终态（释放或放弃）。
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Release { .. } | Self::Abandoned { .. })
    }

    /// 计划涉及的 SubmissionId。
    pub fn submission_id(&self) -> Option<SubmissionId> {
        match self {
            Self::Prepare { submission_id, .. }
            | Self::Resubmit { submission_id, .. }
            | Self::QueryExisting { submission_id }
            | Self::Release { submission_id }
            | Self::WaitForLease { submission_id, .. }
            | Self::Rebuild { submission_id, .. } => Some(*submission_id),
            Self::Abandoned { .. } => None,
        }
    }
}

/// 派生指定重建序号对应的 SubmissionId（同输入永远同 ID）。
pub fn derive_attempt_submission_id(
    draft_id: &str,
    mls_epoch: u64,
    attempt_index: u32,
) -> Result<SubmissionId> {
    if attempt_index > MAX_ATTEMPT_INDEX {
        return Err(invalid_target("attempt index exceeds the rebuild budget"));
    }
    derive_submission_id(draft_id, mls_epoch, attempt_index)
}

/// SYNC-003 / SYNC-004 的调度决策。
///
/// `worker` 是本次调度者标识；`now_unix_ms` 必须来自可信时钟。决策只依赖传入的
/// 只读投影，不读系统时间、不写数据库。
pub fn plan_submission(
    draft: &DraftSnapshot,
    attempt: Option<&AttemptSnapshot>,
    worker: &str,
    now_unix_ms: i64,
) -> Result<SubmissionPlan> {
    validate_id("draft", &draft.draft_id)?;
    validate_id("worker", worker)?;
    if now_unix_ms < 0 {
        return Err(invalid_target("submission clock must be non-negative"));
    }
    if draft.attempt_index > MAX_ATTEMPT_INDEX {
        return Err(invalid_target("attempt index exceeds the rebuild budget"));
    }

    let Some(attempt) = attempt else {
        if draft.state == DraftState::Accepted {
            return Err(corrupted(
                "draft is formally accepted but has no attempt to release",
            ));
        }
        if draft.state == DraftState::Cancelled {
            return Ok(SubmissionPlan::Abandoned {
                draft_id: draft.draft_id.clone(),
            });
        }
        return Ok(SubmissionPlan::Prepare {
            draft_id: draft.draft_id.clone(),
            submission_id: derive_attempt_submission_id(
                &draft.draft_id,
                draft.mls_epoch,
                draft.attempt_index,
            )?,
            mls_epoch: draft.mls_epoch,
            attempt_index: draft.attempt_index,
        });
    };

    if attempt.draft_id != draft.draft_id {
        return Err(corrupted("attempt belongs to a different draft"));
    }
    if let (Some(_), None) = (attempt.lease_until_ms, attempt.lease_owner.as_deref()) {
        return Err(corrupted("attempt lease has a deadline but no owner"));
    }
    if let Some(until_ms) = attempt.lease_until_ms
        && until_ms < 0
    {
        return Err(corrupted("attempt lease deadline is negative"));
    }

    match attempt.state {
        AttemptState::Accepted => Ok(SubmissionPlan::Release {
            submission_id: attempt.submission_id,
        }),
        AttemptState::Unknown => Ok(SubmissionPlan::QueryExisting {
            submission_id: attempt.submission_id,
        }),
        AttemptState::Rejected => {
            if draft.state == DraftState::Cancelled {
                return Ok(SubmissionPlan::Abandoned {
                    draft_id: draft.draft_id.clone(),
                });
            }
            let next_index = draft.attempt_index.saturating_add(1);
            if next_index > MAX_ATTEMPT_INDEX {
                return Err(invalid_target("attempt index exceeds the rebuild budget"));
            }
            let next_submission =
                derive_attempt_submission_id(&draft.draft_id, draft.mls_epoch, next_index)?;
            if next_submission == attempt.submission_id {
                return Err(corrupted(
                    "rebuilt attempt reuses the rejected SubmissionId",
                ));
            }
            Ok(SubmissionPlan::Rebuild {
                draft_id: draft.draft_id.clone(),
                rejected_submission_id: attempt.submission_id,
                submission_id: next_submission,
                mls_epoch: draft.mls_epoch,
                attempt_index: next_index,
            })
        }
        AttemptState::Prepared | AttemptState::Submitted => {
            if draft.state == DraftState::Cancelled {
                return Ok(SubmissionPlan::Abandoned {
                    draft_id: draft.draft_id.clone(),
                });
            }
            if draft.state == DraftState::Accepted {
                return Err(corrupted(
                    "draft is formally accepted but its attempt is not",
                ));
            }
            if let Some(until_ms) = attempt.lease_until_ms
                && until_ms > now_unix_ms
            {
                return Ok(SubmissionPlan::WaitForLease {
                    submission_id: attempt.submission_id,
                    lease_until_ms: until_ms,
                    held_by_current_worker: attempt.lease_owner.as_deref() == Some(worker),
                });
            }
            Ok(SubmissionPlan::Resubmit {
                submission_id: attempt.submission_id,
                lease_version: attempt.lease_version,
                was_submitted: attempt.state == AttemptState::Submitted,
            })
        }
    }
}

/// 显式拒绝把未知结果的尝试当成可重发。
///
/// 调用方若绕过 [`SubmissionPlan`] 自行重发，必须先经过这道检查：未知结果只允许按
/// 原 SubmissionId 对账，返回 `UNKNOWN_RESULT`（PROTOCOL-014）。
pub fn reject_unknown_resend(attempt: &AttemptSnapshot) -> Result<()> {
    if attempt.state == AttemptState::Unknown {
        return Err(unknown_result(
            "unknown submission must be reconciled by its SubmissionId, not resent",
        ));
    }
    Ok(())
}

const _: () = {
    assert!(MAX_ATTEMPT_INDEX > 0);
};
