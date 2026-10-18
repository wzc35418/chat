//! CORE-007 + SYNC-003/004 的发送编排切片（不引入真实网络）。
//!
//! 本模块把「意图 → 一次加密尝试 → 提交 → 结果落库」串成一条可测试的链路，
//! **不新造状态机**：所有调度决策都交给 `chat_sync::send::plan_submission`
//! （同草稿同 SubmissionId、unknown 只对账、租约门、拒绝后重建、成功才释放），
//! 持久化交给 storage 的既有事务函数（T1 `persist_send_intent`、T2
//! `persist_prepared_attempt`、STORAGE-014/015 的租约与结果语义）。
//!
//! 三个可注入端口：
//!
//! - [`CryptoEngine`]：为一条草稿真实加密一次（返回 MLS epoch 与密文）；
//!   编排层从不构造密文，测试里用真实 OpenMLS 设备（`chat_crypto::engine::MlsDevice`）。
//! - [`NodeSubmitter`]：提交不可变密文、按 SubmissionId 查询结果；只有
//!   `Accepted`/`Unknown`/`Rejected` 三态，绝不把未知当成功。
//! - [`chat_sync::clock::Clock`]：可注入时钟（测试用 `FixedClock`，生产用 `SystemClock`）。
//!
//! 三段入口：
//!
//! 1. [`SendOrchestrator::begin_send`]：单笔 T1 事务写草稿与准备任务，返回 `draft_id`；
//! 2. [`SendOrchestrator::prepare`]：按调度计划**恰好一次**加密并写 T2（新尝试），
//!    已有耐久尝试时直接复用（不重复加密）；结果未知时只提示对账；
//! 3. [`SendOrchestrator::dispatch`]：领取发送任务租约 → 调用节点端口 → 按
//!    `Accepted`/`Unknown`/`Rejected` 落库（正式接受=任务完成 + attempt accepted +
//!    草稿 accepted；未知=保留对账窗口、绝不重发；拒绝=attempt rejected，由下一次
//!    `prepare` 用新 SubmissionId 重建）。
//!
//! 已知限制（详见 docs/implementation/core.md 的「发送编排实现记录」）：
//!
//! - storage 只提供批量领取（`claim_outbox_batch`），没有按 SubmissionId 定向领取；
//!   本模块在 `WriteScope::outbox` 事务内用**同一 CAS 规则**定向领取，避免把其它
//!   到期任务误租。
//! - storage 没有按草稿枚举 attempt、也没有「把草稿置 accepted」的正式入口；本模块用
//!   只读连接做投影查询、在 intent 事务内更新 `drafts.state`，并在 `Release` 路径上
//!   自愈（先落 outbox 结果、再置草稿，失败时下一次 `dispatch` 会补齐）。
//! - MLS provider 记录变化（CRYPTO-005）未接线：`crypto_changes` 传空，加密引擎是
//!   会话级 OpenMLS 设备；真实 provider 持久化接入后应改为同事务写入。
//! - 权限校验（DOMAIN-001）、附件依赖、同步补齐（SYNC-008）不在本切片。

use chat_domain::{OperationState, RetryDecision, classify_retry};
use chat_protocol::{ConversationId, OperationId, SubmissionId};
use chat_storage::formal::{
    DraftRecord, JobOutcome, JobResult, LeasedJob, MessageContent, MessageOrigin, OUTBOX_LEASE_MS,
    PreparedWrite, StorageError, Store, ValidatedSendRequest, WriteScope, commit_transaction,
    materialize_session, persist_prepared_attempt, persist_send_intent, rollback_transaction,
};
use chat_sync::clock::Clock;
use chat_sync::send::{
    AttemptSnapshot, AttemptState, DraftSnapshot, DraftState, SubmissionPlan, plan_submission,
};
use chat_sync::{ErrorCode, SyncError};

/// 发送编排结果类型：错误码一律来自 PROTOCOL-014。
pub type SendResult<T> = core::result::Result<T, SyncError>;

fn storage_detail(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::StorageLocked => "storage.locked",
        ErrorCode::StateCorrupted => "state.corrupted",
        ErrorCode::InvalidStateTransition => "storage.conflict",
        ErrorCode::InvalidTarget => "storage.invalid_input",
        ErrorCode::InvalidCursor => "storage.invalid_cursor",
        ErrorCode::PermissionDenied => "domain.permission_denied",
        ErrorCode::QuotaExceeded => "storage.disk_full",
        _ => "storage.failure",
    }
}

/// storage 错误 → 冻结错误码（不新增码，不把损坏/冲突降级成成功）。
fn storage_failure(error: StorageError) -> SyncError {
    let code = match &error {
        StorageError::Locked(_) | StorageError::Busy(_) => ErrorCode::StorageLocked,
        StorageError::StateCorrupted(_) => ErrorCode::StateCorrupted,
        StorageError::MigrationFailed(_) | StorageError::SchemaTooNew { .. } => {
            ErrorCode::StorageFailure
        }
        StorageError::OperationIdConflict(_)
        | StorageError::SubmissionIdConflict(_)
        | StorageError::MessageIdConflict(_)
        | StorageError::VersionConflict { .. }
        | StorageError::JobLeaseConflict(_)
        | StorageError::InvalidStateTransition(_) => ErrorCode::InvalidStateTransition,
        StorageError::RecoveryRequired(_)
        | StorageError::RollbackFailed(_)
        | StorageError::Failure(_) => ErrorCode::StorageFailure,
        StorageError::DiskFull => ErrorCode::QuotaExceeded,
        StorageError::InvalidInput(_) => ErrorCode::InvalidTarget,
        StorageError::InvalidCursor(_) => ErrorCode::InvalidCursor,
        StorageError::PermissionDenied(_) => ErrorCode::PermissionDenied,
        // storage 仍在演进：新增的冲突类错误一律按“状态冲突/存储失败”显式失败，
        // 绝不因为未知变体而放行（fail-closed）。
        StorageError::IdentityAccountConflict(_) => ErrorCode::InvalidStateTransition,
        // MLS epoch 单调性被破坏：PROTOCOL-014 有 STALE_EPOCH，DOMAIN-010 归类为
        // “先同步再重建”，不能降级成普通失败或成功。
        StorageError::StaleEpoch { .. } => ErrorCode::StaleEpoch,
    };
    SyncError::new(code, storage_detail(code))
}

