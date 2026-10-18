//! 客户端内核服务层（CORE-005/006/007/010 的竖直切片）。
//!
//! `ConversationCore` 是 Kotlin 业务服务的唯一持久真相入口：它持有一个正式加密
//! `Store`（主密钥由 `Store` 内部的 `ProtectedStorageKey` 持有，绝不复制到 DTO）
//! 与生命周期状态，只编排 storage 的既有函数，不自行加密、不自行模拟投递成功：
//!
//! - 草稿：`create_draft` 校验 PROTOCOL-001 ID 与正文上限，在单笔 T1 事务内补建会话行
//!   并调用 STORAGE-006 `persist_send_intent`；草稿正文只在内存与封装缓冲中出现；
//! - 正式状态：`apply_verified_entry` 委托 STORAGE-008，单笔 T3 事务内写会话摘要与
//!   正式消息行，失败整体回滚；
//! - 查询：`list_conversations` / `list_messages` 委托 STORAGE-012/013 的键集分页，
//!   把内部游标令牌原样透传给调用方（不解释、不伪造）；
//! - 生命周期：`close` 释放 `Store`（同时释放独占写锁），之后所有方法显式
//!   `CORE_CLOSED`，而不是返回过期快照。
//!
//! 已知限制（详见 docs/implementation/core.md 的对应实现状态）：
//! - `mine` 由会话摘要记录的本机作者身份判定：本 store 尚未接入身份模块，因此
//!   以“曾经写入过的 `mine = true` 条目的作者”作为本机身份；没有该记录的会话里
//!   远端消息一律 `mine = false`（不猜测）；
//! - `unread_count` 恒为 `None`：未读需要读位置存储（schema 无该表），不返回伪造的 0；
//! - 游标是 storage 的内部 `spg1` 令牌，不是线协议 `cur1` 令牌；
//! - 草稿 ID 是 storage 派生的本地标识（`draft-<32 位小写 hex>`），不是
//!   PROTOCOL-001 `DraftId` 文本，调用方必须原样回传。

use std::fmt;
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_identity::contact::transition_allowed;
use chat_identity::{
    ContactState, DeviceKeys, Fingerprint, IdentityError, LocalAccount, MAX_DISPLAY_NAME_BYTES,
};
use chat_protocol::{AccountId, ConversationId, DeviceId, MessageId, OperationId};
use chat_sync::clock::SystemClock;
use chat_sync::error::{ErrorCode, SyncError};
use sha2::{Digest, Sha256};

use crate::core_events::{
    CoreEvent, EventBatch, EventBus, SendService, DEFAULT_EVENT_CAPACITY, MAX_EVENT_BATCH,
};
use crate::send_orchestrator::{
    CancelOutcome, CryptoEngine, EncryptedPayload, NodeSubmitter, SendResult, SubmitAttempt,
    SubmitOutcome,
};
use crate::send_runtime::{MAX_MLS_GROUP_KEY_BYTES, SendNodeConfig, SendRuntime, SendStep, SendStepView};
use crate::groups::{
    CreateGroupRequest, GROUP_POLICY_VERSION, GroupBootstrapPeer, GroupMemberKeyPackage, GroupView,
};
use chat_crypto::engine::MlsDevice;
use chat_domain::MemberRole;
use chat_storage::formal::{
    MASTER_KEY_BYTES, MAX_ID_BYTES, MAX_SESSION_TITLE_BYTES, KeyProvenance, MessageContent,
    MessageOrigin, MessageView, PageCursor, ProtectedStorageKey, SessionSummary, StorageError,
    Store, StoreOptions, ValidatedSendRequest, WriteScope, commit_transaction, materialize_session,
    open_store, persist_send_intent, rollback_transaction,
};

/// 已验证条目输入（STORAGE-008）。FFI 层通过本重导出构造，避免额外依赖 chat-storage。
pub use chat_storage::formal::VerifiedEntryInput;

/// 内核层稳定错误码。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreError {
    /// 调用方输入不合法（ID 文本、长度上限、正文契约）。
    InvalidInput,
    /// 内核已关闭（或持有锁的线程 panic 后不可再用）。
    Closed,
}

impl CoreError {
    /// 稳定错误码字符串（FfiError 直接透传，UI 不解析消息）。
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "INVALID_INPUT",
            Self::Closed => "CORE_CLOSED",
        }
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for CoreError {}

fn invalid_input(reason: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(CoreError::InvalidInput).context(reason.into())
}

fn closed() -> anyhow::Error {
    anyhow::Error::new(CoreError::Closed)
}

/// 稳定错误码：优先返回 storage 的既有错误码，其次返回内核错误码，最后是
/// `CORE_FAILURE`（未知错误绝不伪装成成功）。
pub fn error_code(error: &anyhow::Error) -> &'static str {
    if let Some(storage) = error.downcast_ref::<StorageError>() {
        return storage.code();
    }
    // 身份层错误码由 PROTOCOL-014 冻结（identity 不新增码），直接透传码表字符串。
    if let Some(identity) = error.downcast_ref::<IdentityError>() {
        return identity.code().as_str();
    }
    // 发送编排（SYNC-003/004 执行侧）与调度决策都用冻结码表。
    if let Some(sync) = error.downcast_ref::<SyncError>() {
        return sync.code().as_str();
    }
    // 群治理（GROUPS-001~006 + 控制提交接受/丢弃）的冻结码。
    if let Some(group) = error.downcast_ref::<crate::groups::GroupError>() {
        return group.code();
    }
    // 事件流的有界拒绝（容量/单批上限）。
    if let Some(event) = error.downcast_ref::<crate::core_events::EventError>() {
        return event.as_str();
    }
    if let Some(core) = error.downcast_ref::<CoreError>() {
        return core.code();
    }
    "CORE_FAILURE"
}

/// (稳定错误码, 脱敏消息) 投影：FFI 层直接用它构造自己的错误类型，
/// 不必依赖 `anyhow` 的具体类型。
pub fn error_projection(error: &anyhow::Error) -> (&'static str, String) {
    (error_code(error), error_message(error))
}

/// 脱敏后的错误消息：截断 300 字符并把换行压平，避免把 SQL 或长输入塞进 UI。
pub fn error_message(error: &anyhow::Error) -> String {
    let text = error.to_string();
    let truncated: String = text.chars().take(300).collect();
    truncated.replace('\n', " ")
}

/// 草稿投影（CORE-007 的 QUEUED 结果与 CORE-010 的状态查询共用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftView {
    /// 本地草稿 ID（storage 派生，原样回传给后续调用）。
    pub draft_id: String,
    /// 用户操作幂等键（PROTOCOL-001 文本）。
    pub operation_id: String,
    /// 会话 ID（PROTOCOL-001 文本）。
    pub conversation_id: String,
    /// 明文正文（只用于可信 UI 路径，不落盘）。
    pub body: String,
    /// 草稿状态：`queued` 或 `prepared`。
    pub state: String,
    /// 关联的 SubmissionId（尚无准备尝试时为 `None`）。
    pub submission_id: Option<String>,
}

/// CORE-010 草稿状态投影：只含状态与可执行动作，**不含正文**。
///
/// 与 [`DraftView`] 的区别：`DraftView` 是“新建草稿的返回值”（带明文正文，只给可信
/// UI 路径）；本类型是“当前真实状态”（无正文），供列表/按钮态与事件合并使用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftStatusView {
    /// 本地草稿 ID（storage 派生；原样回传）。
    pub draft_id: String,
    /// 会话 ID。
    pub conversation_id: String,
    /// 用户操作幂等键（PROTOCOL-001 文本）。
    pub operation_id: String,
    /// 有效状态：`queued` / `prepared` / `submitted` / `accepted` / `rejected` /
    /// `unknown` / `cancelled` / `failed`（见 [`draft_status_view`] 的推导表）。
    pub status: String,
    /// 最新尝试的 SubmissionId（尚无尝试时为 `None`）。
    pub submission_id: Option<String>,
    /// 最新尝试的重建序号（0 = 首次尝试；尚无尝试时为 `None`）。
    pub attempt_index: Option<u32>,
    /// 已尝试次数（含被拒绝的重建尝试）。
    pub attempts: u32,
    /// 可观测的最后活动时间（Unix 毫秒）。schema 没有 `drafts.updated_at`，这里取
    /// 草稿创建时间与发送任务 `retry_at`/`lease_until` 的最大值，不伪造写入时间。
    pub updated_at_ms: u64,
    /// 现在调用 `retry_draft` 是否会真正推进（已接受/已取消/结果未知/租约被占时为 `false`）。
    pub can_retry: bool,
    /// 现在调用 `cancel_draft` 是否还有意义（已接受 → `CancelTooLate`，已取消 → 无意义）。
    pub can_cancel: bool,
}

/// 发送任务行的只读投影（状态查询用；不修改任何状态）。
struct DraftSendJobRow {
    retry_at: i64,
    lease_until: Option<i64>,
}

fn draft_attempt_rows(
    store: &Store,
    draft_id: &str,
) -> Result<Vec<(String, String)>, StorageError> {
    let connection = store.connection();
    let mut statement = connection
        .prepare(
            "SELECT submission_id, state FROM message_attempts
              WHERE draft_id = ?1 ORDER BY rowid ASC",
        )
        .map_err(StorageError::from)?;
    let rows = statement
        .query_map([draft_id], |row| {
            Ok((row.get::<usize, String>(0)?, row.get::<usize, String>(1)?))
        })
        .map_err(StorageError::from)?;
    let mut attempts = Vec::new();
    for row in rows {
        attempts.push(row.map_err(StorageError::from)?);
    }
    Ok(attempts)
}

