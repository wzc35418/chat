//! SYNC-002：outbox 批执行器（把耐久待办任务真正推进到节点）。
//!
//! 设计约束：
//!
//! - **不新造状态机**：每个任务都交给既有 `SendOrchestrator`（`prepare` / `dispatch`），
//!   调度决策仍由 `chat_sync::send::plan_submission` 决定；本模块只负责“领取 → 执行 →
//!   按结果分类 → 完成/延后”。
//! - **一条失败不阻塞其它**：逐任务捕获结果与错误，错误按 PROTOCOL-014 的
//!   `ErrorCode::retryable()` 分成“延后（deferred）”与“终态拒绝（rejected）”，然后继续下一个任务。
//! - **绝不重发**：`unknown` 只写对账窗口（`JobResult::Unknown`），绝不重新提交、绝不重新加密；
//!   重复执行同一批是幂等的（任务已完成 → 不再被领取）。
//! - **不 sleep**：本函数只跑一圈；等待/退避由持久状态（`retry_at` / 租约窗口）表达，
//!   `stop` 只在任务边界检查（不做长阻塞）。
//! - **租约过期的任务由既有机制接管**：`claim_outbox_batch` 已经包含
//!   `leased AND lease_until <= now` 的到期重领，这里不额外实现。
//!
//! 已知限制（同步记录在 docs/implementation/sync.md）：
//!
//! - 批内顺序是**持久领取顺序**（`retry_at` 升序 + `job_id` 升序）。`retry_at` 的初始值就是草稿
//!   创建时间，因此它等价于“先创建先执行”；`LeasedJob` 不暴露 `created_at` 列。
//! - 标记结果自身失败（租约被别人抢走 / 存储冲突）按 `deferred` 计并继续下一个任务：
//!   不在这里重试写库（下一轮租约到期后会自然重新领取）。
//! - 本模块不负责“谁在什么时候调用它”（常驻循环/定时器）；调用方决定节奏。

use std::sync::atomic::{AtomicBool, Ordering};

use chat_protocol::ErrorCode;
use chat_storage::formal::{JobResult, LeasedJob, StorageError, Store};
use chat_sync::clock::Clock;
use chat_sync::SyncError;

use crate::send_orchestrator::{
    CryptoEngine, DispatchOutcome, NodeSubmitter, PrepareOutcome, SendOrchestrator, SendResult,
};

/// 单轮最多领取的任务数（与 STORAGE-014 的领取上限一致）。
pub const OUTBOX_BATCH_LIMIT: u32 = 32;

/// 停止原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboxStopReason {
    /// 调用方在任务边界请求停止（剩余任务留给下一轮）。
    StopRequested,
}

/// 一轮批执行的结果。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutboxBatchResult {
    /// 本轮真正尝试执行的任务数（不含被别人持有而没领到的）。
    pub processed: u32,
    /// 成功推进（任务完成；含幂等重放）。
    pub accepted: u32,
    /// 结果不确定：只标记对账窗口，**没有重发**。
    pub unknown: u32,
    /// 明确失败（不可重试）：任务置 `failed`。
    pub rejected: u32,
    /// 本轮无法推进：被别人持有，或可重试失败/等待租约（已按退避重排）。
    pub deferred: u32,
    /// 被 stop 打断时的原因。
    pub stopped_reason: Option<OutboxStopReason>,
}

/// 一个任务的处置结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Disposition {
    Accepted,
    Unknown,
    Rejected,
    Deferred,
}

impl OutboxBatchResult {
    fn record(&mut self, disposition: Disposition) {
        self.processed = self.processed.saturating_add(1);
        match disposition {
            Disposition::Accepted => self.accepted = self.accepted.saturating_add(1),
            Disposition::Unknown => self.unknown = self.unknown.saturating_add(1),
            Disposition::Rejected => self.rejected = self.rejected.saturating_add(1),
            Disposition::Deferred => self.deferred = self.deferred.saturating_add(1),
        }
    }
}

