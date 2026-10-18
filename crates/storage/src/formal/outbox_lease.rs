//! STORAGE-014 / STORAGE-015：outbox 租约领取与结果标记。
//!
//! 设计约束：
//! - **原子租约**：领取在单笔短写事务内完成，每条任务用 `lease_version` 做 CAS
//!   （`UPDATE ... WHERE job_id = ? AND lease_version = ?`），受影响行数不为 1 即显式失败。
//!   领取时 `lease_version + 1`，因此旧 worker 手里的版本必然过期，不能覆盖新租约。
//! - **只领取到期任务**：`pending` 且 `retry_at <= now`，或 `leased` 且
//!   `lease_until <= now`；`cancel_requested = 1` 与依赖未完成的（`depends_on` 尚未
//!   `complete`）任务一律不领取。
//! - **有界批量**：条数上限 + 载荷字节上限（按 attempt 密文 / 草稿密文的实际长度累计）。
//! - **三态结果**：success / failed / unknown。`unknown` 绝不当作成功：任务保持
//!   `leased`、保留 owner 与版本、把租约延长到对账窗口，attempt 置 `unknown`，
//!   在窗口内不会被其它 worker 重领。
//! - **幂等与冲突**：同一租约的重复结果按“结果类别”幂等返回（不改行、不提交），
//!   结果类别冲突返回 `INVALID_STATE_TRANSITION`，旧版本/别的持有者返回
//!   `JOB_LEASE_CONFLICT`。
//!
//! 限制（docs/implementation/storage.md 同步记录）：`unknown` 只延长对账窗口，
//! 真正的对账（按 SubmissionId 查询节点接受状态）属于同步/节点模块，未在本函数实现；
//! 结果诊断不落盘（P0 schema 无 outbox 诊断列，且不新增迁移），只随 `JobState` 返回。

use rusqlite::{Connection, OptionalExtension, params};

use super::error::StorageError;
use super::{
    CoreTransaction, Result, Store, WriteScope, commit_transaction, ensure_id, now_ms,
    rollback_transaction,
};

/// 单次领取的任务条数上限。
pub const MAX_CLAIM_BATCH: u32 = 32;
/// 单次领取的载荷字节上限（按 attempt/草稿密文长度累计）。
pub const MAX_CLAIM_BATCH_BYTES: usize = 4 * 1024 * 1024;
/// 正常租约时长：30 秒。worker 崩溃后到期即可被重新领取。
pub const OUTBOX_LEASE_MS: i64 = 30_000;
/// 可重试失败的基础退避：5 秒（按 retry_count 指数增长）。
pub const OUTBOX_RETRY_BASE_MS: i64 = 5_000;
/// 退避上限：5 分钟。
pub const OUTBOX_RETRY_MAX_MS: i64 = 300_000;
/// 结果不确定（unknown）的对账窗口：6 小时。窗口内不重领，留给对账。
pub const OUTBOX_UNKNOWN_RECONCILE_MS: i64 = 6 * 60 * 60 * 1000;
/// 结果诊断字符上限（诊断不落盘，只随返回的 JobState 交给上层日志）。
pub const MAX_DIAGNOSTIC_CHARS: usize = 200;

/// 被领取的 outbox 任务：附当前租约版本与持有者。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeasedJob {
    /// 任务 ID。
    pub job_id: String,
    /// 去重键。
    pub dedup_key: String,
    /// 任务种类（prepare / send）。
    pub kind: String,
    /// 载荷引用（prepare 为 draft_id，send 为 SubmissionId）。
    pub payload_ref: String,
    /// 关联会话。
    pub conversation_id: Option<String>,
    /// 依赖任务。
    pub depends_on: Option<String>,
    /// 已重试次数。
    pub retry_count: u64,
    /// 下次可尝试时间（领取前的值）。
    pub retry_at: i64,
    /// 租约持有者。
    pub worker: String,
    /// 租约到期时间。
    pub lease_until: i64,
    /// 本次租约版本；旧 worker 不能用更小版本更新结果。
    pub lease_version: u64,
}