fn draft_send_job_row(
    store: &Store,
    submission_id: &str,
) -> Result<Option<DraftSendJobRow>, StorageError> {
    let dedup_key = format!("send:{submission_id}");
    let connection = store.connection();
    let mut statement = connection
        .prepare("SELECT retry_at, lease_until FROM outbox_jobs WHERE dedup_key = ?1")
        .map_err(StorageError::from)?;
    let rows = statement
        .query_map([dedup_key.as_str()], |row| {
            Ok(DraftSendJobRow {
                retry_at: row.get::<usize, i64>(0)?,
                lease_until: row.get::<usize, Option<i64>>(1)?,
            })
        })
        .map_err(StorageError::from)?;
    let mut found = None;
    for row in rows {
        found = Some(row.map_err(StorageError::from)?);
    }
    Ok(found)
}

/// 有效状态推导：草稿状态优先（accepted/cancelled/failed 是终态），
/// 否则取最新尝试的状态（acceptance 是权威事实，所以 attempt accepted 也算 accepted）。
fn effective_draft_status(draft_state: &str, attempt_state: Option<&str>) -> String {
    match draft_state {
        "accepted" | "delivered" => "accepted".to_owned(),
        "cancelled" => "cancelled".to_owned(),
        "failed" => "failed".to_owned(),
        _ => match attempt_state {
            None => "queued".to_owned(),
            Some(state) => state.to_owned(),
        },
    }
}

/// CORE-010：按存储投影草稿状态（`ConversationCore` 与 `SendService` 共用同一实现）。
///
/// - 未知草稿返回 `Ok(None)`；
/// - 状态查询**不解密正文**（正文损坏只在发送路径与 `draft_status` 的 body 投影上显式失败）；
/// - `can_retry`/`can_cancel` 由状态与冻结规则推导，见下方推导表，绝不臆造：
///
/// | 有效状态 | can_retry | can_cancel |
/// |---|---|---|
/// | queued / prepared / submitted（无有效租约） | true | true |
/// | queued / prepared / submitted（发送任务有效租约被占） | false（等租约） | true |
/// | rejected（attempt_index < MAX_ATTEMPT_INDEX） | true（新 SubmissionId 重建） | true |
/// | rejected（预算用尽） | false | true |
/// | unknown | false（只能对账） | true（`CancelUnknown` 只记意图） |
/// | accepted | false（`AlreadyAccepted`） | false（`CancelTooLate`） |
/// | cancelled / failed | false | false |
pub fn draft_status_view(
    store: &Store,
    draft_id: &str,
) -> Result<Option<DraftStatusView>, StorageError> {
    if !is_local_draft_id(draft_id) {
        return Err(StorageError::InvalidInput(
            "draft_id must be the opaque identifier returned by create_draft".to_owned(),
        ));
    }
    let row: Option<(String, String, String, i64)> = {
        let connection = store.connection();
        let mut statement = connection
            .prepare(
                "SELECT conversation_id, operation_id, state, created_at
                   FROM drafts WHERE draft_id = ?1",
            )
            .map_err(StorageError::from)?;
        let rows = statement
            .query_map([draft_id], |row| {
                Ok((
                    row.get::<usize, String>(0)?,
                    row.get::<usize, String>(1)?,
                    row.get::<usize, String>(2)?,
                    row.get::<usize, i64>(3)?,
                ))
            })
            .map_err(StorageError::from)?;
        let mut found = None;
        for row in rows {
            found = Some(row.map_err(StorageError::from)?);
        }
        found
    };
    let Some((conversation_id, operation_id, draft_state, created_at)) = row else {
        return Ok(None);
    };

    let attempts = draft_attempt_rows(store, draft_id)?;
    let attempts_len = attempts.len();
    let latest = attempts.last();
    let submission_id = latest.map(|(submission, _)| submission.clone());
    let attempt_index = if attempts_len == 0 {
        None
    } else {
        Some(u32::try_from(attempts_len - 1).unwrap_or(u32::MAX))
    };
    let status = effective_draft_status(
        &draft_state,
        latest.map(|(_, state)| state.as_str()),
    );

    let job = match submission_id.as_deref() {
        Some(submission) => draft_send_job_row(store, submission)?,
        None => None,
    };
    let now = now_unix_ms().unwrap_or(0);
    let lease_until = job.as_ref().and_then(|row| row.lease_until);
    let lease_active = lease_until.is_some_and(|until| until > now);
    let mut updated_at = created_at;
    if let Some(row) = job.as_ref() {
        updated_at = updated_at.max(row.retry_at);
        if let Some(until) = row.lease_until {
            updated_at = updated_at.max(until);
        }
    }
    let can_retry = match status.as_str() {
        "queued" | "prepared" | "submitted" => !lease_active,
        "rejected" => attempt_index
            .map(|index| index < chat_sync::send::MAX_ATTEMPT_INDEX)
            .unwrap_or(false),
        _ => false,
    };
    let can_cancel = !matches!(status.as_str(), "accepted" | "cancelled");

    Ok(Some(DraftStatusView {
        draft_id: draft_id.to_owned(),
        conversation_id,
        operation_id,
        status,
        submission_id,
        attempt_index,
        attempts: u32::try_from(attempts_len).unwrap_or(u32::MAX),
        updated_at_ms: u64::try_from(updated_at).unwrap_or(0),
        can_retry,
        can_cancel,
    }))
}

/// 会话列表投影（CORE-005）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationView {
    /// 会话 ID。
    pub conversation_id: String,
    /// 本地摘要标题（未知时为空字符串，UI 自行回退，不伪造占位标题）。
    pub title: String,
    /// 最新条目的预览（按字符截断）。
    pub preview: String,
    /// 未读计数；当前恒为 `None`（需要读位置存储，见模块文档的限制）。
    pub unread_count: Option<u32>,
    /// 最近活动时间（Unix 毫秒）。
    pub updated_at_ms: u64,
}

/// 消息行投影（CORE-006）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageRowView {
    /// 消息 ID；本机待发意图为草稿 ID。
    pub message_id: String,
    /// 会话 ID。
    pub conversation_id: String,
    /// 作者账户 ID（本机待发意图为本机身份，未知时为空字符串）。
    pub author: String,
    /// 明文正文。
    pub body: String,
    /// 是否本机作者。
    pub mine: bool,
    /// 展示时间（Unix 毫秒）。
    pub created_at_ms: u64,
}

/// 一页会话。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationPageView {
    /// 本页条目（最近活动在前）。
    pub items: Vec<ConversationView>,
    /// 下一页游标；`None` 表示没有更多。
    pub next_cursor: Option<String>,
}

/// 身份账户持久化输入（IDENTITY-001 的本地持久化）。
///
/// 两个种子都是机密材料：只在内存中解码，交给 storage 后由 storage 的
/// `Zeroizing` 缓冲封装落盘；本类型不实现 `Clone`，`Debug` 只打印长度。
pub struct IdentityAccountInput {
    /// 显示名（1..=64 字节，不含控制字符）。
    pub display_name: String,
    /// 32 字节账户根密钥种子（64 位十六进制）。
    pub account_seed_hex: String,
    /// 32 字节设备密钥种子（64 位十六进制）。
    pub device_seed_hex: String,
    /// 创建时间（Unix 毫秒；0 表示使用当前时钟）。
    pub created_at_ms: u64,
}

impl fmt::Debug for IdentityAccountInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IdentityAccountInput")
            .field("display_name", &self.display_name)
            .field("account_seed_len", &self.account_seed_hex.len())
            .field("device_seed_len", &self.device_seed_hex.len())
            .field("created_at_ms", &self.created_at_ms)
            .finish()
    }
}

/// 可信恢复后的本机身份句柄：可签名/签发邀请（种子不离开 core）。
///
/// 故意不实现 Clone/Debug：私钥材料不复制、不打印。
pub struct PersistedIdentity {
    /// 账户（含根密钥，可签发邀请与设备凭据）。
    pub account: LocalAccount,
    /// 设备密钥（用于设备凭据/签名）。
    pub device: DeviceKeys,
    /// 设备 ID（已与库中记录核对一致）。
    pub device_id: DeviceId,
}

/// 身份账户投影：**不含任何种子/私钥**，可直接交给 UI。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityAccountView {
    /// 账户 ID（PROTOCOL-001 文本）。
    pub account_id: String,
    /// 设备 ID（PROTOCOL-001 文本）。
    pub device_id: String,
    /// 显示名。
    pub display_name: String,
    /// 分组展示的人工核验指纹（IDENTITY-007）。
    pub fingerprint: String,
    /// 创建时间（Unix 毫秒）。
    pub created_at_ms: u64,
    /// 本次调用是否命中既有记录（读取路径恒为 `true`）。
    pub already_existed: bool,
}

/// 联系人写入请求（IDENTITY-006 的持久化形式）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactUpsertRequest {
    /// 对端账户 ID（PROTOCOL-001 文本）。
    pub account_id: String,
    /// 显示名（1..=64 字节，不含控制字符）。
    pub display_name: String,
    /// 观测/固定指纹（分组或连续十六进制文本）。
    pub fingerprint: String,
    /// 联系人状态：requested / accepted / declined / blocked / identity_changed。
    pub state: String,
    /// 事件时间（Unix 毫秒，用于列表排序）。
    pub updated_at_ms: u64,
}

/// 联系人投影（不含任何密钥材料）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactView {
    /// 派生联系人键（幂等键）。
    pub contact_id: String,
    /// 对端账户 ID。
    pub account_id: String,
    /// 显示名。
    pub display_name: String,
    /// 分组展示的指纹文本。
    pub fingerprint: String,
    /// 联系人状态。
    pub state: String,
    /// 事件时间。
    pub updated_at_ms: u64,
    /// 本次调用是否命中既有记录（读取路径恒为 `true`）。
    pub already_existed: bool,
}

/// 邀请一次性消费结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsumedInviteView {
    /// 邀请 ID。
    pub invite_id: String,
    /// 消费时间（Unix 毫秒）。
    pub consumed_at_ms: u64,
}

/// 一页消息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagePageView {
    /// 本页条目（本机待发在前，正式消息按序号降序）。
    pub items: Vec<MessageRowView>,
    /// 下一页游标；`None` 表示没有更多。
    pub next_cursor: Option<String>,
}