/// SYNC-002 落地：执行一轮 outbox（生产入口，时间取自编排器的时钟）。
///
/// `stop` 为真时在**任务边界**停止（不打断正在执行的那一个任务），并返回停止原因。
pub fn run_outbox_once<S: NodeSubmitter, C: CryptoEngine, K: Clock>(
    orchestrator: &mut SendOrchestrator<S, C, K>,
    stop: &AtomicBool,
) -> SendResult<OutboxBatchResult> {
    let now_unix_ms = orchestrator.clock_mut().now_unix_ms();
    run_outbox_once_at(orchestrator, stop, now_unix_ms, OUTBOX_BATCH_LIMIT)
}

/// 与 [`run_outbox_once`] 相同，但显式给定时钟与批量上限（测试/诊断入口）。
///
/// `now_unix_ms` 必须为正：非正值视为时钟故障（与发送编排层一致，不静默当 0）。
pub fn run_outbox_once_at<S: NodeSubmitter, C: CryptoEngine, K: Clock>(
    orchestrator: &mut SendOrchestrator<S, C, K>,
    stop: &AtomicBool,
    now_unix_ms: i64,
    limit: u32,
) -> SendResult<OutboxBatchResult> {
    if now_unix_ms <= 0 {
        return Err(SyncError::new(
            ErrorCode::InvalidTarget,
            "outbox worker clock must be positive",
        ));
    }
    let worker = orchestrator.worker().to_owned();
    let mut result = OutboxBatchResult::default();
    // 阶段 1（send）：**不预先领取**。发送任务由 @@dispatch@@ 自己按 CAS 领取并落库，
    // 这样发送计划（patch_submission）看到的是“没人持有”，不会把本 worker 的租约误当成
    // “别人持有”而原地等待；三态结果也由 @@record_result@@ 写库。
    for job in due_send_jobs(orchestrator.store(), now_unix_ms, limit) {
        if stop.load(Ordering::SeqCst) {
            result.stopped_reason = Some(OutboxStopReason::StopRequested);
            break;
        }
        result.record(execute_send(orchestrator, &job));
    }
    if result.stopped_reason.is_none() {
        // 阶段 2（prepare）：准备任务必须领取——成功时 T2 事务会把任务置 complete，
        // 失败/未知时用租约写结果（对账窗口 / 退避）。
        let claimed = orchestrator
            .store_mut()
            .claim_outbox_batch(now_unix_ms, limit, &worker)
            .map_err(storage_error)?;
        let mut claimed = claimed;
        // 稳定批内顺序：先创建先执行（retry_at 初始值就是草稿创建时间）。
        claimed.sort_by(|left, right| {
            (left.retry_at, &left.job_id).cmp(&(right.retry_at, &right.job_id))
        });
        let mut ours: Vec<String> = Vec::new();
        for job in &claimed {
            if stop.load(Ordering::SeqCst) {
                result.stopped_reason = Some(OutboxStopReason::StopRequested);
                break;
            }
            // 阶段 1 没推进的 send 任务被这里领到了：本 worker 持有租约会让 dispatch 等待，
            // 所以只做一次可重试退避，交回 pending 让下一轮重新尝试。
            let disposition = if job.kind == "send" {
                mark_retryable(orchestrator.store_mut(), job, now_unix_ms, || {
                    format!("send job deferred by the batch claim job={}", job.job_id)
                })
                .map(|()| Disposition::Deferred)
            } else {
                execute_prepare_job(orchestrator, job, now_unix_ms)
            };
            ours.push(job.job_id.clone());
            // 单个任务的存储错误不影响其它任务：按“本轮没有推进”计。
            result.record(disposition.unwrap_or(Disposition::Deferred));
        }
        // 被别人持有（未到期）的任务：本轮领不到，明确计入 deferred 而不是假装“没有工作”。
        let used = result
            .accepted
            .saturating_add(result.unknown)
            .saturating_add(result.rejected)
            .saturating_add(result.deferred);
        if used < limit {
            result.deferred = result.deferred.saturating_add(held_by_other_workers(
                orchestrator.store(),
                now_unix_ms,
                limit.saturating_sub(used),
                &ours,
            ));
        }
    }
    Ok(result)
}