/// 任务结果三态（附证据种类）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobResult {
    /// 传输层确认提交成功（**不代表**节点正式接受）。
    TransportAccepted {
        /// 不含正文的诊断。
        detail: String,
    },
    /// 信箱 ACK（不升级为正式接受或送达）。
    MailboxAck {
        /// 不含正文的诊断。
        detail: String,
    },
    /// 节点正式接受。
    FormallyAccepted {
        /// 不含正文的诊断。
        detail: String,
    },
    /// 送达回执。
    Delivered {
        /// 不含正文的诊断。
        detail: String,
    },
    /// 明确失败；`retryable` 决定重排还是终态失败。
    Failed {
        /// 是否可重试。
        retryable: bool,
        /// 不含正文的诊断。
        detail: String,
    },
    /// 结果不确定（超时/断连/部分写入）：保留可对账状态，绝不当作成功。
    Unknown {
        /// 不含正文的诊断。
        detail: String,
    },
}

/// 结果类别（幂等与冲突判定只看类别）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResultClass {
    Success,
    Failure,
    Unknown,
}

impl ResultClass {
    const fn label(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failed",
            Self::Unknown => "unknown",
        }
    }
}

impl JobResult {
    /// 结果诊断（不含正文；落盘/日志前仍需截断）。
    pub fn detail(&self) -> &str {
        match self {
            Self::TransportAccepted { detail }
            | Self::MailboxAck { detail }
            | Self::FormallyAccepted { detail }
            | Self::Delivered { detail }
            | Self::Failed { detail, .. }
            | Self::Unknown { detail } => detail,
        }
    }

    pub(crate) fn class(&self) -> ResultClass {
        match self {
            Self::TransportAccepted { .. }
            | Self::MailboxAck { .. }
            | Self::FormallyAccepted { .. }
            | Self::Delivered { .. } => ResultClass::Success,
            Self::Failed { .. } => ResultClass::Failure,
            Self::Unknown { .. } => ResultClass::Unknown,
        }
    }
}

/// 结果落库后的任务去向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobOutcome {
    /// 任务完成（成功类别）。
    Completed,
    /// 可重试失败：已重排，等到 `retry_at`。
    RetryScheduled,
    /// 终态失败。
    Failed,
    /// 结果不确定：保留对账窗口内的租约。
    Unknown,
}

/// 标记结果后返回的任务状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobState {
    /// 任务 ID。
    pub job_id: String,
    /// 数据库中的任务状态。
    pub state: String,
    /// 结果去向。
    pub outcome: JobOutcome,
    /// 当前租约版本。
    pub lease_version: u64,
    /// 已重试次数。
    pub retry_count: u64,
    /// 下次可尝试时间。
    pub retry_at: i64,
    /// 当前租约到期时间（`unknown` 时为对账窗口结束时间）。
    pub lease_until: Option<i64>,
    /// 已截断的诊断（不落盘）。
    pub diagnostic: String,
    /// 关联 attempt 的最新状态（prepare 任务为 `None`）。
    pub attempt_state: Option<String>,
    /// 本次调用是否只是重复确认既有结果（未改任何行）。
    pub idempotent: bool,
}

struct JobRow {
    job_id: String,
    kind: String,
    payload_ref: String,
    state: String,
    retry_count: i64,
    retry_at: i64,
    lease_owner: Option<String>,
    lease_until: Option<i64>,
    lease_version: u64,
}

fn busy_or(error: StorageError) -> StorageError {
    match error {
        StorageError::Locked(reason) => StorageError::Busy(reason),
        other => other,
    }
}

fn bounded_detail(detail: &str) -> String {
    detail
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_DIAGNOSTIC_CHARS)
        .collect()
}