/// 事件包装使用的固定调度者标识（进入 outbox 租约语义；同一进程内稳定）。
const EVENT_SEND_WORKER: &str = "ffi-cancel-worker";

/// 不联网的节点提交桩：`submit`/`query` 一律显式失败（`NETWORK_UNAVAILABLE`）。
///
/// 语义（与 docs/implementation/bridge.md 一致）：FFI 尚未接入真实节点时，任何
/// “提交/查询”都必须显式失败；桩**绝不伪造** Accepted，也不假装结果未知。
#[derive(Clone, Copy, Debug, Default)]
pub struct StubSubmitter;

impl NodeSubmitter for StubSubmitter {
    fn submit(&self, _attempt: &SubmitAttempt) -> SendResult<SubmitOutcome> {
        Err(SyncError::new(
            ErrorCode::NetworkUnavailable,
            "node.submit_not_wired",
        ))
    }

    fn query(&self, _submission_id: &str) -> SendResult<SubmitOutcome> {
        Err(SyncError::new(
            ErrorCode::NetworkUnavailable,
            "node.query_not_wired",
        ))
    }
}

/// 未接入 MLS 群时桩端口报告的 epoch。
///
/// **不是**真实群状态：只用于“取消/状态”这类需要 plan 身份但**绝不加密**的路径；
/// 任何真正需要密文的动作（`encrypt`）都会显式 `PROTOCOL_UNSUPPORTED`。
pub const UNWIRED_MLS_EPOCH: u64 = 0;

/// 不加密的加密引擎桩：接口齐全但绝不产生密文。
///
/// - `epoch`：固定返回 [`UNWIRED_MLS_EPOCH`]（让取消/状态路径能建立 plan 身份）；
/// - `encrypt`：显式 `PROTOCOL_UNSUPPORTED`，绝不返回伪造密文。
///
/// 因此 `prepare`/`dispatch`/`retry` 在未接线时必然显式失败，不存在“假成功”。
#[derive(Clone, Copy, Debug, Default)]
pub struct StubCrypto;

impl CryptoEngine for StubCrypto {
    fn epoch(&self, _conversation_id: &str) -> SendResult<u64> {
        Ok(UNWIRED_MLS_EPOCH)
    }

    fn encrypt(
        &mut self,
        _conversation_id: &str,
        _draft_id: &str,
        _body: &str,
    ) -> SendResult<EncryptedPayload> {
        Err(SyncError::new(
            ErrorCode::ProtocolUnsupported,
            "mls.encrypt_not_wired",
        ))
    }
}

/// 客户端内核服务：持有正式 `Store`（含主密钥）与生命周期状态。
///
/// `Store` 是独占单写者句柄，因此这里用 `Mutex<Option<..>>` 表达“打开/已关闭”：
/// `close` 取走句柄即释放独占写锁；之后任何调用都显式返回 `CORE_CLOSED`。
pub struct ConversationCore {
    database_path: String,
    store: Mutex<Option<Store>>,
    /// 内核事件总线（有界、单调序号）：`cancel_draft` 与 `close` 发布，`poll_events` 读取。
    events: Mutex<EventBus>,
    /// 发送节点配置（SND）：只保存，不发请求；`None` = 未配置，send_* 显式失败。
    send_node: Mutex<Option<SendNodeConfig>>,
    /// MLS 群键覆盖（SND）：`None` = 自动绑定本库唯一的持久 MLS 群。
    send_group: Mutex<Option<String>>,
}

impl fmt::Debug for ConversationCore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConversationCore")
            .field("database_path", &self.database_path)
            .field("open", &self.is_open())
            .finish()
    }
}

fn decode_hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 解析 64 位十六进制密钥材料（32 字节）；解码缓冲用后由调用方清零。
fn decode_seed_hex(value: &str, label: &str) -> anyhow::Result<[u8; MASTER_KEY_BYTES]> {
    let bytes = value.as_bytes();
    if bytes.len() != MASTER_KEY_BYTES * 2 {
        return Err(invalid_input(format!(
            "{label} must be exactly 64 hexadecimal characters (32 bytes)"
        )));
    }
    let mut raw = [0u8; MASTER_KEY_BYTES];
    let (pairs, remainder) = bytes.as_chunks::<2>();
    debug_assert!(remainder.is_empty(), "hex length was checked above");
    for (index, pair) in pairs.iter().enumerate() {
        let Some(high) = decode_hex_digit(pair[0]) else {
            raw.fill(0);
            return Err(invalid_input(format!(
                "{label} contains a non-hexadecimal character"
            )));
        };
        let Some(low) = decode_hex_digit(pair[1]) else {
            raw.fill(0);
            return Err(invalid_input(format!(
                "{label} contains a non-hexadecimal character"
            )));
        };
        raw[index] = (high << 4) | low;
    }
    Ok(raw)
}

/// 解析 64 位十六进制主密钥（32 字节）；解码缓冲用后立即清零。
fn decode_master_key(master_key_hex: &str) -> anyhow::Result<ProtectedStorageKey> {
    let mut raw = decode_seed_hex(master_key_hex, "master key")?;
    let key = ProtectedStorageKey::try_from_slice(&raw).map_err(anyhow::Error::new);
    raw.fill(0);
    key
}

fn now_unix_ms() -> anyhow::Result<i64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| invalid_input("system clock is before the Unix epoch"))?;
    i64::try_from(elapsed.as_millis())
        .map_err(|_| invalid_input("system clock is outside the supported range"))
}

fn parse_conversation(text: &str) -> anyhow::Result<ConversationId> {
    ConversationId::parse(text).map_err(|error| {
        invalid_input(format!(
            "conversation_id must be PROTOCOL-001 conversation text (conv:1:<64 lowercase hex>): {error}"
        ))
    })
}

fn parse_message(text: &str) -> anyhow::Result<MessageId> {
    MessageId::parse(text).map_err(|error| {
        invalid_input(format!(
            "message_id must be PROTOCOL-001 message text (msg:1:<64 lowercase hex>): {error}"
        ))
    })
}

fn parse_operation(text: &str) -> anyhow::Result<OperationId> {
    OperationId::parse(text).map_err(|error| {
        invalid_input(format!(
            "operation_id must be PROTOCOL-001 operation text (op:1:<64 lowercase hex>): {error}"
        ))
    })
}

fn parse_account(text: &str) -> anyhow::Result<AccountId> {
    AccountId::parse(text).map_err(|error| {
        invalid_input(format!(
            "author_account must be PROTOCOL-001 account text (acct:1:<64 lowercase hex>): {error}"
        ))
    })
}

fn parse_device(text: &str) -> anyhow::Result<DeviceId> {
    DeviceId::parse(text).map_err(|error| {
        invalid_input(format!(
            "author_device must be PROTOCOL-001 device text (dev:1:<64 lowercase hex>): {error}"
        ))
    })
}

/// 草稿 ID 是 storage 派生的本地标识，不是 PROTOCOL-001 文本：只做有界结构校验。
fn is_local_draft_id(draft_id: &str) -> bool {
    !draft_id.is_empty()
        && draft_id.len() <= MAX_ID_BYTES
        && draft_id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':')
        })
}

fn validate_local_draft_id(draft_id: &str) -> anyhow::Result<()> {
    if !is_local_draft_id(draft_id) {
        return Err(invalid_input(
            "draft_id must be the opaque identifier returned by create_draft",
        ));
    }
    Ok(())
}

fn content_text(content: MessageContent) -> String {
    match content {
        MessageContent::Text(text) => text,
    }
}

fn is_mine(summary: &SessionSummary, account: &str, device: &str) -> bool {
    match (&summary.local_account, &summary.local_device) {
        (Some(local_account), _) => local_account == account,
        (None, Some(local_device)) => local_device == device,
        (None, None) => false,
    }
}

impl ConversationCore {
    /// 打开或创建本地加密库（CORE-001 的最小切片）。
    ///
    /// 数据目录必须已存在（正式存储不隐式创建目录）；错误密钥/损坏库显式失败，
    /// 绝不重建同名空库。
    pub fn open(database_path: &str, master_key_hex: &str) -> anyhow::Result<Self> {
        if database_path.is_empty() {
            return Err(invalid_input("database_path must not be empty"));
        }
        let key = decode_master_key(master_key_hex)?;
        let store = open_store(
            StoreOptions::new(database_path, KeyProvenance::Raw32),
            key,
        )
        .map_err(anyhow::Error::new)?;
        // 事件总线随 open 初始化：容量固定且可测，溢出时由 poll 显式要求 resync。
        let events = EventBus::new(DEFAULT_EVENT_CAPACITY).map_err(anyhow::Error::new)?;
        Ok(Self {
            database_path: database_path.to_owned(),
            store: Mutex::new(Some(store)),
            events: Mutex::new(events),
            send_node: Mutex::new(None),
            send_group: Mutex::new(None),
        })
    }

    /// 库文件路径。
    pub fn database_path(&self) -> &str {
        &self.database_path
    }

    /// 是否仍在打开状态。
    pub fn is_open(&self) -> bool {
        match self.store.lock() {
            Ok(guard) => guard.is_some(),
            Err(_) => false,
        }
    }