/// 执行一个已领取的 **prepare** 任务（send 任务走阶段 1，不在这里领取）。
fn execute_prepare_job<S: NodeSubmitter, C: CryptoEngine, K: Clock>(
    orchestrator: &mut SendOrchestrator<S, C, K>,
    job: &LeasedJob,
    now_unix_ms: i64,
) -> SendResult<Disposition> {
    match job.kind.as_str() {
        "prepare" => execute_prepare(orchestrator, job, now_unix_ms),
        // 未知种类不猜：终态拒绝，避免每轮空转。
        other => {
            mark(
                orchestrator.store_mut(),
                job,
                JobResult::Failed {
                    retryable: false,
                    detail: format!("unsupported outbox job kind {other}"),
                },
                now_unix_ms,
            )?;
            Ok(Disposition::Rejected)
        }
    }
}

fn execute_prepare<S: NodeSubmitter, C: CryptoEngine, K: Clock>(
    orchestrator: &mut SendOrchestrator<S, C, K>,
    job: &LeasedJob,
    now_unix_ms: i64,
) -> SendResult<Disposition> {
    let draft_id = job.payload_ref.clone();
    match orchestrator.prepare(&draft_id) {
        Ok(PrepareOutcome::Prepared {
            submission_id,
            already_prepared,
            ..
        }) => {
            // 成功：T2 事务里已经把准备任务置 complete；这里再写一次是幂等的
            // （已完成 + 同类结果 = 不写行）。
            mark(
                orchestrator.store_mut(),
                job,
                JobResult::TransportAccepted {
                    detail: format!(
                        "prepare ok submission={submission_id} reused={already_prepared}"
                    ),
                },
                now_unix_ms,
            )?;
            Ok(Disposition::Accepted)
        }
        Ok(PrepareOutcome::AlreadyAccepted { submission_id }) => {
            mark(
                orchestrator.store_mut(),
                job,
                JobResult::TransportAccepted {
                    detail: format!("draft already accepted submission={submission_id}"),
                },
                now_unix_ms,
            )?;
            Ok(Disposition::Accepted)
        }
        Ok(PrepareOutcome::AwaitingReconciliation { submission_id }) => {
            // 未知结果：只写对账窗口，绝不重发/重新加密。
            mark(
                orchestrator.store_mut(),
                job,
                JobResult::Unknown {
                    detail: format!("awaiting reconciliation submission={submission_id}"),
                },
                now_unix_ms,
            )?;
            Ok(Disposition::Unknown)
        }
        Ok(PrepareOutcome::WaitingForLease { submission_id, .. }) => {
            mark_retryable(orchestrator.store_mut(), job, now_unix_ms, || {
                format!("send lease is held elsewhere submission={submission_id}")
            })?;
            Ok(Disposition::Deferred)
        }
        Ok(PrepareOutcome::Abandoned { draft_id }) => {
            mark(
                orchestrator.store_mut(),
                job,
                JobResult::Failed {
                    retryable: false,
                    detail: format!("draft cancelled draft={draft_id}"),
                },
                now_unix_ms,
            )?;
            Ok(Disposition::Rejected)
        }
        Err(error) => classify_error(orchestrator.store_mut(), job, &error, now_unix_ms),
    }
}

/// 一个**到期**的 send 任务投影（阶段 1 不需要先领取它）。
struct DueSendJob {
    job_id: String,
    payload_ref: String,
    retry_at: i64,
}

/// 列出本轮应该尝试的 send 任务：@@pending && retry_at <= now@@，或租约已过期。
///
/// 这里只做**只读枚举**；真正的原子领取由 @@dispatch@@ 内部的 CAS 完成（不在这里重复实现）。
fn due_send_jobs(store: &Store, now_unix_ms: i64, limit: u32) -> Vec<DueSendJob> {
    let Ok(jobs) = store.outbox_jobs() else {
        return Vec::new();
    };
    let mut due: Vec<DueSendJob> = jobs
        .into_iter()
        .filter(|job| job.kind == "send")
        .filter(|job| {
            (job.state == "pending" && job.retry_at <= now_unix_ms)
                || (job.state == "leased"
                    && job.lease_until.is_some_and(|until| until <= now_unix_ms))
        })
        .map(|job| DueSendJob {
            job_id: job.job_id,
            payload_ref: job.payload_ref,
            retry_at: job.retry_at,
        })
        .collect();
    due.sort_by(|left, right| (left.retry_at, &left.job_id).cmp(&(right.retry_at, &right.job_id)));
    due.truncate(limit as usize);
    due
}