/// 第 N 次重试的退避：base × 2^(N-1)，上限 5 分钟。
fn retry_delay_ms(retry_count: i64) -> i64 {
    let exponent = u32::try_from(retry_count.saturating_sub(1).clamp(0, 6)).unwrap_or(0);
    let factor = 1i64.checked_shl(exponent).unwrap_or(OUTBOX_RETRY_MAX_MS);
    OUTBOX_RETRY_BASE_MS
        .saturating_mul(factor)
        .min(OUTBOX_RETRY_MAX_MS)
}

fn load_job(connection: &Connection, job_id: &str) -> Result<Option<JobRow>> {
    let row: Option<(String, String, String, String, i64, i64, Option<String>, Option<i64>, u64)> =
        connection
            .query_row(
                "SELECT job_id, kind, payload_ref, state, retry_count, retry_at,
                        lease_owner, lease_until, lease_version
                   FROM outbox_jobs WHERE job_id = ?1",
                [job_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ))
                },
            )
            .optional()?;
    Ok(row.map(
        |(
            job_id,
            kind,
            payload_ref,
            state,
            retry_count,
            retry_at,
            lease_owner,
            lease_until,
            lease_version,
        )| JobRow {
            job_id,
            kind,
            payload_ref,
            state,
            retry_count,
            retry_at,
            lease_owner,
            lease_until,
            lease_version,
        },
    ))
}

fn load_attempt_state(connection: &Connection, job: &JobRow) -> Result<Option<String>> {
    if job.kind != "send" {
        return Ok(None);
    }
    let state: Option<String> = connection
        .query_row(
            "SELECT state FROM message_attempts WHERE submission_id = ?1",
            [&job.payload_ref],
            |row| row.get(0),
        )
        .optional()?;
    Ok(state)
}

fn is_attempt_downgrade(current: &str, target: &str) -> bool {
    match current {
        "accepted" => target != "accepted",
        "rejected" => target != "rejected",
        _ => false,
    }
}

fn update_attempt(
    transaction: &mut CoreTransaction<'_>,
    job: &JobRow,
    target: &str,
) -> Result<()> {
    if job.kind != "send" {
        return Ok(());
    }
    let current: Option<String> = transaction
        .connection()
        .query_row(
            "SELECT state FROM message_attempts WHERE submission_id = ?1",
            [&job.payload_ref],
            |row| row.get(0),
        )
        .optional()?;
    let Some(current) = current else {
        return Err(StorageError::StateCorrupted(format!(
            "outbox job {} references a missing attempt",
            job.job_id
        )));
    };
    if current == target {
        return Ok(());
    }
    if is_attempt_downgrade(&current, target) {
        return Err(StorageError::InvalidStateTransition(format!(
            "attempt {} is {current} and cannot move back to {target}",
            job.payload_ref
        )));
    }
    let updated = transaction.connection().execute(
        "UPDATE message_attempts SET state = ?2 WHERE submission_id = ?1",
        params![job.payload_ref, target],
    )?;
    if updated != 1 {
        return Err(StorageError::StateCorrupted(format!(
            "attempt {} disappeared while recording a job result",
            job.payload_ref
        )));
    }
    transaction.note(1);
    Ok(())
}