    fn store_guard(&self) -> anyhow::Result<MutexGuard<'_, Option<Store>>> {
        self.store.lock().map_err(|_| closed())
    }

    /// 关闭内核：释放存储句柄与独占写锁，并发布一条 `core_closed` 事件。
    ///
    /// 重复调用是幂等的（只有真正从“打开”变成“关闭”的那一次才发布事件）。
    pub fn close(&self) {
        let mut was_open = false;
        if let Ok(mut guard) = self.store.lock() {
            was_open = guard.take().is_some();
        }
        if was_open
            && let Ok(mut events) = self.events.lock()
        {
            events.push(CoreEvent::CoreClosed);
        }
    }

    /// 事件总线句柄（内部使用；只在锁内短暂持有）。
    fn events_guard(&self) -> anyhow::Result<MutexGuard<'_, EventBus>> {
        self.events.lock().map_err(|_| closed())
    }

    /// CORE-009 + CORE-003：取消一条草稿，并把本次产生的状态变化事件并入内核事件总线。
    ///
    /// 单写者独占打开的存储不能同时被两个句柄持有，因此这里把 `Store` 临时交给
    /// [`SendService`]（桩端口 + 系统时钟），执行完**无论成败都把句柄放回**；
    /// 放回失败的唯一可能是 `SendService` 构造失败（此时句柄已被消费），对象随后
    /// 表现为关闭（`CORE_CLOSED`），错误照原样返回。
    ///
    /// 三态语义完全由 `SendService::cancel_draft` 决定：`CancelRequested` /
    /// `CancelTooLate` / `CancelUnknown`，本方法不重写、不猜测。
    pub fn cancel_draft(&mut self, draft_id: &str) -> anyhow::Result<CancelOutcome> {
        validate_local_draft_id(draft_id)?;
        let store = {
            let mut guard = self.store_guard()?;
            guard.take().ok_or_else(closed)?
        };
        let mut service = match SendService::new(
            store,
            StubSubmitter,
            StubCrypto,
            SystemClock,
            EVENT_SEND_WORKER,
        ) {
            Ok(service) => service,
            Err(error) => {
                // 句柄已经被 SendService 消费：对象进入关闭状态，错误显式返回。
                return Err(error
                    .context("cannot open the cancel session; the store was released"));
            }
        };
        let outcome = service.cancel_draft(draft_id);
        // 取走本次事件（序号从 0 开始；service 是本次新建的实例）。
        let mut events = Vec::new();
        if let Ok(batch) = service.events().poll(0, MAX_EVENT_BATCH) {
            events = batch.events;
        }
        let store = service.into_store().ok_or_else(closed)?;
        {
            let mut guard = self.store_guard()?;
            *guard = Some(store);
        }
        {
            let mut bus = self.events_guard()?;
            for event in events {
                bus.push(event);
            }
        }
        outcome
    }

    /// SND：保存节点配置（只保存，不发任何请求；重复配置覆盖上一次）。
    ///
    /// 端点必须是可解析的 http(s) 地址；`node_public_key_hex` 给出时必须是 64 位十六进制。
    /// 之后 `send_prepare`/`send_dispatch`/`send_retry`/`send_reconcile` 会用它装配真实运行时。
    pub fn configure_send_node(
        &mut self,
        endpoint_url: &str,
        node_id: &str,
        node_public_key_hex: Option<&str>,
    ) -> anyhow::Result<()> {
        let config = SendNodeConfig::new(endpoint_url, node_id, node_public_key_hex)
            .map_err(anyhow::Error::new)?;
        let mut guard = self.send_node.lock().map_err(|_| closed())?;
        *guard = Some(config);
        Ok(())
    }

    /// SND：显式指定发送所用的 MLS 群键（不设置时自动绑定本库唯一的持久 MLS 群）。
    ///
    /// 群键是 MLS group id 的小写十六进制（不透明标识，不是密钥材料）。
    pub fn configure_send_group(&mut self, mls_group_key: &str) -> anyhow::Result<()> {
        if mls_group_key.is_empty() || mls_group_key.len() > MAX_MLS_GROUP_KEY_BYTES {
            return Err(invalid_input(
                "mls group key must be a bounded identifier",
            ));
        }
        let mut guard = self.send_group.lock().map_err(|_| closed())?;
        *guard = Some(mls_group_key.to_owned());
        Ok(())
    }

    /// SND：当前是否已配置发送节点（不联网、不改状态）。
    pub fn send_node_configured(&self) -> bool {
        self.send_node
            .lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false)
    }

    /// SND：prepare（必要时恰好加密一次），返回状态投影。
    pub fn send_prepare(&mut self, draft_id: &str) -> anyhow::Result<SendStepView> {
        self.run_send_step(draft_id, SendStep::Prepare)
    }

    /// SND：dispatch（真实节点；只走三态，绝不把未知当成功）。
    pub fn send_dispatch(&mut self, draft_id: &str) -> anyhow::Result<SendStepView> {
        self.run_send_step(draft_id, SendStep::Dispatch)
    }

    /// SND：retry（拒绝后重建，新 SubmissionId）。
    pub fn send_retry(&mut self, draft_id: &str) -> anyhow::Result<SendStepView> {
        self.run_send_step(draft_id, SendStep::Retry)
    }

    /// SND：reconcile（结果未知时按 SubmissionId 对账）。
    pub fn send_reconcile(&mut self, draft_id: &str) -> anyhow::Result<SendStepView> {
        self.run_send_step(draft_id, SendStep::Reconcile)
    }

    /// SND 单步执行：装配真实运行时 → 执行 → 把本次事件并入内核总线 → **放回 Store 句柄**。
    ///
    /// 未配置节点时直接 `NODE_UNAVAILABLE`（不碰 Store、不联网）；装配失败时把句柄放回并
    /// 原样返回错误码（身份未入库 / MLS 群未加入 / 群歧义 / 节点配置非法）。
    fn run_send_step(
        &mut self,
        draft_id: &str,
        step: SendStep,
    ) -> anyhow::Result<SendStepView> {
        validate_local_draft_id(draft_id)?;
        let config = {
            let guard = self.send_node.lock().map_err(|_| closed())?;
            guard.clone().ok_or_else(|| {
                anyhow::Error::new(SyncError::new(
                    ErrorCode::NodeUnavailable,
                    "the send node is not configured",
                ))
            })?
        };
        // 草稿必须存在：在装配 MLS/节点之前拒掉“未知草稿”，错误码更精确。
        {
            let guard = self.store_guard()?;
            let store = guard.as_ref().ok_or_else(closed)?;
            let exists = store
                .read_draft(draft_id)
                .map_err(anyhow::Error::new)?
                .is_some();
            if !exists {
                return Err(anyhow::Error::new(SyncError::new(
                    ErrorCode::InvalidTarget,
                    "draft does not exist",
                )));
            }
        }
        let group_hint = {
            let guard = self.send_group.lock().map_err(|_| closed())?;
            guard.clone()
        };
        let store = {
            let mut guard = self.store_guard()?;
            guard.take().ok_or_else(closed)?
        };
        let mut runtime = match SendRuntime::open(store, &config, group_hint.as_deref()) {
            Ok(runtime) => runtime,
            Err(failure) => {
                let crate::send_runtime::SendOpenError { error, store } = failure;
                if let Some(store) = store {
                    let mut guard = self.store_guard()?;
                    *guard = Some(store);
                }
                return Err(error);
            }
        };
        let step_result = runtime.step(draft_id, step);
        // 本次运行时产生的事件（序号从 0 开始，因为 service 是本次新建的实例）。
        let mut produced = Vec::new();
        if let Ok(batch) = runtime.poll_events(0, MAX_EVENT_BATCH) {
            produced = batch.events;
        }
        let store = runtime.into_store().ok_or_else(closed)?;
        {
            let mut guard = self.store_guard()?;
            *guard = Some(store);
        }
        {
            let mut bus = self.events_guard()?;
            for event in produced {
                bus.push(event);
            }
        }
        step_result
    }

    /// CORE-003 的最小切片：按单调序号读取内核事件（不改变任何状态，重复读取幂等）。
    ///
    /// 游标早于缓冲起点或超前于本实例时返回 `resync_required = true` 与一条
    /// `resync_required` 事件，绝不静默丢事件；`limit` 必须在 `1..=MAX_EVENT_BATCH`。
    pub fn poll_events(&self, since_seq: u64, limit: u32) -> anyhow::Result<EventBatch> {
        self.events_guard()?
            .poll(since_seq, limit)
            .map_err(anyhow::Error::new)
    }

    /// CORE-007 的本地切片：持久化一条发送意图并返回 `queued` 草稿。
    ///
    /// 会话行不存在时以空标题补建（标题由后续已验证条目补写），因此
    /// “先建草稿再收到首条正式消息”的顺序也可用。
    pub fn create_draft(
        &mut self,
        conversation_id: &str,
        operation_id: &str,
        body: &str,
    ) -> anyhow::Result<DraftView> {
        let conversation = parse_conversation(conversation_id)?;
        let operation = parse_operation(operation_id)?;
        let content = MessageContent::text(body).map_err(anyhow::Error::new)?;
        let created_at = now_unix_ms()?;
        let request = ValidatedSendRequest {
            conversation_id: conversation.encode(),
            client_operation_id: operation.encode(),
            content,
            reply_to: None,
            origin: MessageOrigin::UserInput,
            created_at,
        };
        let body = body.to_owned();
        let mut guard = self.store_guard()?;
        let store = guard.as_mut().ok_or_else(closed)?;
        let mut transaction = store
            .begin_transaction(WriteScope::intent(&request.conversation_id))
            .map_err(anyhow::Error::new)?;
        let outcome = persist_draft_in_transaction(&mut transaction, request);
        match outcome {
            Ok(record) => {
                commit_transaction(transaction).map_err(anyhow::Error::new)?;
                Ok(DraftView {
                    draft_id: record.draft_id,
                    operation_id: record.operation_id,
                    conversation_id: record.conversation_id,
                    body,
                    state: record.state,
                    submission_id: None,
                })
            }
            Err(error) => {
                let _ = rollback_transaction(transaction);
                Err(anyhow::Error::new(error))
            }
        }
    }

    /// CORE-010 的本地切片：读取草稿状态；未知草稿返回 `None`。
    pub fn draft_status(&self, draft_id: &str) -> anyhow::Result<Option<DraftView>> {
        validate_local_draft_id(draft_id)?;
        let guard = self.store_guard()?;
        let store = guard.as_ref().ok_or_else(closed)?;
        let Some(record) = store.read_draft(draft_id).map_err(anyhow::Error::new)? else {
            return Ok(None);
        };
        let submission_id = store
            .draft_submission_id(&record.draft_id)
            .map_err(anyhow::Error::new)?;
        Ok(Some(DraftView {
            draft_id: record.draft_id,
            operation_id: record.operation_id,
            conversation_id: record.conversation_id,
            body: content_text(record.content),
            state: record.state,
            submission_id,
        }))
    }

    /// CORE-010：读取草稿的真实状态投影（无正文），未知草稿返回 `None`。
    ///
    /// 与 `draft_status` 的区别：本方法不解密正文，返回按钮态（`can_retry`/`can_cancel`）
    /// 与最新尝试信息，供 UI 与事件合并使用。
    pub fn get_draft_status(&self, draft_id: &str) -> anyhow::Result<Option<DraftStatusView>> {
        validate_local_draft_id(draft_id)?;
        let guard = self.store_guard()?;
        let store = guard.as_ref().ok_or_else(closed)?;
        draft_status_view(store, draft_id).map_err(anyhow::Error::new)
    }

    /// 写入一条已验证条目（STORAGE-008），返回可展示的消息行。
    ///
    /// 所有 ID 先按 PROTOCOL-001 规范文本校验：会话 `conv:1:<hex>`、消息
    /// `msg:1:<hex>`、作者账户 `acct:1:<hex>`、作者设备 `dev:1:<hex>`。
    pub fn apply_verified_entry(
        &mut self,
        input: VerifiedEntryInput,
    ) -> anyhow::Result<MessageRowView> {
        parse_conversation(&input.conversation_id)?;
        parse_message(&input.message_id)?;
        parse_account(&input.author_account)?;
        parse_device(&input.author_device)?;
        if input.title.len() > MAX_SESSION_TITLE_BYTES {
            return Err(invalid_input(format!(
                "title must not exceed {MAX_SESSION_TITLE_BYTES} bytes"
            )));
        }
        let mut guard = self.store_guard()?;
        let store = guard.as_mut().ok_or_else(closed)?;
        let applied = store
            .apply_verified_entry(input)
            .map_err(anyhow::Error::new)?;
        Ok(MessageRowView {
            message_id: applied.message_id,
            conversation_id: applied.conversation_id,
            author: applied.author_account,
            body: applied.body,
            mine: applied.mine,
            created_at_ms: u64::try_from(applied.created_at_ms).unwrap_or(0),
        })
    }

    /// 写入一条已验证条目并在同一事务内登记它引用的附件（ATTACHMENTS-008）。
    ///
    /// 与 `apply_verified_entry` 相同的 ID 校验；附件引用登记由 STORAGE-008 的事务保证
    /// 原子性：任一附件缺失/非法则整笔回滚（消息不落库、引用不留一半），重复落库幂等。
    pub fn apply_verified_entry_with_attachments(
        &mut self,
        input: VerifiedEntryInput,
        attachment_ids: Vec<String>,
    ) -> anyhow::Result<MessageRowView> {
        parse_conversation(&input.conversation_id)?;
        parse_message(&input.message_id)?;
        parse_account(&input.author_account)?;
        parse_device(&input.author_device)?;
        if input.title.len() > MAX_SESSION_TITLE_BYTES {
            return Err(invalid_input(format!(
                "title must not exceed {MAX_SESSION_TITLE_BYTES} bytes"
            )));
        }
        let mut guard = self.store_guard()?;
        let store = guard.as_mut().ok_or_else(closed)?;
        let applied = store
            .apply_verified_entry_with_attachments(input, attachment_ids)
            .map_err(anyhow::Error::new)?;
        Ok(MessageRowView {
            message_id: applied.message_id,
            conversation_id: applied.conversation_id,
            author: applied.author_account,
            body: applied.body,
            mine: applied.mine,
            created_at_ms: u64::try_from(applied.created_at_ms).unwrap_or(0),
        })
    }

    /// 某条消息引用的附件 ID 列表（有界、按 ID 升序），供界面展示消息附件。
    pub fn attachments_for_message(&self, message_id: &str) -> anyhow::Result<Vec<String>> {
        parse_message(message_id)?;
        let guard = self.store_guard()?;
        let store = guard.as_ref().ok_or_else(closed)?;
        store
            .message_attachment_refs_for_message(message_id)
            .map_err(anyhow::Error::new)
    }

    /// CORE-005 的本地切片：会话列表分页。
    pub fn list_conversations(
        &self,
        cursor: Option<&str>,
        limit: u32,
    ) -> anyhow::Result<ConversationPageView> {
        let guard = self.store_guard()?;
        let store = guard.as_ref().ok_or_else(closed)?;
        let page_cursor = cursor.map(PageCursor::from_text);
        let page = store
            .query_conversation_page(page_cursor.as_ref(), limit)
            .map_err(anyhow::Error::new)?;
        let mut items = Vec::with_capacity(page.items.len());
        for row in page.items {
            let summary = store
                .session_summary(&row.conversation_id)
                .map_err(anyhow::Error::new)?
                .unwrap_or_default();
            let updated_at_ms = summary.updated_at_ms.max(row.last_activity_at);
            items.push(ConversationView {
                conversation_id: row.conversation_id,
                title: summary.title,
                preview: summary.preview,
                unread_count: row.unread_count,
                updated_at_ms: u64::try_from(updated_at_ms).unwrap_or(0),
            });
        }
        Ok(ConversationPageView {
            items,
            next_cursor: page.next_cursor.map(|cursor| cursor.to_string()),
        })
    }

    /// CORE-006 的本地切片：单会话消息分页（含本机待发意图）。
    pub fn list_messages(
        &self,
        conversation_id: &str,
        cursor: Option<&str>,
        limit: u32,
    ) -> anyhow::Result<MessagePageView> {
        let conversation = parse_conversation(conversation_id)?;
        let conversation_text = conversation.encode();
        let guard = self.store_guard()?;
        let store = guard.as_ref().ok_or_else(closed)?;
        let summary = store
            .session_summary(&conversation_text)
            .map_err(anyhow::Error::new)?
            .unwrap_or_default();
        let page_cursor = cursor.map(PageCursor::from_text);
        let page = store
            .query_message_page(&conversation_text, page_cursor.as_ref(), limit)
            .map_err(anyhow::Error::new)?;
        let mut items = Vec::with_capacity(page.items.len());
        for view in page.items {
            items.push(message_row_view(&summary, conversation_text.clone(), view));
        }
        Ok(MessagePageView {
            items,
            next_cursor: page.next_cursor.map(|cursor| cursor.to_string()),
        })
    }

    /// 保存（或更新展示信息）本机身份账户，返回脱敏投影。
    ///
    /// 语义与 storage 一致：首次写入插入单行记录；同账户同种子同显示名/指纹幂等
    /// （不写行、不推进库版本）；同账户不同显示名更新展示信息（创建时间保留）；
    /// 另一个账户返回 `IDENTITY_ACCOUNT_CONFLICT`，绝不覆盖已绑定身份。
    pub fn save_identity_account(
        &mut self,
        input: IdentityAccountInput,
    ) -> anyhow::Result<IdentityAccountView> {
        let mut account_seed = decode_seed_hex(&input.account_seed_hex, "account_seed_hex")?;
        let mut device_seed = decode_seed_hex(&input.device_seed_hex, "device_seed_hex")?;
        let display_name = input.display_name.trim().to_owned();
        let created_at_ms = if input.created_at_ms == 0 {
            u64::try_from(now_unix_ms()?)
                .map_err(|_| invalid_input("system clock is outside the supported range"))?
        } else {
            input.created_at_ms
        };
        let account = LocalAccount::from_seed(&display_name, created_at_ms, &account_seed)
            .map_err(anyhow::Error::new)?;
        let account_id = account.account_id();
        let anchor = account.trust_anchor();
        let device = DeviceKeys::from_seed(&device_seed);
        let device_id = device.device_id(account_id);
        let storage_input = chat_storage::formal::IdentityAccountInput {
            account_id: account_id.encode(),
            device_id: device_id.encode(),
            account_public_key: *anchor.account_public_key(),
            device_public_key: device.public_key(),
            account_seed: account_seed.to_vec(),
            device_seed: device_seed.to_vec(),
            display_name: account.display_name().to_owned(),
            fingerprint: *account.fingerprint().as_bytes(),
            created_at_ms: i64::try_from(created_at_ms)
                .map_err(|_| invalid_input("created_at_ms is outside the supported range"))?,
        };
        account_seed.fill(0);
        device_seed.fill(0);
        let mut guard = self.store_guard()?;
        let store = guard.as_mut().ok_or_else(closed)?;
        let record = store
            .save_identity_account(storage_input)
            .map_err(anyhow::Error::new)?;
        Ok(identity_account_view(record))
    }

    /// 可信恢复路径：从受保护存储读回种子并重建账户/设备句柄。
    ///
    /// 种子只在本函数内解码使用（解码缓冲立即清零），调用方拿到的是可签名的
    /// [`LocalAccount`] 与 [`DeviceKeys`]；若派生出的账户/设备 ID 与库中记录不一致，
    /// 说明记录被篡改或换了密钥，直接失败（绝不降级为“新身份”）。
    pub fn open_persisted_identity(&self) -> anyhow::Result<Option<PersistedIdentity>> {
        let guard = self.store_guard()?;
        let store = guard.as_ref().ok_or_else(closed)?;
        let Some(seeds) = store.load_identity_seeds().map_err(anyhow::Error::new)? else {
            return Ok(None);
        };
        let account_seed: [u8; 32] = seeds
            .account_seed
            .as_slice()
            .try_into()
            .map_err(|_| invalid_input("stored account seed has an unexpected length"))?;
        let device_seed: [u8; 32] = seeds
            .device_seed
            .as_slice()
            .try_into()
            .map_err(|_| invalid_input("stored device seed has an unexpected length"))?;
        let created_at_ms = u64::try_from(seeds.created_at_ms.max(0))
            .map_err(|_| invalid_input("stored created_at_ms is outside the supported range"))?;
        let account = LocalAccount::from_seed(&seeds.display_name, created_at_ms, &account_seed)
            .map_err(anyhow::Error::new)?;
        let device = DeviceKeys::from_seed(&device_seed);
        if account.account_id().encode() != seeds.account_id {
            return Err(invalid_input(
                "stored account seed does not match the persisted account id",
            ));
        }
        let device_id = device.device_id(account.account_id());
        if device_id.encode() != seeds.device_id {
            return Err(invalid_input(
                "stored device seed does not match the persisted device id",
            ));
        }
        Ok(Some(PersistedIdentity {
            account,
            device,
            device_id,
        }))
    }

    /// 读取本机身份账户投影；未保存时返回 `None`。
    pub fn load_identity_account(&self) -> anyhow::Result<Option<IdentityAccountView>> {
        let guard = self.store_guard()?;
        let store = guard.as_ref().ok_or_else(closed)?;
        let record = store
            .load_identity_account()
            .map_err(anyhow::Error::new)?;
        Ok(record.map(identity_account_view))
    }

    /// 按 `contact_id`（由对端账户派生）幂等写入联系人。
    ///
    /// 已存在的联系人必须先通过 identity 的冻结状态机（`transition_allowed`），
    /// 非法迁移返回 `INVALID_STATE_TRANSITION`，绝不把状态静默归一。
    pub fn upsert_contact(
        &mut self,
        request: ContactUpsertRequest,
    ) -> anyhow::Result<ContactView> {
        let account = parse_account(&request.account_id)?;
        let fingerprint = Fingerprint::parse(&request.fingerprint).map_err(anyhow::Error::new)?;
        let state = parse_contact_state(&request.state)?;
        let display_name = request.display_name.trim().to_owned();
        if display_name.is_empty() || display_name.len() > MAX_DISPLAY_NAME_BYTES {
            return Err(invalid_input(format!(
                "display_name must be 1..={MAX_DISPLAY_NAME_BYTES} bytes"
            )));
        }
        if display_name.chars().any(char::is_control) {
            return Err(invalid_input(
                "display_name must not contain control characters",
            ));
        }
        let account_text = account.encode();
        let mut guard = self.store_guard()?;
        let store = guard.as_mut().ok_or_else(closed)?;
        if let Some(existing) = store
            .read_contact(&account_text)
            .map_err(anyhow::Error::new)?
        {
            let previous = parse_contact_state(&existing.state)?;
            if !transition_allowed(previous, state) {
                return Err(anyhow::Error::new(IdentityError::InvalidStateTransition {
                    from: previous,
                    to: state,
                }));
            }
        }
        let storage_input = chat_storage::formal::ContactUpsertInput {
            account_id: account_text,
            display_name,
            fingerprint: *fingerprint.as_bytes(),
            state: state.as_str().to_owned(),
            updated_at_ms: i64::try_from(request.updated_at_ms)
                .map_err(|_| invalid_input("updated_at_ms is outside the supported range"))?,
        };
        let record = store
            .upsert_contact(storage_input)
            .map_err(anyhow::Error::new)?;
        Ok(contact_view(record))
    }

    /// 列出全部联系人（按事件时间降序，其次联系人键升序）。
    pub fn list_contacts(&self) -> anyhow::Result<Vec<ContactView>> {
        let guard = self.store_guard()?;
        let store = guard.as_ref().ok_or_else(closed)?;
        let records = store.list_contacts().map_err(anyhow::Error::new)?;
        Ok(records.into_iter().map(contact_view).collect())
    }

    /// 一次性消费邀请；同一 `invite_id` 只允许成功一次。
    ///
    /// 第二次调用（以及同 ID 不同摘要）返回 `INVALID_STATE_TRANSITION`，且不写行。
    /// 本方法只记录“已消费”，**不验证邀请本身**：邀请的签名/期限/信任锚校验由
    /// IDENTITY-004/005（`chat_identity::verify_invite`）在调用前完成。
    pub fn consume_invite(&mut self, invite_id: &str) -> anyhow::Result<ConsumedInviteView> {
        let digest = invite_digest(invite_id);
        let consumed_at_ms = now_unix_ms()?;
        let mut guard = self.store_guard()?;
        let store = guard.as_mut().ok_or_else(closed)?;
        let record = store
            .consume_invite(invite_id, &digest, consumed_at_ms)
            .map_err(anyhow::Error::new)?;
        Ok(ConsumedInviteView {
            invite_id: record.invite_id,
            consumed_at_ms: u64::try_from(record.consumed_at_ms).unwrap_or(0),
        })
    }

    /// 由邀请口令文本派生稳定的一次性邀请 ID（供 `consume_invite` 使用）。
    ///
    /// 只校验前缀与长度，不验证签名/期限：派生 ID 与校验口令是两件事，后者属于
    /// IDENTITY-004/005。同一口令永远得到同一 ID。
    pub fn invite_id_for_token(invite_token: &str) -> anyhow::Result<String> {
        let token = invite_token.trim();
        if !token.starts_with(chat_identity::invite::INVITE_TOKEN_VERSION) {
            return Err(invalid_input(
                "invite token must start with the inv1 version prefix",
            ));
        }
        if token.len() > chat_identity::invite::MAX_INVITE_TOKEN_BYTES {
            return Err(invalid_input(format!(
                "invite token exceeds {} bytes",
                chat_identity::invite::MAX_INVITE_TOKEN_BYTES
            )));
        }
        let digest = domain_digest(b"chat/core/invite-token-id/v1", token.as_bytes());
        Ok(format!("invite-{}", lower_hex(&digest[..16])))
    }

    // ---------------------------------------------------------------- 群治理（GROUPS）

    /// GROUPS-001：建立一个小群（创建者 + 引导成员），返回真实群投影。
    ///
    /// - 本机 MLS 设备在建群时**新建**（群还不存在，无法从库恢复），群状态随即写入正式库；
    /// - 引导成员需要同进程设备句柄（crypto 只有双人引导路径），因此其 MLS 设备在这里临时创建：
    ///   真实邀请加入属 GROUPS-004 `accept_group_invite`（未实现，见 groups.md）；
    /// - `local_credential` 为空时使用**持久身份**的设备 ID（未入库身份 → `INVALID_STATE_TRANSITION`）；
    /// - `members` 是追加成员：调用方提供真实 KeyPackage，走 CRYPTO-005 Add 提交（结果是 pending）。
    pub fn group_create(
        &mut self,
        name: &str,
        local_credential: &str,
        bootstrap_credential: &str,
        members: Vec<GroupMemberKeyPackage>,
    ) -> anyhow::Result<GroupView> {
        let local = if local_credential.trim().is_empty() {
            self.persisted_device_id()?
        } else {
            local_credential.trim().to_owned()
        };
        let mut device = MlsDevice::new(&local).map_err(|_| {
            anyhow::Error::new(crate::groups::GroupError::from(
                chat_storage::formal::StorageError::StateCorrupted(
                    "the local MLS device could not be created".to_owned(),
                ),
            ))
        })?;
        let peer = MlsDevice::new(bootstrap_credential.trim()).map_err(|_| {
            anyhow::Error::new(crate::groups::GroupError::from(
                chat_storage::formal::StorageError::StateCorrupted(
                    "the bootstrap MLS device could not be created".to_owned(),
                ),
            ))
        })?;
        let request = CreateGroupRequest {
            name: name.to_owned(),
            local_credential: local,
            policy_version: GROUP_POLICY_VERSION,
            members,
        };
        let bootstrap = GroupBootstrapPeer {
            credential: bootstrap_credential.trim().to_owned(),
            device: &peer,
        };
        let mut guard = self.store_guard()?;
        let store = guard.as_mut().ok_or_else(closed)?;
        crate::groups::create_group(&mut device, &bootstrap, store, &request)
            .map_err(anyhow::Error::new)
    }

    /// GROUPS-002：读取正式群快照（不联网）。
    pub fn group_snapshot(&self, group_id: &str) -> anyhow::Result<GroupView> {
        let guard = self.store_guard()?;
        let store = guard.as_ref().ok_or_else(closed)?;
        crate::groups::group_snapshot(store, group_id).map_err(anyhow::Error::new)
    }

    /// GROUPS-003：变更成员角色（群主才能改；不能改群主；真实 MLS 控制提交 + pending）。
    ///
    /// `role` 用稳定文本（`owner`/`admin`/`member`），未知文本显式 `INVALID_INPUT`。
    pub fn group_set_role(
        &mut self,
        group_id: &str,
        actor_credential: &str,
        target_credential: &str,
        role: &str,
    ) -> anyhow::Result<GroupView> {
        let role = group_role_from_code(role)
            .ok_or_else(|| invalid_input("role must be one of: owner, admin, member"))?;
        self.with_group_device(group_id, |device, store| {
            crate::groups::set_role(device, store, group_id, actor_credential, target_credential, role)
        })
    }

    /// GROUPS-004：设置成员禁言（纯策略层，无 MLS 提交）。
    pub fn group_set_muted(
        &mut self,
        group_id: &str,
        actor_credential: &str,
        target_credential: &str,
        muted: bool,
    ) -> anyhow::Result<GroupView> {
        let mut guard = self.store_guard()?;
        let store = guard.as_mut().ok_or_else(closed)?;
        crate::groups::set_muted(store, group_id, actor_credential, target_credential, muted)
            .map_err(anyhow::Error::new)
    }

    /// GROUPS-005：移除确认成员（真实 MLS 移除提交 + pending）。
    pub fn group_remove_member(
        &mut self,
        group_id: &str,
        actor_credential: &str,
        target_credential: &str,
    ) -> anyhow::Result<GroupView> {
        self.with_group_device(group_id, |device, store| {
            crate::groups::remove_member(device, store, group_id, actor_credential, target_credential)
        })
    }

    /// GROUPS-006：改名（同一套名称校验；同名幂等）。
    pub fn group_rename(
        &mut self,
        group_id: &str,
        actor_credential: &str,
        new_name: &str,
    ) -> anyhow::Result<GroupView> {
        let mut guard = self.store_guard()?;
        let store = guard.as_mut().ok_or_else(closed)?;
        crate::groups::rename_group(store, group_id, actor_credential, new_name)
            .map_err(anyhow::Error::new)
    }

    /// 接受并合并当前控制提交（CRYPTO-007；本地接受证据 ≠ 节点回执）。
    pub fn group_accept_pending(&mut self, group_id: &str) -> anyhow::Result<GroupView> {
        let mut guard = self.store_guard()?;
        let store = guard.as_mut().ok_or_else(closed)?;
        let mut device = crate::groups::load_group_device(store, group_id).map_err(anyhow::Error::new)?;
        crate::groups::accept_pending_control(&mut device, store, group_id)
            .map_err(anyhow::Error::new)
    }

    /// 丢弃当前控制提交并回滚（CRYPTO-008；policy_version 不回退）。
    pub fn group_discard_pending(
        &mut self,
        group_id: &str,
        reason: &str,
    ) -> anyhow::Result<GroupView> {
        let mut guard = self.store_guard()?;
        let store = guard.as_mut().ok_or_else(closed)?;
        crate::groups::discard_pending_control(store, group_id, reason).map_err(anyhow::Error::new)
    }

    // ------------------------------------------------ 群治理经节点回执（NODE-006）

    /// NODE-006：把本机 pending 控制提交送到节点，返回**真实回执**（回执 + 候选原文 + 提交时的 head）。
    ///
    /// - 设备签名钥与持久身份同源（`load_identity_seeds` → `signing_key_from_seed`），因此节点看到的
    ///   设备 ID 与库里一致；未入库身份 → `INVALID_STATE_TRANSITION`；
    /// - `endpoint_url` 必须是可解析的 http(s) URL；`node_id` 是节点身份；`node_public_key_hex` 可选
    ///   （给了就固定节点公钥，回执验签仍以回执自带的 head 公钥为准）；
    /// - 提交前的全部前置（群快照、pending、群键、epoch 绑定、节点 head CAS）由
    ///   crate::group_node::submit_pending_control 执行，任一不满足都显式失败且不发请求；
    /// - 传输/超时统一映射为 `NODE_UNAVAILABLE`。
    pub fn group_submit_control(
        &mut self,
        group_id: &str,
        endpoint_url: &str,
        node_id: &str,
        node_public_key_hex: Option<&str>,
    ) -> anyhow::Result<ControlReceiptView> {
        let mut guard = self.store_guard()?;
        let store = guard.as_mut().ok_or_else(closed)?;
        let seeds = store
            .load_identity_seeds()
            .map_err(anyhow::Error::new)?
            .ok_or_else(|| {
                anyhow::Error::new(SyncError::new(
                    ErrorCode::InvalidStateTransition,
                    "the local identity is not enrolled in this database",
                ))
            })?;
        let device_seed: [u8; 32] = seeds.device_seed.as_slice().try_into().map_err(|_| {
            anyhow::Error::new(SyncError::new(
                ErrorCode::StateCorrupted,
                "the persisted device seed is not 32 bytes",
            ))
        })?;
        let signing_key = chat_protocol::signing_key_from_seed(&device_seed);
        let mut endpoint =
            chat_node_client::NodeEndpoint::new(endpoint_url, node_id).map_err(|_| {
                anyhow::Error::new(SyncError::new(
                    ErrorCode::InvalidTarget,
                    "node endpoint must be a parseable http(s) url",
                ))
            })?;
        if let Some(hex_key) = node_public_key_hex {
            let key: [u8; 32] = chat_node_services::canonical::hex_decode::<32>(
                "node public key",
                hex_key.trim(),
            )
            .map_err(|_| {
                anyhow::Error::new(SyncError::new(
                    ErrorCode::InvalidTarget,
                    "node public key must be 32 bytes of lowercase hex",
                ))
            })?;
            endpoint = endpoint.with_node_public_key(key);
        }
        let client =
            chat_node_client::NodeClient::new(endpoint, seeds.device_id.clone(), signing_key)
                .map_err(|_| {
                    anyhow::Error::new(SyncError::new(
                        ErrorCode::InvalidTarget,
                        "node client could not be created from the persisted device identity",
                    ))
                })?;
        let device = crate::groups::load_group_device(store, group_id).map_err(anyhow::Error::new)?;
        let receipt = crate::group_node::submit_pending_control(&device, store, group_id, &client)
            .map_err(anyhow::Error::new)?;
        Ok(ControlReceiptView::from_receipt(receipt))
    }

    /// NODE-006 接受侧：用**真实回执**接受并合并 pending 控制提交。
    ///
    /// 回执绑定（候选正文逐字节等于本 pending 的 Commit、提交/作者/群/节点身份、节点签名、
    /// 日志链摘要）全部复核通过后才合并；任何不符都显式失败且 epoch/成员不变。
    pub fn group_accept_with_receipt(
        &mut self,
        group_id: &str,
        receipt: &ControlReceiptView,
    ) -> anyhow::Result<GroupView> {
        let mut guard = self.store_guard()?;
        let store = guard.as_mut().ok_or_else(closed)?;
        let mut device =
            crate::groups::load_group_device(store, group_id).map_err(anyhow::Error::new)?;
        let rebuilt = receipt.to_control_receipt()?;
        crate::group_node::accept_pending_control_with_receipt(
            &mut device,
            store,
            group_id,
            &rebuilt,
        )
        .map_err(anyhow::Error::new)
    }

    /// 持久身份的设备 ID（未入库身份 → `INVALID_STATE_TRANSITION`）。
    fn persisted_device_id(&self) -> anyhow::Result<String> {
        let guard = self.store_guard()?;
        let store = guard.as_ref().ok_or_else(closed)?;
        let seeds = store
            .load_identity_seeds()
            .map_err(anyhow::Error::new)?
            .ok_or_else(|| {
                anyhow::Error::new(SyncError::new(
                    ErrorCode::InvalidStateTransition,
                    "the local identity is not enrolled in this database",
                ))
            })?;
        Ok(seeds.device_id)
    }

    /// 群操作通用装配：从**持久库**恢复本机 MLS 设备，再执行动作（沿用 groups 的冻结码）。
    fn with_group_device<T>(
        &mut self,
        group_id: &str,
        action: impl FnOnce(&mut MlsDevice, &mut Store) -> crate::groups::GroupResult<T>,
    ) -> anyhow::Result<T> {
        let mut guard = self.store_guard()?;
        let store = guard.as_mut().ok_or_else(closed)?;
        let mut device =
            crate::groups::load_group_device(store, group_id).map_err(anyhow::Error::new)?;
        action(&mut device, store).map_err(anyhow::Error::new)
    }
}