/// SQLite 错误 → 冻结错误码（类型由调用点推断，本 crate 不直接依赖 rusqlite）。
fn sql_failure<E: Into<StorageError>>(error: E) -> SyncError {
    storage_failure(error.into())
}

fn invalid_target(detail: &'static str) -> SyncError {
    SyncError::new(ErrorCode::InvalidTarget, detail)
}

fn invalid_id(detail: &'static str) -> SyncError {
    SyncError::new(ErrorCode::InvalidId, detail)
}

fn invalid_state(detail: &'static str) -> SyncError {
    SyncError::new(ErrorCode::InvalidStateTransition, detail)
}

fn corrupted(detail: &'static str) -> SyncError {
    SyncError::new(ErrorCode::StateCorrupted, detail)
}

/// 真实 MLS 密文与加密时使用的 epoch。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncryptedPayload {
    /// 加密使用的 MLS epoch。
    pub mls_epoch: u64,
    /// 不可变密文（只能由 [`CryptoEngine`] 产生）。
    pub ciphertext: Vec<u8>,
}

/// 加密端口：一条草稿只允许加密一次（同 SubmissionId 复用既有字节）。
pub trait CryptoEngine {
    /// 会话当前 MLS epoch（用于派生 SubmissionId）。
    fn epoch(&self, conversation_id: &str) -> SendResult<u64>;

    /// 为该草稿明文生成一次真实密文。
    fn encrypt(
        &mut self,
        conversation_id: &str,
        draft_id: &str,
        body: &str,
    ) -> SendResult<EncryptedPayload>;
}

/// 提交给节点的不可变尝试投影（密文来自 storage 的耐久字节，不由编排层重造）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmitAttempt {
    /// SubmissionId（PROTOCOL-001 文本）。
    pub submission_id: String,
    /// 会话 ID。
    pub conversation_id: String,
    /// 加密使用的 MLS epoch。
    pub mls_epoch: u64,
    /// 不可变密文。
    pub ciphertext: Vec<u8>,
}

/// 节点提交结果三态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// 节点正式接受（只有它能让草稿落 accepted）。
    Accepted {
        /// 不含正文的诊断。
        detail: String,
    },
    /// 结果未知（超时/断连/部分写入）：只允许按原 SubmissionId 对账。
    Unknown {
        /// 不含正文的诊断。
        detail: String,
    },
    /// 明确拒绝：由下一次 prepare 用新 SubmissionId 重建。
    Rejected {
        /// 不含正文的诊断。
        detail: String,
    },
}

/// 节点端口：提交密文尝试、按 SubmissionId 查询结果。
///
/// 实现方必须是**幂等**的：同 SubmissionId 同字节重复提交只能被接受一次；
/// 本编排层保证不会把未知结果当作可重发，也不会重新加密。
pub trait NodeSubmitter {
    /// 提交一次不可变尝试。
    fn submit(&self, attempt: &SubmitAttempt) -> SendResult<SubmitOutcome>;

    /// 按 SubmissionId 查询既有提交结果（结果未知时的唯一合法动作）。
    fn query(&self, submission_id: &str) -> SendResult<SubmitOutcome>;
}

/// `prepare` 的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrepareOutcome {
    /// 已有耐久尝试（可能是本次刚写入，也可能是幂等复用）。
    Prepared {
        /// SubmissionId（PROTOCOL-001 文本）。
        submission_id: String,
        /// 加密使用的 MLS epoch。
        mls_epoch: u64,
        /// 重建序号（0 = 首次尝试）。
        attempt_index: u32,
        /// 本次调用是否只是复用已存在的尝试（未重新加密）。
        already_prepared: bool,
    },
    /// 结果未知：只能按原 SubmissionId 对账，禁止重新加密。
    AwaitingReconciliation {
        /// 结果未知的 SubmissionId。
        submission_id: String,
    },
    /// 发送任务的有效租约被其它 worker 持有：等待到期，禁止并发加密/提交。
    WaitingForLease {
        /// 尝试 ID。
        submission_id: String,
        /// 租约到期时间（Unix 毫秒）。
        lease_until_ms: i64,
    },
    /// 草稿已正式接受：无需再准备。
    AlreadyAccepted {
        /// 已接受的 SubmissionId。
        submission_id: String,
    },
    /// 草稿已取消/终止：不发送、不重建。
    Abandoned {
        /// 草稿 ID。
        draft_id: String,
    },
}

/// `dispatch` 的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// 节点正式接受（草稿与 outbox 已落库）。
    Accepted {
        /// 已接受的 SubmissionId。
        submission_id: String,
        /// 本次调用是否只是幂等重放（未再次提交）。
        idempotent: bool,
    },
    /// 结果未知：保留对账窗口，绝不重发、绝不重新加密。
    Unknown {
        /// 结果未知的 SubmissionId。
        submission_id: String,
    },
    /// 明确拒绝：attempt 已置 rejected，下一次 prepare 会用新 SubmissionId 重建。
    Rejected {
        /// 被拒绝的 SubmissionId。
        submission_id: String,
    },
    /// 发送任务的有效租约/退避未到期：本次不提交。
    WaitingForLease {
        /// 尝试 ID。
        submission_id: String,
        /// 可以再次尝试的时间（Unix 毫秒）。
        until_ms: i64,
    },
    /// 草稿已取消/终止：不提交。
    Abandoned {
        /// 草稿 ID。
        draft_id: String,
    },
}