/// 阶段 1：提交一条到期 send 任务（领取与结果落库都由 dispatch 完成）。
fn execute_send<S: NodeSubmitter, C: CryptoEngine, K: Clock>(
    orchestrator: &mut SendOrchestrator<S, C, K>,
    job: &DueSendJob,
) -> Disposition {
    let submission_id = job.payload_ref.clone();
    let attempt = match orchestrator.store().read_attempt(&submission_id) {
        Ok(attempt) => attempt,
        Err(_) => return Disposition::Deferred,
    };
    let Some(attempt) = attempt else {
        // 尝试缺失：这是状态不一致，但没有租约就不能写终态结果；下一轮由恢复流程处理。
        return Disposition::Rejected;
    };
    let draft_id = attempt.draft_id.clone();
    match orchestrator.dispatch(&draft_id) {
        // dispatch 内部已经按 JobResult 落库（completed/unknown/failed），不再重复写。
        Ok(DispatchOutcome::Accepted { .. }) => Disposition::Accepted,
        Ok(DispatchOutcome::Unknown { .. }) => Disposition::Unknown,
        Ok(DispatchOutcome::Rejected { .. }) => Disposition::Rejected,
        Ok(DispatchOutcome::WaitingForLease { .. }) => {
            // 别人持有有效租约：本轮不提交、不写库，等租约到期（下一轮自然重试）。
            Disposition::Deferred
        }
        Ok(DispatchOutcome::Abandoned { .. }) => Disposition::Rejected,
        // 错误不让本批停摆；retryable 决定“延后”还是“拒绝”（无租约可写，下一轮自然重试）。
        Err(error) => {
            if error.code().retryable() {
                Disposition::Deferred
            } else {
                Disposition::Rejected
            }
        }
    }
}
/// 错误分类：PROTOCOL-014 的 `retryable()` 决定“延后”还是“终态拒绝”。
fn classify_error(
    store: &mut Store,
    job: &LeasedJob,
    error: &SyncError,
    now_unix_ms: i64,
) -> SendResult<Disposition> {
    let code = error.code();
    if code.retryable() {
        mark_retryable(store, job, now_unix_ms, || format!("retryable failure {code:?}"))?;
        Ok(Disposition::Deferred)
    } else {
        mark(
            store,
            job,
            JobResult::Failed {
                retryable: false,
                detail: format!("terminal failure {code:?}"),
            },
            now_unix_ms,
        )?;
        Ok(Disposition::Rejected)
    }
}

fn mark_retryable(
    store: &mut Store,
    job: &LeasedJob,
    now_unix_ms: i64,
    detail: impl FnOnce() -> String,
) -> SendResult<()> {
    mark(
        store,
        job,
        JobResult::Failed {
            retryable: true,
            detail: detail(),
        },
        now_unix_ms,
    )
}

fn mark(
    store: &mut Store,
    job: &LeasedJob,
    result: JobResult,
    now_unix_ms: i64,
) -> SendResult<()> {
    store
        .mark_outbox_result_at(job, result, now_unix_ms)
        .map(|_| ())
        .map_err(storage_error)
}

/// 本批窗口内**被其它 worker 有效持有**的任务数（本轮领不到 → deferred）。
///
/// 只统计 `leased` 且租约未到期的行；到期行由 `claim_outbox_batch` 直接接管，不计入。
fn held_by_other_workers(
    store: &Store,
    now_unix_ms: i64,
    limit: u32,
    claimed: &[String],
) -> u32 {
    let Ok(jobs) = store.outbox_jobs() else {
        return 0;
    };
    let mut held = 0_u32;
    for job in jobs {
        if held >= limit {
            break;
        }
        // 本 worker 本轮刚领到的任务不算“被别人持有”。
        if claimed.iter().any(|job_id| job_id == &job.job_id) {
            continue;
        }
        if job.state == "leased" && job.lease_until.is_some_and(|until| until > now_unix_ms) {
            held = held.saturating_add(1);
        }
    }
    held
}

fn storage_error(error: StorageError) -> SyncError {
    let _ = error;
    SyncError::new(ErrorCode::StorageFailure, "outbox worker storage failure")
}