/// 节点回执的**公开副本**（不含任何秘密材料：全是节点与协议本来就要公开的字段）。
///
/// 三段分别对应 NODE-006 的证据三要素：节点签名回执、被提交的候选原文、提交时的节点 head 快照。
/// 三者必须一起流转——只保存回执无法排除“拿 A 的回执去接受 B 的提交”。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ControlReceiptView {
    /// 节点签名回执。
    pub receipt: ControlAppendView,
    /// 被提交的候选原文（kind = control，正文是 pending Commit 的十六进制）。
    pub candidate: ControlCandidateView,
    /// 提交时读到的节点 head 快照（节点身份与 CAS 依据）。
    pub head: ControlHeadSnapshotView,
}

impl ControlReceiptView {
    /// 由 group_node 的真实回执构造公开副本。
    pub fn from_receipt(receipt: crate::group_node::ControlReceipt) -> Self {
        let metadata = receipt.candidate.control_metadata.clone();
        Self {
            receipt: ControlAppendView {
                group_id: receipt.receipt.group_id,
                submission_id: receipt.receipt.submission_id,
                author_device_id: receipt.receipt.author_device_id,
                seq: receipt.receipt.seq,
                candidate_hash: receipt.receipt.candidate_hash,
                previous_hash: receipt.receipt.previous_hash,
                log_hash: receipt.receipt.log_hash,
                accepted_at: receipt.receipt.accepted_at,
                node_id: receipt.receipt.node_id,
                node_public_key: receipt.receipt.node_public_key,
                signature: receipt.receipt.signature,
            },
            candidate: ControlCandidateView {
                protocol_major: receipt.candidate.protocol_major,
                protocol_minor: receipt.candidate.protocol_minor,
                submission_id: receipt.candidate.submission_id,
                group_id: receipt.candidate.group_id,
                author_device_id: receipt.candidate.author_device_id,
                kind: receipt.candidate.kind,
                expected_control_head: receipt.candidate.expected_control_head,
                policy_version: receipt.candidate.policy_version,
                mls_epoch: receipt.candidate.mls_epoch,
                encrypted_payload: receipt.candidate.encrypted_payload,
                operation: metadata.as_ref().map(|item| item.operation.clone()),
                target_digest: metadata.as_ref().map(|item| item.target_digest.clone()),
            },
            head: ControlHeadSnapshotView {
                group_id: receipt.head.group_id,
                coordinator_id: receipt.head.coordinator_id,
                seq: receipt.head.seq,
                log_hash: receipt.head.log_hash,
                control_head: receipt.head.control_head,
                policy_version: receipt.head.policy_version,
                mls_epoch: receipt.head.mls_epoch,
                node_id: receipt.head.node_id,
                node_public_key: receipt.head.node_public_key,
                signature: receipt.head.signature,
            },
        }
    }