/// `cancel_draft` 的结果（CORE-009 三态）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CancelOutcome {
    /// 取消意图已持久化：草稿字节保留，后续 `dispatch` 不会再提交。
    CancelRequested {
        /// 草稿 ID。
        draft_id: String,
        /// 关联的最新 SubmissionId（尚无尝试时为 `None`）。
        submission_id: Option<String>,
    },
    /// 已经正式接受：不能撤回，消息也不会被删除。
    CancelTooLate {
        /// 草稿 ID。
        draft_id: String,
        /// 已接受的 SubmissionId。
        submission_id: String,
    },
    /// 结果未知：只记录取消意图，必须继续按 SubmissionId 对账（绝不乐观删除）。
    CancelUnknown {
        /// 草稿 ID。
        draft_id: String,
        /// 结果未知的 SubmissionId。
        submission_id: String,
    },
}

/// `reconcile` 的报告：只描述“本次是否修复/是否需要对账”，不做乐观推断。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// 涉及的最新 SubmissionId（尚无尝试时为 `None`）。
    pub submission_id: Option<String>,
    /// 本次调用是否把草稿补成 `accepted`（修复“attempt 已接受但草稿仍 queued”的窗口）。
    pub draft_repaired: bool,
    /// 结果未知：只能按 SubmissionId 对账，本次没有任何写操作。
    pub awaiting_reconciliation: bool,
}

impl ReconcileReport {
    /// 是否已经一致（无需修复、也不需要额外对账）。
    pub fn is_consistent(&self) -> bool {
        !self.draft_repaired && !self.awaiting_reconciliation
    }
}

/// 存储侧 attempt 行投影（按草稿枚举，用于推导重建序号）。
struct AttemptRow {
    submission_id: String,
    state: String,
    mls_epoch: u64,
}

/// 存储侧发送任务行投影。
struct SendJobRow {
    job_id: String,
    state: String,
    retry_at: i64,
    lease_owner: Option<String>,
    lease_until: Option<i64>,
    lease_version: u64,
}

/// 一次调度决策的输入与结论。
struct PlanState {
    draft: DraftRecord,
    attempts: Vec<AttemptRow>,
    plan: SubmissionPlan,
}

/// 定向领取租约的结果。
enum LeaseAccess {
    /// 已取得（或沿用自己的）租约，可以落结果。
    Leased(LeasedJob),
    /// 有效租约/退避未到期，本次不能落结果。
    Waiting { until_ms: i64 },
    /// 任务不在可领取状态（已完成/已失败）：状态不一致，显式失败。
    NotClaimable,
}

fn content_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(text) => text.clone(),
    }
}

fn draft_state(raw: &str) -> SendResult<DraftState> {
    match raw {
        "queued" | "unknown" => Ok(DraftState::Queued),
        "accepted" | "delivered" => Ok(DraftState::Accepted),
        "cancelled" | "failed" => Ok(DraftState::Cancelled),
        _ => Err(corrupted(
            "stored draft state is not a supported send state",
        )),
    }
}

fn attempt_state(raw: &str) -> SendResult<AttemptState> {
    match raw {
        "prepared" => Ok(AttemptState::Prepared),
        "submitted" => Ok(AttemptState::Submitted),
        "accepted" => Ok(AttemptState::Accepted),
        "rejected" => Ok(AttemptState::Rejected),
        "unknown" => Ok(AttemptState::Unknown),
        _ => Err(corrupted(
            "stored attempt state is not a supported send state",
        )),
    }
}

/// 重建序号：既有 attempt 数 - 1（首次尝试为 0），与 SYNC-003/004 的
/// `DraftSnapshot.attempt_index` 语义一致（Rebuild 用 `+1` 派生新 SubmissionId）。
fn new_attempt_index(attempts: usize) -> SendResult<u32> {
    let latest = attempts.saturating_sub(1);
    u32::try_from(latest)
        .map_err(|_| corrupted("stored attempt count exceeds the rebuild index range"))
}

/// 发送编排器：持有独占存储句柄与三个端口。
///
/// 存储是单写者独占打开，因此编排器拥有 `Store`；同一进程内不要同时打开第二个句柄。
pub struct SendOrchestrator<S: NodeSubmitter, C: CryptoEngine, K: Clock> {
    store: Store,
    submitter: S,
    engine: C,
    clock: K,
    worker: String,
}

impl<S: NodeSubmitter, C: CryptoEngine, K: Clock> std::fmt::Debug for SendOrchestrator<S, C, K> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SendOrchestrator")
            .field("worker", &self.worker)
            .field("now_unix_ms", &self.clock.now_unix_ms())
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

impl<S: NodeSubmitter, C: CryptoEngine, K: Clock> SendOrchestrator<S, C, K> {
    /// 构造编排器；`worker` 是本次调度者标识（进入 outbox 租约，冲突时显式失败）。
    pub fn new(store: Store, submitter: S, engine: C, clock: K, worker: &str) -> SendResult<Self> {
        chat_sync::ids::validate_id("worker", worker)?;
        Ok(Self {
            store,
            submitter,
            engine,
            clock,
            worker: worker.to_owned(),
        })
    }