impl Store {
    /// STORAGE-014：原子领取到期任务。
    ///
    /// `now_unix_ms` 由调用方注入（保存与时间相关的判定不依赖隐藏时钟）；
    /// `worker` 是本次领取的持有者标识，写入 `lease_owner`。
    pub fn claim_outbox_batch(
        &mut self,
        now_unix_ms: i64,
        limit: u32,
        worker: &str,
    ) -> Result<Vec<LeasedJob>> {
        self.ensure_ready()?;
        ensure_id("worker", worker)?;
        if !(1..=MAX_CLAIM_BATCH).contains(&limit) {
            return Err(StorageError::InvalidInput(format!(
                "claim batch limit must be within 1..={MAX_CLAIM_BATCH}"
            )));
        }
        if now_unix_ms < 0 {
            return Err(StorageError::InvalidInput(
                "now must be a non-negative Unix millisecond timestamp".to_owned(),
            ));
        }
        let mut transaction = self.begin_transaction(WriteScope::outbox()).map_err(busy_or)?;
        match claim_inner(&mut transaction, now_unix_ms, limit, worker) {
            Ok(jobs) => {
                commit_transaction(transaction)?;
                Ok(jobs)
            }
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }

    /// STORAGE-015：按租约记录结果（生产入口，使用系统 UTC 时钟排重试）。
    pub fn mark_outbox_result(&mut self, lease: &LeasedJob, result: JobResult) -> Result<JobState> {
        self.mark_outbox_result_at(lease, result, now_ms())
    }

    /// STORAGE-015（注入时间戳）：重试排期与对账窗口使用调用方提供的时间。
    pub fn mark_outbox_result_at(
        &mut self,
        lease: &LeasedJob,
        result: JobResult,
        now_unix_ms: i64,
    ) -> Result<JobState> {
        self.ensure_ready()?;
        ensure_id("job", &lease.job_id)?;
        ensure_id("worker", &lease.worker)?;
        if now_unix_ms < 0 {
            return Err(StorageError::InvalidInput(
                "now must be a non-negative Unix millisecond timestamp".to_owned(),
            ));
        }
        let mut transaction = self.begin_transaction(WriteScope::outbox()).map_err(busy_or)?;
        match mark_inner(&mut transaction, lease, &result, now_unix_ms) {
            Ok((state, wrote)) => {
                if wrote {
                    commit_transaction(transaction)?;
                } else {
                    rollback_transaction(transaction)?;
                }
                Ok(state)
            }
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }
}

struct ClaimCandidate {
    job_id: String,
    dedup_key: String,
    kind: String,
    payload_ref: String,
    conversation_id: Option<String>,
    depends_on: Option<String>,
    retry_count: i64,
    retry_at: i64,
    lease_version: u64,
    payload_bytes: Option<i64>,
}

const CANDIDATE_SQL: &str = "
    SELECT j.job_id, j.dedup_key, j.kind, j.payload_ref, j.conversation_id, j.depends_on,
           j.retry_count, j.retry_at, j.lease_version,
           CASE j.kind
             WHEN 'send' THEN (SELECT LENGTH(a.immutable_ciphertext) FROM message_attempts a
                                WHERE a.submission_id = j.payload_ref)
             ELSE (SELECT LENGTH(d.protected_content) FROM drafts d
                    WHERE d.draft_id = j.payload_ref)
           END AS payload_bytes
      FROM outbox_jobs j
     WHERE j.state IN ('pending', 'leased')
       AND j.cancel_requested = 0
       AND ((j.state = 'pending' AND j.retry_at <= ?1)
            OR (j.state = 'leased' AND j.lease_until IS NOT NULL AND j.lease_until <= ?1))
       AND (j.depends_on IS NULL
            OR EXISTS (SELECT 1 FROM outbox_jobs p
                        WHERE p.job_id = j.depends_on AND p.state = 'complete'))
     ORDER BY j.retry_at ASC, j.job_id ASC
     LIMIT ?2";

fn claim_inner(
    transaction: &mut CoreTransaction<'_>,
    now_unix_ms: i64,
    limit: u32,
    worker: &str,
) -> Result<Vec<LeasedJob>> {
    let candidates = {
        let connection = transaction.connection();
        let mut statement = connection.prepare(CANDIDATE_SQL)?;
        let rows = statement.query_map(params![now_unix_ms, i64::from(limit)], |row| {
            Ok(ClaimCandidate {
                job_id: row.get(0)?,
                dedup_key: row.get(1)?,
                kind: row.get(2)?,
                payload_ref: row.get(3)?,
                conversation_id: row.get(4)?,
                depends_on: row.get(5)?,
                retry_count: row.get(6)?,
                retry_at: row.get(7)?,
                lease_version: row.get(8)?,
                payload_bytes: row.get(9)?,
            })
        })?;
        let mut collected = Vec::new();
        for row in rows {
            collected.push(row?);
        }
        collected
    };

    let lease_until = now_unix_ms.saturating_add(OUTBOX_LEASE_MS);
    let mut leased = Vec::new();
    let mut total_bytes = 0usize;
    for candidate in candidates {
        let payload_bytes = candidate.payload_bytes.ok_or_else(|| {
            StorageError::StateCorrupted(format!(
                "outbox job {} has no readable payload for kind {}",
                candidate.job_id, candidate.kind
            ))
        })?;
        let payload_bytes = usize::try_from(payload_bytes.max(0)).unwrap_or(usize::MAX);
        // 字节预算：超界即停止领取（单条任务自身超界时仍领取，避免永久饥饿）。
        if !leased.is_empty() && total_bytes.saturating_add(payload_bytes) > MAX_CLAIM_BATCH_BYTES {
            break;
        }
        let updated = transaction.connection().execute(
            "UPDATE outbox_jobs
                SET state = 'leased', lease_owner = ?3, lease_until = ?4,
                    lease_version = lease_version + 1
              WHERE job_id = ?1
                AND lease_version = ?2
                AND cancel_requested = 0
                AND ((state = 'pending' AND retry_at <= ?5)
                     OR (state = 'leased' AND lease_until IS NOT NULL AND lease_until <= ?5))",
            params![
                candidate.job_id,
                candidate.lease_version,
                worker,
                lease_until,
                now_unix_ms
            ],
        )?;
        if updated != 1 {
            return Err(StorageError::JobLeaseConflict(format!(
                "outbox job {} changed while claiming the lease",
                candidate.job_id
            )));
        }
        transaction.note(1);
        total_bytes = total_bytes.saturating_add(payload_bytes);
        leased.push(LeasedJob {
            job_id: candidate.job_id,
            dedup_key: candidate.dedup_key,
            kind: candidate.kind,
            payload_ref: candidate.payload_ref,
            conversation_id: candidate.conversation_id,
            depends_on: candidate.depends_on,
            retry_count: u64::try_from(candidate.retry_count.max(0)).unwrap_or(0),
            retry_at: candidate.retry_at,
            worker: worker.to_owned(),
            lease_until,
            lease_version: candidate.lease_version.saturating_add(1),
        });
    }
    Ok(leased)
}

fn mark_inner(
    transaction: &mut CoreTransaction<'_>,
    lease: &LeasedJob,
    result: &JobResult,
    now_unix_ms: i64,
) -> Result<(JobState, bool)> {
    let job = load_job(transaction.connection(), &lease.job_id)?.ok_or_else(|| {
        StorageError::JobLeaseConflict(format!("outbox job {} does not exist", lease.job_id))
    })?;
    let diagnostic = bounded_detail(result.detail());

    if job.state == "leased" {
        let owned = job.lease_owner.as_deref() == Some(lease.worker.as_str());
        if !owned || job.lease_version != lease.lease_version {
            return Err(StorageError::JobLeaseConflict(format!(
                "outbox job {} is leased to {} at version {} (caller holds version {})",
                job.job_id,
                job.lease_owner.as_deref().unwrap_or("<none>"),
                job.lease_version,
                lease.lease_version
            )));
        }
        let (new_state, retry_count, retry_at, lease_until, outcome) = match result {
            JobResult::TransportAccepted { .. } | JobResult::MailboxAck { .. } => {
                update_attempt(transaction, &job, "submitted")?;
                (
                    "complete",
                    job.retry_count,
                    job.retry_at,
                    None,
                    JobOutcome::Completed,
                )
            }
            JobResult::FormallyAccepted { .. } | JobResult::Delivered { .. } => {
                update_attempt(transaction, &job, "accepted")?;
                (
                    "complete",
                    job.retry_count,
                    job.retry_at,
                    None,
                    JobOutcome::Completed,
                )
            }
            JobResult::Failed {
                retryable: true, ..
            } => {
                let retry_count = job.retry_count.saturating_add(1);
                let retry_at = now_unix_ms.saturating_add(retry_delay_ms(retry_count));
                (
                    "pending",
                    retry_count,
                    retry_at,
                    None,
                    JobOutcome::RetryScheduled,
                )
            }
            JobResult::Failed {
                retryable: false, ..
            } => {
                update_attempt(transaction, &job, "rejected")?;
                let retry_count = job.retry_count.saturating_add(1);
                (
                    "failed",
                    retry_count,
                    now_unix_ms,
                    None,
                    JobOutcome::Failed,
                )
            }
            JobResult::Unknown { .. } => {
                // 结果不确定：保留 owner 与租约版本，把租约延长到对账窗口；
                // 绝不落成 complete，也不删除投递任务。
                update_attempt(transaction, &job, "unknown")?;
                (
                    "leased",
                    job.retry_count,
                    job.retry_at,
                    Some(now_unix_ms.saturating_add(OUTBOX_UNKNOWN_RECONCILE_MS)),
                    JobOutcome::Unknown,
                )
            }
        };
        let updated = if new_state == "leased" {
            transaction.connection().execute(
                "UPDATE outbox_jobs SET lease_until = ?2
                  WHERE job_id = ?1 AND state = 'leased'
                    AND lease_owner = ?3 AND lease_version = ?4",
                params![job.job_id, lease_until, lease.worker, lease.lease_version],
            )?
        } else {
            transaction.connection().execute(
                "UPDATE outbox_jobs
                    SET state = ?2, lease_owner = NULL, lease_until = NULL,
                        retry_count = ?3, retry_at = ?4
                  WHERE job_id = ?1 AND state = 'leased'
                    AND lease_owner = ?5 AND lease_version = ?6",
                params![
                    job.job_id,
                    new_state,
                    retry_count,
                    retry_at,
                    lease.worker,
                    lease.lease_version
                ],
            )?
        };
        if updated != 1 {
            return Err(StorageError::JobLeaseConflict(format!(
                "outbox job {} lost its lease while recording the result",
                job.job_id
            )));
        }
        transaction.note(1);
        let attempt_state = load_attempt_state(transaction.connection(), &job)?;
        return Ok((
            JobState {
                job_id: job.job_id,
                state: new_state.to_owned(),
                outcome,
                lease_version: job.lease_version,
                retry_count: u64::try_from(retry_count.max(0)).unwrap_or(0),
                retry_at,
                lease_until,
                diagnostic,
                attempt_state,
                idempotent: false,
            },
            true,
        ));
    }

    // 任务不在 leased：只允许同一租约版本的重复结果（幂等），其余都是显式冲突。
    if job.state == "cancelled" {
        return Err(StorageError::InvalidStateTransition(format!(
            "outbox job {} was cancelled",
            job.job_id
        )));
    }
    if job.lease_version != lease.lease_version {
        return Err(StorageError::JobLeaseConflict(format!(
            "stale lease version {} for outbox job {} (current {})",
            lease.lease_version, job.job_id, job.lease_version
        )));
    }
    let current_class = match job.state.as_str() {
        "complete" => ResultClass::Success,
        "pending" | "failed" => ResultClass::Failure,
        other => {
            return Err(StorageError::InvalidStateTransition(format!(
                "outbox job {} is in state {other} and cannot record a result",
                job.job_id
            )));
        }
    };
    if current_class != result.class() {
        return Err(StorageError::InvalidStateTransition(format!(
            "outbox job {} already recorded a {} result and cannot record {}",
            job.job_id,
            current_class.label(),
            result.class().label()
        )));
    }
    let outcome = match job.state.as_str() {
        "complete" => JobOutcome::Completed,
        "pending" => JobOutcome::RetryScheduled,
        _ => JobOutcome::Failed,
    };
    let attempt_state = load_attempt_state(transaction.connection(), &job)?;
    Ok((
        JobState {
            job_id: job.job_id,
            state: job.state,
            outcome,
            lease_version: job.lease_version,
            retry_count: u64::try_from(job.retry_count.max(0)).unwrap_or(0),
            retry_at: job.retry_at,
            lease_until: job.lease_until,
            diagnostic,
            attempt_state,
            idempotent: true,
        },
        false,
    ))
}