    /// 还原成 group_node 的证据三要素（字段原样搬运，验签与绑定校验在 group_node 侧完成）。
    pub fn to_control_receipt(&self) -> anyhow::Result<crate::group_node::ControlReceipt> {
        let metadata = match (&self.candidate.operation, &self.candidate.target_digest) {
            (Some(operation), Some(target_digest)) => {
                Some(chat_node_services::group::CandidateControlMetadata {
                    operation: operation.clone(),
                    target_digest: target_digest.clone(),
                })
            }
            (None, None) => None,
            _ => {
                return Err(anyhow::Error::new(SyncError::new(
                    ErrorCode::InvalidCandidate,
                    "control metadata must carry both operation and target digest",
                )));
            }
        };
        Ok(crate::group_node::ControlReceipt {
            receipt: chat_node_services::group::AppendResult {
                group_id: self.receipt.group_id.clone(),
                submission_id: self.receipt.submission_id.clone(),
                author_device_id: self.receipt.author_device_id.clone(),
                seq: self.receipt.seq,
                candidate_hash: self.receipt.candidate_hash.clone(),
                previous_hash: self.receipt.previous_hash.clone(),
                log_hash: self.receipt.log_hash.clone(),
                accepted_at: self.receipt.accepted_at,
                node_id: self.receipt.node_id.clone(),
                node_public_key: self.receipt.node_public_key.clone(),
                signature: self.receipt.signature.clone(),
            },
            candidate: chat_node_services::group::GroupCandidate {
                protocol_major: self.candidate.protocol_major,
                protocol_minor: self.candidate.protocol_minor,
                submission_id: self.candidate.submission_id.clone(),
                group_id: self.candidate.group_id.clone(),
                author_device_id: self.candidate.author_device_id.clone(),
                kind: self.candidate.kind.clone(),
                expected_control_head: self.candidate.expected_control_head.clone(),
                policy_version: self.candidate.policy_version,
                mls_epoch: self.candidate.mls_epoch,
                encrypted_payload: self.candidate.encrypted_payload.clone(),
                control_metadata: metadata,
            },
            head: chat_node_services::group::SignedLogHead {
                group_id: self.head.group_id.clone(),
                coordinator_id: self.head.coordinator_id.clone(),
                seq: self.head.seq,
                log_hash: self.head.log_hash.clone(),
                control_head: self.head.control_head.clone(),
                policy_version: self.head.policy_version,
                mls_epoch: self.head.mls_epoch,
                node_id: self.head.node_id.clone(),
                node_public_key: self.head.node_public_key.clone(),
                signature: self.head.signature.clone(),
            },
        })
    }