    /// 只读存储访问（诊断/断言用）。
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// 可变存储访问（outbox 领取、恢复等上层集成用）。
    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }

    /// 取出底层 Store（把同一个句柄交还上层；不改变任何编排状态）。
    ///
    /// 存储是单写者独占打开，调用方取走后不得再使用本编排器。
    pub fn into_store(self) -> Store {
        self.store
    }

    /// 可注入时钟（测试里显式推进时间）。
    pub fn clock_mut(&mut self) -> &mut K {
        &mut self.clock
    }

    /// 本次调度者标识。
    pub fn worker(&self) -> &str {
        &self.worker
    }

    /// 当前可信时间（Unix 毫秒）；非正值视为时钟故障。
    fn now_ms(&self) -> SendResult<i64> {
        let now = self.clock.now_unix_ms();
        if now <= 0 {
            return Err(invalid_target("submission clock must be positive"));
        }
        Ok(now)
    }

    /// CORE-007 的本地切片：写一条发送意图（T1），返回 `draft_id`。
    ///
    /// 同一 `operation_id` 重放返回同一草稿（storage 的确定性 draft id + 请求摘要），
    /// 不会产生第二个意图、也不会重复加密。
    pub fn begin_send(
        &mut self,
        conversation_id: &str,
        operation_id: &str,
        body: &str,
    ) -> SendResult<String> {
        let conversation = ConversationId::parse(conversation_id)
            .map_err(|_| invalid_id("conversation id must be PROTOCOL-001 conversation text"))?;
        let operation = OperationId::parse(operation_id)
            .map_err(|_| invalid_id("operation id must be PROTOCOL-001 operation text"))?;
        let content = MessageContent::text(body).map_err(storage_failure)?;
        let now = self.now_ms()?;
        let request = ValidatedSendRequest {
            conversation_id: conversation.encode(),
            client_operation_id: operation.encode(),
            content,
            reply_to: None,
            origin: MessageOrigin::UserInput,
            created_at: now,
        };
        let mut transaction = self
            .store
            .begin_transaction(WriteScope::intent(&request.conversation_id))
            .map_err(storage_failure)?;
        let outcome = (|| {
            materialize_session(&mut transaction, &request.conversation_id, "")?;
            persist_send_intent(&mut transaction, request)
        })();
        match outcome {
            Ok(draft) => {
                commit_transaction(transaction).map_err(storage_failure)?;
                Ok(draft.draft_id)
            }
            Err(error) => {
                let _ = rollback_transaction(transaction);
                Err(storage_failure(error))
            }
        }
    }

    /// SYNC-003 的执行侧：把草稿变成一次耐久加密尝试（同草稿只加密一次）。
    pub fn prepare(&mut self, draft_id: &str) -> SendResult<PrepareOutcome> {
        let now = self.now_ms()?;
        let PlanState {
            draft,
            attempts,
            plan,
        } = self.plan_now(draft_id, now)?;
        match plan {
            SubmissionPlan::Prepare {
                submission_id,
                mls_epoch,
                attempt_index,
                ..
            }
            | SubmissionPlan::Rebuild {
                submission_id,
                mls_epoch,
                attempt_index,
                ..
            } => {
                let submission_text = submission_id.encode();
                // 幂等：同 SubmissionId 已有耐久尝试时绝不重新加密。
                if self
                    .store
                    .read_attempt(&submission_text)
                    .map_err(storage_failure)?
                    .is_some()
                {
                    return Ok(PrepareOutcome::Prepared {
                        submission_id: submission_text,
                        mls_epoch,
                        attempt_index,
                        already_prepared: true,
                    });
                }
                let body = content_text(&draft.content);
                let payload = self
                    .engine
                    .encrypt(&draft.conversation_id, &draft.draft_id, &body)?;
                if payload.mls_epoch != mls_epoch {
                    return Err(invalid_state(
                        "MLS epoch changed while preparing the attempt",
                    ));
                }
                self.persist_attempt(&draft, &submission_text, mls_epoch, payload.ciphertext)?;
                Ok(PrepareOutcome::Prepared {
                    submission_id: submission_text,
                    mls_epoch,
                    attempt_index,
                    already_prepared: false,
                })
            }
            SubmissionPlan::Resubmit { submission_id, .. } => {
                let Some(latest) = attempts.last() else {
                    return Err(corrupted("send plan references a missing attempt"));
                };
                Ok(PrepareOutcome::Prepared {
                    submission_id: submission_id.encode(),
                    mls_epoch: latest.mls_epoch,
                    attempt_index: new_attempt_index(attempts.len())?,
                    already_prepared: true,
                })
            }
            SubmissionPlan::QueryExisting { submission_id } => {
                Ok(PrepareOutcome::AwaitingReconciliation {
                    submission_id: submission_id.encode(),
                })
            }
            SubmissionPlan::WaitForLease {
                submission_id,
                lease_until_ms,
                ..
            } => Ok(PrepareOutcome::WaitingForLease {
                submission_id: submission_id.encode(),
                lease_until_ms,
            }),
            SubmissionPlan::Release { submission_id } => Ok(PrepareOutcome::AlreadyAccepted {
                submission_id: submission_id.encode(),
            }),
            SubmissionPlan::Abandoned { draft_id } => Ok(PrepareOutcome::Abandoned { draft_id }),
        }
    }

    /// SYNC-004 的执行侧：提交一次已耐久尝试，并把结果落库。
    pub fn dispatch(&mut self, draft_id: &str) -> SendResult<DispatchOutcome> {
        let now = self.now_ms()?;
        let PlanState { draft, plan, .. } = self.plan_now(draft_id, now)?;
        match plan {
            SubmissionPlan::Release { submission_id } => {
                // 幂等重放：接受已经落过库，顺手把可能缺失的草稿状态补齐。
                self.mark_draft_accepted(&draft)?;
                Ok(DispatchOutcome::Accepted {
                    submission_id: submission_id.encode(),
                    idempotent: true,
                })
            }
            SubmissionPlan::QueryExisting { submission_id } => {
                let submission_text = submission_id.encode();
                // 结果未知：只能按原 SubmissionId 查询，绝不重发、绝不重新加密。
                let outcome = self.submitter.query(&submission_text)?;
                self.record_result(&draft, &submission_text, outcome, now)
            }
            SubmissionPlan::Resubmit { submission_id, .. } => {
                let submission_text = submission_id.encode();
                let attempt = self
                    .store
                    .read_attempt(&submission_text)
                    .map_err(storage_failure)?
                    .ok_or_else(|| corrupted("send plan references a missing attempt"))?;
                let outcome = self.submitter.submit(&SubmitAttempt {
                    submission_id: submission_text.clone(),
                    conversation_id: attempt.conversation_id,
                    mls_epoch: attempt.mls_epoch,
                    ciphertext: attempt.immutable_ciphertext,
                })?;
                self.record_result(&draft, &submission_text, outcome, now)
            }
            SubmissionPlan::WaitForLease {
                submission_id,
                lease_until_ms,
                ..
            } => Ok(DispatchOutcome::WaitingForLease {
                submission_id: submission_id.encode(),
                until_ms: lease_until_ms,
            }),
            SubmissionPlan::Abandoned { draft_id } => Ok(DispatchOutcome::Abandoned { draft_id }),
            SubmissionPlan::Prepare { .. } => Err(invalid_state(
                "attempt must be prepared before it can be submitted",
            )),
            SubmissionPlan::Rebuild { .. } => Err(invalid_state(
                "rejected attempt must be rebuilt by prepare before it can be submitted",
            )),
        }
    }

    /// CORE-008：用户显式重试一条失败/过期的草稿。
    ///
    /// 语义（不新增状态机，仍由 SYNC-003/004 决策）：
    ///
    /// - 已正式接受 → `AlreadyAccepted`（不再重试，也绝不复用同一 SubmissionId 之外的路径）；
    /// - 结果未知 → `AwaitingReconciliation`：**只允许对账**，本调用不加密、不提交、不改状态；
    /// - 已取消/终止 → 显式 `INVALID_STATE_TRANSITION`（RETRY_NOT_ALLOWED 的冻结码投影）；
    /// - 不可重试的失败（权限/撤销/凭据/损坏）→ 显式错误：会话不再可发按 DOMAIN-010
    ///   `classify_retry` 判为 `WaitForUser` 并返回 `PERMISSION_DENIED`；
    ///   存储校验失败（损坏/篡改）原样返回 `STATE_CORRUPTED`；
    /// - 其余（新草稿、可重试失败、租约过期、被拒绝）→ 重新走 `prepare` 调度：
    ///   `Resubmit` 复用同一 SubmissionId 与同一密文（不重新加密）、`Rebuild` 用新
    ///   SubmissionId 重建（序号受 `MAX_ATTEMPT_INDEX` 限制）、`Prepare` 首次加密。
    pub fn retry_draft(&mut self, draft_id: &str) -> SendResult<PrepareOutcome> {
        let now = self.now_ms()?;
        let PlanState { draft, plan, .. } = self.plan_now(draft_id, now)?;
        match plan {
            SubmissionPlan::Release { submission_id } => Ok(PrepareOutcome::AlreadyAccepted {
                submission_id: submission_id.encode(),
            }),
            SubmissionPlan::QueryExisting { submission_id } => {
                Ok(PrepareOutcome::AwaitingReconciliation {
                    submission_id: submission_id.encode(),
                })
            }
            SubmissionPlan::Abandoned { .. } => Err(invalid_state(
                "draft is cancelled and cannot be retried",
            )),
            _ => {
                // 权限类不可重试：会话已离开/解散/暂停时重试必然失败，显式拒绝。
                self.ensure_conversation_sendable(&draft)?;
                // 复用既有 prepare 路径（同草稿幂等、重建有界），不复制加密逻辑。
                self.prepare(draft_id)
            }
        }
    }

    /// CORE-009：取消一条尚未正式接受的草稿（三态，绝不乐观删除）。
    ///
    /// - 已接受 → `CancelTooLate`：不撤回、不删除消息，只报告事实；
    /// - 结果未知 → `CancelUnknown`：只把用户意图落到草稿状态，**不写**
    ///   `cancel_requested`（否则对账窗口结束后该尝试永远无法被重新领取对账）；
    /// - 其余（排队/已加密待发/租约内/已拒绝）→ `CancelRequested`：草稿字节保留、
    ///   `drafts.state = 'cancelled'`、准备与发送任务标 `cancel_requested = 1`，
    ///   后续 `dispatch` 一律走 `Abandoned`（零提交）。
    pub fn cancel_draft(&mut self, draft_id: &str) -> SendResult<CancelOutcome> {
        let now = self.now_ms()?;
        let PlanState {
            draft,
            attempts,
            plan,
        } = self.plan_now(draft_id, now)?;
        // 只汇报“已经耐久存在”的 attempt：Prepare 计划里的派生 ID 还不是事实。
        let attempt_submission = attempts.last().map(|row| row.submission_id.clone());
        match plan {
            SubmissionPlan::Release { submission_id } => Ok(CancelOutcome::CancelTooLate {
                draft_id: draft.draft_id,
                submission_id: submission_id.encode(),
            }),
            SubmissionPlan::QueryExisting { submission_id } => {
                self.mark_draft_cancelled(&draft)?;
                Ok(CancelOutcome::CancelUnknown {
                    draft_id: draft.draft_id,
                    submission_id: submission_id.encode(),
                })
            }
            _ => {
                self.mark_draft_cancelled(&draft)?;
                self.suppress_outbox_jobs(&draft, attempt_submission.as_deref())?;
                Ok(CancelOutcome::CancelRequested {
                    draft_id: draft.draft_id,
                    submission_id: attempt_submission,
                })
            }
        }
    }

    /// outbox 结果对账：修复“attempt 已接受但草稿仍 queued”的两笔事务窗口。
    ///
    /// 幂等：修复后再次调用返回 `draft_repaired = false`；结果未知只报告
    /// `awaiting_reconciliation` 且**不做任何写操作**；其余状态原样报告一致。
    pub fn reconcile(&mut self, draft_id: &str) -> SendResult<ReconcileReport> {
        let now = self.now_ms()?;
        let PlanState { draft, plan, .. } = self.plan_now(draft_id, now)?;
        match plan {
            SubmissionPlan::Release { submission_id } => {
                // 接受是权威事实：即使草稿被取消/失败过，也以接受为准补齐。
                let draft_repaired = self.mark_draft_accepted(&draft)?;
                Ok(ReconcileReport {
                    submission_id: Some(submission_id.encode()),
                    draft_repaired,
                    awaiting_reconciliation: false,
                })
            }
            SubmissionPlan::QueryExisting { submission_id } => Ok(ReconcileReport {
                submission_id: Some(submission_id.encode()),
                draft_repaired: false,
                awaiting_reconciliation: true,
            }),
            other => Ok(ReconcileReport {
                submission_id: other.submission_id().map(|id| id.encode()),
                draft_repaired: false,
                awaiting_reconciliation: false,
            }),
        }
    }

    /// 会话是否仍允许发送（DOMAIN-001 的最小持久投影）。
    ///
    /// left/dissolved/paused 一律按 DOMAIN-010 `classify_retry` 判为 `WaitForUser`，
    /// 返回冻结码 `PERMISSION_DENIED`（不可自动重试）。
    fn ensure_conversation_sendable(&self, draft: &DraftRecord) -> SendResult<()> {
        let state = self
            .load_conversation_state(&draft.conversation_id)?
            .ok_or_else(|| invalid_target("conversation does not exist"))?;
        let blocked = match state.as_str() {
            "left" | "dissolved" | "paused" => Some(ErrorCode::PermissionDenied),
            _ => None,
        };
        if let Some(code) = blocked {
            let decision = classify_retry(code, OperationState::Prepared);
            if decision != RetryDecision::ResendSameBytes && !decision.requires_sync() {
                return Err(SyncError::new(
                    code,
                    "conversation no longer allows sending",
                ));
            }
        }
        Ok(())
    }

    /// 读取会话状态（只读投影；无该会话返回 `None`）。
    fn load_conversation_state(&self, conversation_id: &str) -> SendResult<Option<String>> {
        let connection = self.store.connection();
        let mut statement = connection
            .prepare("SELECT state FROM conversations WHERE conversation_id = ?1")
            .map_err(sql_failure)?;
        let rows = statement
            .query_map([conversation_id], |row| {
                let value: String = row.get(0)?;
                Ok(value)
            })
            .map_err(sql_failure)?;
        let mut found = None;
        for row in rows {
            found = Some(row.map_err(sql_failure)?);
        }
        Ok(found)
    }

    /// 把草稿置 `cancelled`（保留密文与 attempt 行，幂等）。
    fn mark_draft_cancelled(&mut self, draft: &DraftRecord) -> SendResult<()> {
        let transaction = self
            .store
            .begin_transaction(WriteScope::intent(&draft.conversation_id))
            .map_err(storage_failure)?;
        let updated = transaction
            .connection()
            .execute(
                "UPDATE drafts SET state = 'cancelled'
                  WHERE draft_id = ?1 AND state IN ('queued','unknown','failed')",
                [draft.draft_id.as_str()],
            )
            .map_err(sql_failure);
        match updated {
            Ok(_) => {
                commit_transaction(transaction).map_err(storage_failure)?;
                Ok(())
            }
            Err(error) => {
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }

    /// 标记准备/发送任务不可再被领取（`cancel_requested = 1`，storage 的领取会跳过）。
    ///
    /// 只标记仍在 `pending/leased` 的任务；已完成/已失败的任务保持原样。
    fn suppress_outbox_jobs(
        &mut self,
        draft: &DraftRecord,
        submission_id: Option<&str>,
    ) -> SendResult<()> {
        let prepare_key = format!("prepare:{}", draft.draft_id);
        let send_key = submission_id.map(|id| format!("send:{id}"));
        let transaction = self
            .store
            .begin_transaction(WriteScope::outbox())
            .map_err(storage_failure)?;
        let outcome = (|| -> SendResult<()> {
            transaction
                .connection()
                .execute(
                    "UPDATE outbox_jobs SET cancel_requested = 1
                      WHERE dedup_key = ?1 AND state IN ('pending','leased')",
                    [prepare_key.as_str()],
                )
                .map_err(sql_failure)?;
            if let Some(send_key) = send_key.as_deref() {
                transaction
                    .connection()
                    .execute(
                        "UPDATE outbox_jobs SET cancel_requested = 1
                          WHERE dedup_key = ?1 AND state IN ('pending','leased')",
                        [send_key],
                    )
                    .map_err(sql_failure)?;
            }
            Ok(())
        })();
        match outcome {
            Ok(()) => {
                commit_transaction(transaction).map_err(storage_failure)?;
                Ok(())
            }
            Err(error) => {
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }

    /// 读取草稿与 attempt 投影，并按 SYNC-003/004 决策下一步。
    fn plan_now(&self, draft_id: &str, now_unix_ms: i64) -> SendResult<PlanState> {
        chat_sync::ids::validate_id("draft", draft_id)?;
        let draft = self
            .store
            .read_draft(draft_id)
            .map_err(storage_failure)?
            .ok_or_else(|| invalid_target("draft does not exist"))?;
        let attempts = self.load_attempts(&draft.draft_id)?;
        let mls_epoch = match attempts.last() {
            Some(latest) => latest.mls_epoch,
            None => self.engine.epoch(&draft.conversation_id)?,
        };
        let snapshot = DraftSnapshot {
            draft_id: draft.draft_id.clone(),
            state: draft_state(&draft.state)?,
            mls_epoch,
            attempt_index: new_attempt_index(attempts.len())?,
        };
        let attempt_snapshot = self.attempt_snapshot(&draft, attempts.last())?;
        let plan = plan_submission(
            &snapshot,
            attempt_snapshot.as_ref(),
            &self.worker,
            now_unix_ms,
        )?;
        Ok(PlanState {
            draft,
            attempts,
            plan,
        })
    }

    /// 最新 attempt 的只读投影（含发送任务租约，语义同 STORAGE-014）。
    fn attempt_snapshot(
        &self,
        draft: &DraftRecord,
        latest: Option<&AttemptRow>,
    ) -> SendResult<Option<AttemptSnapshot>> {
        let Some(latest) = latest else {
            return Ok(None);
        };
        let submission_id = SubmissionId::parse(&latest.submission_id)
            .map_err(|_| corrupted("stored submission id is not PROTOCOL-001 text"))?;
        let job = self.load_send_job(&latest.submission_id)?;
        Ok(Some(AttemptSnapshot {
            submission_id,
            draft_id: draft.draft_id.clone(),
            state: attempt_state(&latest.state)?,
            mls_epoch: latest.mls_epoch,
            lease_owner: job.as_ref().and_then(|job| job.lease_owner.clone()),
            lease_until_ms: job.as_ref().and_then(|job| job.lease_until),
            lease_version: job.as_ref().map(|job| job.lease_version).unwrap_or(0),
        }))
    }

    /// 按草稿枚举既有 attempt（升序，末条为最新）。
    ///
    /// storage 未暴露按草稿枚举 attempt 的正式入口，这里用只读连接做投影查询。
    fn load_attempts(&self, draft_id: &str) -> SendResult<Vec<AttemptRow>> {
        let connection = self.store.connection();
        let mut statement = connection
            .prepare(
                "SELECT submission_id, state, mls_epoch FROM message_attempts
                  WHERE draft_id = ?1 ORDER BY rowid ASC",
            )
            .map_err(sql_failure)?;
        let rows = statement
            .query_map([draft_id], |row| {
                Ok(AttemptRow {
                    submission_id: row.get(0)?,
                    state: row.get(1)?,
                    mls_epoch: row.get(2)?,
                })
            })
            .map_err(sql_failure)?;
        let mut attempts = Vec::new();
        for row in rows {
            attempts.push(row.map_err(sql_failure)?);
        }
        Ok(attempts)
    }

    /// 读取某 SubmissionId 的发送任务行（无则 `None`）。
    fn load_send_job(&self, submission_id: &str) -> SendResult<Option<SendJobRow>> {
        let dedup_key = format!("send:{submission_id}");
        let connection = self.store.connection();
        let mut statement = connection
            .prepare(
                "SELECT job_id, state, retry_at, lease_owner, lease_until, lease_version
                   FROM outbox_jobs WHERE dedup_key = ?1",
            )
            .map_err(sql_failure)?;
        let rows = statement
            .query_map([dedup_key.as_str()], |row| {
                Ok(SendJobRow {
                    job_id: row.get(0)?,
                    state: row.get(1)?,
                    retry_at: row.get(2)?,
                    lease_owner: row.get(3)?,
                    lease_until: row.get(4)?,
                    lease_version: row.get(5)?,
                })
            })
            .map_err(sql_failure)?;
        let mut found = None;
        for row in rows {
            found = Some(row.map_err(sql_failure)?);
        }
        Ok(found)
    }

    /// T2：写入不可变尝试、排入发送任务并把准备任务置完成（单事务）。
    fn persist_attempt(
        &mut self,
        draft: &DraftRecord,
        submission_id: &str,
        mls_epoch: u64,
        ciphertext: Vec<u8>,
    ) -> SendResult<()> {
        let prepared = PreparedWrite {
            draft_id: draft.draft_id.clone(),
            conversation_id: draft.conversation_id.clone(),
            submission_id: submission_id.to_owned(),
            mls_epoch,
            immutable_ciphertext: ciphertext,
            request_hash: draft.request_hash,
            crypto_changes: Vec::new(),
        };
        let mut transaction = self
            .store
            .begin_transaction(WriteScope::attempt(&draft.conversation_id))
            .map_err(storage_failure)?;
        match persist_prepared_attempt(&mut transaction, prepared) {
            Ok(_) => {
                commit_transaction(transaction).map_err(storage_failure)?;
                Ok(())
            }
            Err(error) => {
                let _ = rollback_transaction(transaction);
                Err(storage_failure(error))
            }
        }
    }

    /// 定向领取该 SubmissionId 的发送任务租约（语义同 STORAGE-014）。
    ///
    /// storage 只暴露批量领取，这里用同一 CAS 规则（`lease_version` +
    /// `state`/`lease_until` 条件）在 outbox 事务内定向领取，避免把其它到期任务误租。
    fn claim_target_lease(
        &mut self,
        submission_id: &str,
        now_unix_ms: i64,
    ) -> SendResult<LeaseAccess> {
        let Some(job) = self.load_send_job(submission_id)? else {
            return Ok(LeaseAccess::NotClaimable);
        };
        let claimable = match job.state.as_str() {
            "pending" => job.retry_at <= now_unix_ms,
            "leased" => job.lease_until.is_some_and(|until| until <= now_unix_ms),
            _ => false,
        };
        if !claimable {
            return Ok(match job.state.as_str() {
                "pending" => LeaseAccess::Waiting {
                    until_ms: job.retry_at,
                },
                "leased" => LeaseAccess::Waiting {
                    until_ms: job.lease_until.unwrap_or(job.retry_at),
                },
                _ => LeaseAccess::NotClaimable,
            });
        }
        let lease_until = now_unix_ms.saturating_add(OUTBOX_LEASE_MS);
        let lease_version =
            i64::try_from(job.lease_version).map_err(|_| corrupted("lease version is out of range"))?;
        let dedup_key = format!("send:{submission_id}");
        let transaction = self
            .store
            .begin_transaction(WriteScope::outbox())
            .map_err(storage_failure)?;
        let outcome = (|| -> SendResult<LeaseAccess> {
            let updated = transaction
                .connection()
                .execute(
                    "UPDATE outbox_jobs
                        SET state = 'leased', lease_owner = ?3, lease_until = ?4,
                            lease_version = lease_version + 1
                      WHERE dedup_key = ?1
                        AND lease_version = ?2
                        AND cancel_requested = 0
                        AND ((state = 'pending' AND retry_at <= ?5)
                             OR (state = 'leased' AND lease_until IS NOT NULL AND lease_until <= ?5))",
                    (
                        dedup_key.as_str(),
                        lease_version,
                        self.worker.as_str(),
                        lease_until,
                        now_unix_ms,
                    ),
                )
                .map_err(sql_failure)?;
            if updated != 1 {
                // CAS 失败：另一个 worker 抢先一步，本次不改任何状态。
                return Ok(LeaseAccess::Waiting {
                    until_ms: lease_until,
                });
            }
            Ok(LeaseAccess::Leased(LeasedJob {
                job_id: job.job_id.clone(),
                dedup_key: dedup_key.clone(),
                kind: "send".to_owned(),
                payload_ref: submission_id.to_owned(),
                conversation_id: None,
                depends_on: None,
                retry_count: 0,
                retry_at: job.retry_at,
                worker: self.worker.clone(),
                lease_until,
                lease_version: job.lease_version.saturating_add(1),
            }))
        })();
        match outcome {
            Ok(LeaseAccess::Leased(lease)) => {
                commit_transaction(transaction).map_err(storage_failure)?;
                Ok(LeaseAccess::Leased(lease))
            }
            Ok(other) => {
                let _ = rollback_transaction(transaction);
                Ok(other)
            }
            Err(error) => {
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }

    /// 落结果时的租约来源：优先定向领取；否则沿用本 worker 尚未到期的有效租约
    /// （unknown 对账窗口内必须能落对账结果）。
    fn lease_for_result(
        &mut self,
        submission_id: &str,
        now_unix_ms: i64,
    ) -> SendResult<LeaseAccess> {
        if let LeaseAccess::Leased(lease) = self.claim_target_lease(submission_id, now_unix_ms)? {
            return Ok(LeaseAccess::Leased(lease));
        }
        let Some(job) = self.load_send_job(submission_id)? else {
            return Ok(LeaseAccess::NotClaimable);
        };
        if job.state == "leased" && job.lease_owner.as_deref() == Some(self.worker.as_str()) {
            return Ok(LeaseAccess::Leased(LeasedJob {
                job_id: job.job_id,
                dedup_key: format!("send:{submission_id}"),
                kind: "send".to_owned(),
                payload_ref: submission_id.to_owned(),
                conversation_id: None,
                depends_on: None,
                retry_count: 0,
                retry_at: job.retry_at,
                worker: self.worker.clone(),
                lease_until: job.lease_until.unwrap_or(now_unix_ms),
                lease_version: job.lease_version,
            }));
        }
        Ok(match job.state.as_str() {
            "pending" => LeaseAccess::Waiting {
                until_ms: job.retry_at,
            },
            "leased" => LeaseAccess::Waiting {
                until_ms: job.lease_until.unwrap_or(job.retry_at),
            },
            _ => LeaseAccess::NotClaimable,
        })
    }

    /// 把节点三态结果按 STORAGE-015 语义落库。
    fn record_result(
        &mut self,
        draft: &DraftRecord,
        submission_id: &str,
        outcome: SubmitOutcome,
        now_unix_ms: i64,
    ) -> SendResult<DispatchOutcome> {
        let lease = match self.lease_for_result(submission_id, now_unix_ms)? {
            LeaseAccess::Leased(lease) => lease,
            LeaseAccess::Waiting { until_ms } => {
                return Ok(DispatchOutcome::WaitingForLease {
                    submission_id: submission_id.to_owned(),
                    until_ms,
                });
            }
            LeaseAccess::NotClaimable => {
                return Err(invalid_state(
                    "send job is not in a claimable state for this result",
                ));
            }
        };
        let job_result = match outcome {
            SubmitOutcome::Accepted { detail } => JobResult::FormallyAccepted { detail },
            SubmitOutcome::Unknown { detail } => JobResult::Unknown { detail },
            SubmitOutcome::Rejected { detail } => JobResult::Failed {
                retryable: false,
                detail,
            },
        };
        let state = self
            .store
            .mark_outbox_result_at(&lease, job_result, now_unix_ms)
            .map_err(storage_failure)?;
        match state.outcome {
            JobOutcome::Completed => {
                self.mark_draft_accepted(draft)?;
                Ok(DispatchOutcome::Accepted {
                    submission_id: submission_id.to_owned(),
                    idempotent: false,
                })
            }
            JobOutcome::Unknown => Ok(DispatchOutcome::Unknown {
                submission_id: submission_id.to_owned(),
            }),
            JobOutcome::Failed => Ok(DispatchOutcome::Rejected {
                submission_id: submission_id.to_owned(),
            }),
            JobOutcome::RetryScheduled => Err(invalid_state(
                "send results must not be recorded as retryable failures",
            )),
        }
    }

    /// 正式接受后把草稿置 `accepted`（幂等，并报告本次是否真的修复了草稿）。
    ///
    /// **接受是权威事实**：即使草稿曾被取消或标记失败，也以接受为准补齐（两笔事务窗口
    /// 与取消竞态都靠这里收敛）。storage 没有「置草稿 accepted」的正式入口，这里在
    /// intent 事务内更新；返回 `true` 表示本次补写，`false` 表示本来就一致。
    fn mark_draft_accepted(&mut self, draft: &DraftRecord) -> SendResult<bool> {
        // 先读当前状态（事务持有期间不再借用存储句柄）。
        let current = self
            .store
            .read_draft(&draft.draft_id)
            .map_err(storage_failure)?
            .ok_or_else(|| corrupted("draft disappeared while marking it accepted"))?;
        if current.state == "accepted" {
            return Ok(false);
        }
        let transaction = self
            .store
            .begin_transaction(WriteScope::intent(&draft.conversation_id))
            .map_err(storage_failure)?;
        let updated = transaction
            .connection()
            .execute(
                "UPDATE drafts SET state = 'accepted'
                  WHERE draft_id = ?1 AND state != 'accepted'",
                [draft.draft_id.as_str()],
            )
            .map_err(sql_failure);
        match updated {
            Ok(rows) => {
                commit_transaction(transaction).map_err(storage_failure)?;
                Ok(rows == 1)
            }
            Err(error) => {
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }
}