    /// 节点分配的提交标识（PROTOCOL-001 submission 文本）。
    pub fn submission_id(&self) -> &str {
        &self.receipt.submission_id
    }

    /// 节点日志序号。
    pub fn seq(&self) -> u64 {
        self.receipt.seq
    }

    /// 该条目之后的节点日志链摘要。
    pub fn log_hash(&self) -> &str {
        &self.receipt.log_hash
    }
}

/// 节点签名回执（原样字段）。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ControlAppendView {
    /// 群。
    pub group_id: String,
    /// 提交标识。
    pub submission_id: String,
    /// 作者设备。
    pub author_device_id: String,
    /// 节点日志序号（从 1 开始）。
    pub seq: u64,
    /// 候选全文摘要。
    pub candidate_hash: String,
    /// 前序日志摘要。
    pub previous_hash: String,
    /// 本条之后的日志链摘要。
    pub log_hash: String,
    /// 节点接受时间（Unix 秒）。
    pub accepted_at: u64,
    /// 节点身份。
    pub node_id: String,
    /// 节点公钥（公开材料）。
    pub node_public_key: String,
    /// 节点签名（小写 hex）。
    pub signature: String,
}

/// 被提交的控制候选原文（原样字段）。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ControlCandidateView {
    /// 协议主版本。
    pub protocol_major: u8,
    /// 协议次版本。
    pub protocol_minor: u8,
    /// 提交标识。
    pub submission_id: String,
    /// 群。
    pub group_id: String,
    /// 作者设备。
    pub author_device_id: String,
    /// 候选类别（本路径恒为 `control`）。
    pub kind: String,
    /// 期望控制头（CAS）。
    pub expected_control_head: String,
    /// 期望策略版本。
    pub policy_version: u64,
    /// 期望 MLS epoch。
    pub mls_epoch: u64,
    /// 密文（pending Commit 的小写 hex）。
    pub encrypted_payload: String,
    /// 公开控制操作种类。
    pub operation: Option<String>,
    /// 操作目标摘要（本切片用提交摘要）。
    pub target_digest: Option<String>,
}

/// 提交时读到的节点 head 快照（原样字段）。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ControlHeadSnapshotView {
    /// 群。
    pub group_id: String,
    /// 协调节点身份。
    pub coordinator_id: String,
    /// 日志序号。
    pub seq: u64,
    /// 链尾摘要。
    pub log_hash: String,
    /// 控制头。
    pub control_head: String,
    /// 策略版本。
    pub policy_version: u64,
    /// MLS epoch。
    pub mls_epoch: u64,
    /// 节点身份。
    pub node_id: String,
    /// 节点公钥（公开材料）。
    pub node_public_key: String,
    /// 节点签名（小写 hex）。
    pub signature: String,
}

/// 群角色 → 稳定文本（`owner` / `admin` / `member`），供 FFI/UI 投影。
pub fn group_role_code(role: MemberRole) -> &'static str {
    match role {
        MemberRole::Owner => "owner",
        MemberRole::Admin => "admin",
        MemberRole::Member => "member",
    }
}

/// 稳定文本 → 群角色；未知文本返回 `None`（调用方显式 `INVALID_INPUT`）。
pub fn group_role_from_code(text: &str) -> Option<MemberRole> {
    match text.trim() {
        "owner" => Some(MemberRole::Owner),
        "admin" => Some(MemberRole::Admin),
        "member" => Some(MemberRole::Member),
        _ => None,
    }
}

fn domain_digest(domain: &[u8], value: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
    let digest = hasher.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(digest.as_slice());
    bytes
}

fn lower_hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        text.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    text
}

fn invite_digest(invite_id: &str) -> [u8; 32] {
    domain_digest(b"chat/core/invite-consumption/v1", invite_id.as_bytes())
}

fn parse_contact_state(text: &str) -> anyhow::Result<ContactState> {
    match text {
        "requested" => Ok(ContactState::Requested),
        "accepted" => Ok(ContactState::Accepted),
        "declined" => Ok(ContactState::Declined),
        "blocked" => Ok(ContactState::Blocked),
        "identity_changed" => Ok(ContactState::IdentityChanged),
        _ => Err(invalid_input(
            "contact state must be requested/accepted/declined/blocked/identity_changed",
        )),
    }
}

fn identity_account_view(
    record: chat_storage::formal::IdentityAccountRecord,
) -> IdentityAccountView {
    IdentityAccountView {
        account_id: record.account_id,
        device_id: record.device_id,
        display_name: record.display_name,
        fingerprint: Fingerprint::from_bytes(record.fingerprint).grouped(),
        created_at_ms: u64::try_from(record.created_at_ms).unwrap_or(0),
        already_existed: record.already_existed,
    }
}

fn contact_view(record: chat_storage::formal::ContactRecord) -> ContactView {
    ContactView {
        contact_id: record.contact_id,
        account_id: record.account_id,
        display_name: record.display_name,
        fingerprint: Fingerprint::from_bytes(record.fingerprint).grouped(),
        state: record.state,
        updated_at_ms: u64::try_from(record.updated_at_ms).unwrap_or(0),
        already_existed: record.already_existed,
    }
}

fn message_row_view(
    summary: &SessionSummary,
    conversation_id: String,
    view: MessageView,
) -> MessageRowView {
    let (author, mine) = match (&view.author_account_id, &view.author_device_id) {
        (Some(account), Some(device)) => (account.clone(), is_mine(summary, account, device)),
        // 本机待发意图没有远端作者：一定是本机起草，作者取已知的本机身份。
        _ => (
            summary.local_account.clone().unwrap_or_default(),
            true,
        ),
    };
    MessageRowView {
        message_id: view.message_id,
        conversation_id,
        author,
        body: content_text(view.content),
        mine,
        created_at_ms: u64::try_from(view.display_created_at).unwrap_or(0),
    }
}

fn persist_draft_in_transaction(
    transaction: &mut chat_storage::formal::CoreTransaction<'_>,
    request: ValidatedSendRequest,
) -> chat_storage::formal::Result<chat_storage::formal::DraftRecord> {
    materialize_session(transaction, &request.conversation_id, "")?;
    persist_send_intent(transaction, request)
}


#[cfg(test)]
mod stub_ports {
    use super::{StubCrypto, StubSubmitter, UNWIRED_MLS_EPOCH};
    use crate::send_orchestrator::{CryptoEngine, NodeSubmitter, SubmitAttempt};
    use chat_sync::error::ErrorCode;

    #[test]
    fn stub_ports_fail_explicitly_never_fake_success() {
        let submitter = StubSubmitter;
        let attempt = SubmitAttempt {
            submission_id: "sub:1:0000000000000000000000000000000000000000000000000000000000000001"
                .to_owned(),
            conversation_id: "conv:1:0000000000000000000000000000000000000000000000000000000000000001"
                .to_owned(),
            mls_epoch: 1,
            ciphertext: vec![1, 2, 3],
        };
        // 未接真实节点：提交/查询必须显式失败，绝不伪造 Accepted。
        assert_eq!(
            submitter.submit(&attempt).unwrap_err().code(),
            ErrorCode::NetworkUnavailable
        );
        assert_eq!(
            submitter.query(&attempt.submission_id).unwrap_err().code(),
            ErrorCode::NetworkUnavailable
        );
        let mut crypto = StubCrypto;
        // epoch 只是 plan 身份，不是真实群状态；真正加密必须显式失败。
        assert_eq!(
            crypto.epoch(&attempt.conversation_id).expect("stub epoch"),
            UNWIRED_MLS_EPOCH
        );
        assert_eq!(
            crypto
                .encrypt(&attempt.conversation_id, "draft-1", "body")
                .unwrap_err()
                .code(),
            ErrorCode::ProtocolUnsupported
        );
    }
}

