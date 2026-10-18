//! 本地加密存储的 UniFFI 对象：Kotlin 侧的唯一持久真相入口。
//!
//! 与既有 `bridge.rs`（P0 临时实验对象）完全分开：本对象只暴露正式加密存储的
//! 竖直切片（草稿 / 已验证条目 / 会话与消息分页），不返回任何密码学句柄或数据库对象。
//!
//! 契约（与 docs/implementation/bridge.md 的记录一致）：
//! - `LocalStoreFfi::open(database_path, master_key_hex)`：数据目录必须已存在；
//!   主密钥为 64 位十六进制（32 字节）；错误密钥/损坏库显式失败，绝不重建空库；
//! - 所有 ID 参数按 PROTOCOL-001 规范文本校验：会话 `conv:1:<64 hex>`、消息
//!   `msg:1:<64 hex>`、操作 `op:1:<64 hex>`、账户 `acct:1:<64 hex>`、
//!   设备 `dev:1:<64 hex>`；草稿 ID 是 storage 派生的本地标识，必须原样回传；
//! - 时间戳在 FFI 边界是 `u64`（UniFFI 生成 Kotlin `ULong`）；
//! - 失败一律返回 `FfiError::Store { code, reason }`：`code` 是稳定错误码
//!   （STORAGE_`*` / INVALID_INPUT / INVALID_CURSOR / CORE_CLOSED 等），`reason`
//!   已截断脱敏，UI 不解析它。
//!
//! 线程安全：`ConversationCore` 内部的 `Store` 不是 `Sync`，因此用
//! `std::sync::Mutex` 串行化调用（UniFFI 要求对象可跨线程共享）。

use std::sync::{Arc, Mutex, MutexGuard};

use chat_core::core_events::{CoreEvent, EventBatch as CoreEventBatch};
use chat_core::send_orchestrator::CancelOutcome;
use chat_core::send_runtime::SendStepView as CoreSendStepView;
use chat_core::service::{
    self, ConversationCore, ConversationPageView as CoreConversationPageView,
    ConversationView as CoreConversationView, DraftView as CoreDraftView,
    MessagePageView as CoreMessagePageView, MessageRowView as CoreMessageRowView,
    VerifiedEntryInput,
};

/// FFI 层稳定错误。Kotlin 侧可 `catch (e: FfiException.Store)` 并读取 `code`/`reason`。
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    /// 存储或内核拒绝：`code` 是稳定错误码，`reason` 是脱敏后的可读原因。
    #[error("{code}: {reason}")]
    Store { code: String, reason: String },
}

/// 把内核的 `anyhow::Error` 投影成 FFI 错误：本 crate 不需要依赖 `anyhow`，
/// 由宏在调用点按类型推断完成。
macro_rules! failure {
    ($error:expr) => {{
        let (code, reason) = service::error_projection(&$error);
        FfiError::Store {
            code: code.to_owned(),
            reason,
        }
    }};
}

/// 草稿投影。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DraftView {
    /// 本地草稿 ID（原样回传给 `draft_status`）。
    pub draft_id: String,
    /// 用户操作幂等键。
    pub operation_id: String,
    /// 会话 ID。
    pub conversation_id: String,
    /// 明文正文（只在内存中出现，落盘前已加密）。
    pub body: String,
    /// 草稿状态：`queued` 或 `prepared`。
    pub state: String,
    /// 关联的 SubmissionId（尚无准备尝试时为空）。
    pub submission_id: Option<String>,
}

/// 草稿真实状态投影（CORE-010；**不含正文**）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DraftStatusView {
    /// 本地草稿 ID。
    pub draft_id: String,
    /// 会话 ID。
    pub conversation_id: String,
    /// 用户操作幂等键。
    pub operation_id: String,
    /// 有效状态：queued / prepared / submitted / accepted / rejected / unknown / cancelled / failed。
    pub status: String,
    /// 最新尝试的 SubmissionId（尚无尝试时为空）。
    pub submission_id: Option<String>,
    /// 最新尝试的重建序号（0 表示首次尝试）。
    pub attempt_index: Option<u32>,
    /// 已尝试次数。
    pub attempts: u32,
    /// 可观测的最后活动时间（Unix 毫秒）。
    pub updated_at_ms: u64,
    /// 现在调用 retry 是否会真正推进。
    pub can_retry: bool,
    /// 现在调用 cancel 是否还有意义。
    pub can_cancel: bool,
}

/// 会话列表条目。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ConversationView {
    /// 会话 ID。
    pub conversation_id: String,
    /// 会话标题（未知时为空字符串，UI 自行回退）。
    pub title: String,
    /// 最新条目预览。
    pub preview: String,
    /// 未读计数：当前恒为空（未知，不伪造 0）。
    pub unread_count: Option<u32>,
    /// 最近活动时间（Unix 毫秒）。
    pub updated_at_ms: u64,
}

/// 消息行。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
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
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ConversationPageView {
    /// 本页条目（最近活动在前）。
    pub items: Vec<ConversationView>,
    /// 下一页游标；空表示没有更多。游标必须原样回传，不要解释或伪造。
    pub next_cursor: Option<String>,
}

/// 一页消息。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MessagePageView {
    /// 本页条目（本机待发在前，正式消息按序号降序）。
    pub items: Vec<MessageRowView>,
    /// 下一页游标；空表示没有更多。
    pub next_cursor: Option<String>,
}

/// 取消结果（CORE-009 三态；**不含正文**）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CancelViewFfi {
    /// 本地草稿 ID。
    pub draft_id: String,
    /// 三态之一：`cancel_requested` / `cancel_too_late` / `cancel_unknown`。
    pub outcome: String,
    /// 关联的 SubmissionId（`cancel_requested` 在尚无尝试时为空）。
    pub submission_id: Option<String>,
}

/// 内核变更事件（CORE-003 最小切片）：只含 ID 与稳定状态文本。
///
/// 事件不是可靠消息日志：收到后必须用 `draft_status_view` / `list_*` 重查真实状态。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreEventFfi {
    /// 事件种类：`draft_changed` / `conversation_changed` / `resync_required` / `core_closed`。
    pub kind: String,
    /// 草稿 ID（仅 `draft_changed`）。
    pub draft_id: Option<String>,
    /// 会话 ID（`draft_changed` / `conversation_changed`）。
    pub conversation_id: Option<String>,
    /// 稳定状态文本（仅 `draft_changed`，取值同 `DraftStatusView.status`）。
    pub status: Option<String>,
    /// 最新尝试的 SubmissionId（仅 `draft_changed`，可能为空）。
    pub submission_id: Option<String>,
    /// 重查原因（仅 `resync_required`；静态文本，不含用户内容）。
    pub reason: Option<String>,
}

/// 一页事件。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct EventBatchFfi {
    /// 本批事件（按序号升序；resync 时只有一条 `resync_required`）。
    pub events: Vec<CoreEventFfi>,
    /// 下一次 poll 应传入的游标（本批最后一条事件的序号；无新事件时等于入参）。
    pub next_seq: u64,
    /// 是否要求整体重查（此时本批不含增量事件）。
    pub resync_required: bool,
}

/// 发送链路单步结果（SND）：状态 + SubmissionId + 尝试序号（**不含正文/密钥**）。
///
/// `settled = false` 表示结果尚未确定（仍需 reconcile，禁止重发）；调用方不得把
/// 未确定的结果当成已送达。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SendStepView {
    /// 本地草稿 ID（原样回传）。
    pub draft_id: String,
    /// 有效状态：queued / prepared / submitted / accepted / rejected / unknown / cancelled / failed。
    pub status: String,
    /// 最新尝试的 SubmissionId（尚无尝试时为空）。
    pub submission_id: Option<String>,
    /// 最新尝试的重建序号（0 表示首次尝试）。
    pub attempt_index: Option<u32>,
    /// 人类可读的静态说明（不含正文/密钥/URL）。
    pub detail: String,
    /// 该结果是否已确定（false = 仍需对账，禁止重发）。
    pub settled: bool,
}

/// 正式加密存储对象。
#[derive(uniffi::Object)]
pub struct LocalStoreFfi {
    core: Mutex<ConversationCore>,
}

impl LocalStoreFfi {
    fn lock(&self) -> Result<MutexGuard<'_, ConversationCore>, FfiError> {
        self.core.lock().map_err(|_| FfiError::Store {
            code: "CORE_PANICKED".to_owned(),
            reason: "local store lock is poisoned".to_owned(),
        })
    }
}

#[uniffi::export]
impl LocalStoreFfi {
    /// 打开或创建本地库。数据目录必须已存在，主密钥为 64 位十六进制。
    #[uniffi::constructor]
    pub fn open(database_path: String, master_key_hex: String) -> Result<Arc<Self>, FfiError> {
        let core = ConversationCore::open(&database_path, &master_key_hex)
            .map_err(|error| failure!(error))?;
        Ok(Arc::new(Self {
            core: Mutex::new(core),
        }))
    }

    /// 新建（或幂等复用）一条本地草稿，返回 `queued` 状态。
    pub fn create_draft(
        &self,
        conversation_id: String,
        operation_id: String,
        body: String,
    ) -> Result<DraftView, FfiError> {
        let mut core = self.lock()?;
        let view = core
            .create_draft(&conversation_id, &operation_id, &body)
            .map_err(|error| failure!(error))?;
        Ok(draft_view(view))
    }

    /// 查询草稿状态；未知草稿返回 null。
    /// 某条消息引用的附件 ID 列表（ATTACHMENTS 引用；按 ID 升序、有界）。
    pub fn message_attachment_ids(&self, message_id: String) -> Result<Vec<String>, FfiError> {
        let core = self.lock()?;
        core.attachments_for_message(&message_id)
            .map_err(|error| failure!(error))
    }

    /// 草稿真实状态（CORE-010）：不含正文，供列表/按钮态使用。
    pub fn draft_status_view(&self, draft_id: String) -> Result<Option<DraftStatusView>, FfiError> {
        let core = self.lock()?;
        let status = core
            .get_draft_status(&draft_id)
            .map_err(|error| failure!(error))?;
        Ok(status.map(|view| DraftStatusView {
            draft_id: view.draft_id,
            conversation_id: view.conversation_id,
            operation_id: view.operation_id,
            status: view.status,
            submission_id: view.submission_id,
            attempt_index: view.attempt_index,
            attempts: view.attempts,
            updated_at_ms: view.updated_at_ms,
            can_retry: view.can_retry,
            can_cancel: view.can_cancel,
        }))
    }

    pub fn draft_status(&self, draft_id: String) -> Result<Option<DraftView>, FfiError> {
        let core = self.lock()?;
        let status = core.draft_status(&draft_id).map_err(|error| failure!(error))?;
        Ok(status.map(draft_view))
    }

    /// 写入一条已验证条目（自动建会话行、密封正文、更新会话摘要、按消息 ID 幂等）。
    #[allow(clippy::too_many_arguments)]
    pub fn apply_verified_entry(
        &self,
        conversation_id: String,
        title: String,
        message_id: String,
        author_account: String,
        author_device: String,
        body: String,
        mine: bool,
        created_at_ms: u64,
    ) -> Result<MessageRowView, FfiError> {
        let created_at_ms = i64::try_from(created_at_ms).map_err(|_| FfiError::Store {
            code: "INVALID_INPUT".to_owned(),
            reason: "created_at_ms is outside the supported range".to_owned(),
        })?;
        let mut core = self.lock()?;
        let view = core
            .apply_verified_entry(VerifiedEntryInput {
                conversation_id,
                title,
                message_id,
                author_account,
                author_device,
                body,
                mine,
                created_at_ms,
            })
            .map_err(|error| failure!(error))?;
        Ok(message_row_view(view))
    }

    /// 会话列表分页；`cursor` 为上一页返回的 `next_cursor`。
    pub fn list_conversations(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<ConversationPageView, FfiError> {
        let core = self.lock()?;
        let page = core
            .list_conversations(cursor.as_deref(), limit)
            .map_err(|error| failure!(error))?;
        Ok(conversation_page_view(page))
    }

    /// 单会话消息分页（含本机待发意图）。
    pub fn list_messages(
        &self,
        conversation_id: String,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<MessagePageView, FfiError> {
        let core = self.lock()?;
        let page = core
            .list_messages(&conversation_id, cursor.as_deref(), limit)
            .map_err(|error| failure!(error))?;
        Ok(message_page_view(page))
    }

    /// CORE-009：取消一条尚未正式接受的草稿（三态 `cancel_requested` /
    /// `cancel_too_late` / `cancel_unknown`）。
    ///
    /// 只做薄封装：状态机、落库与事件都由 core 的 `ConversationCore::cancel_draft`
    /// 决定（同一个 `Store` 句柄，不重开库）。**不伪造成功**：FFI 未接真实节点与
    /// MLS 群时，取消只记录用户意图；后续 dispatch/retry 在桩端口上显式失败
    /// （`NETWORK_UNAVAILABLE` / `PROTOCOL_UNSUPPORTED`），见 bridge.md。
    pub fn cancel_draft(&self, draft_id: String) -> Result<CancelViewFfi, FfiError> {
        let mut core = self.lock()?;
        let outcome = core
            .cancel_draft(&draft_id)
            .map_err(|error| failure!(error))?;
        Ok(cancel_view(outcome))
    }

    /// SND：保存发送节点配置（**只保存，不发任何请求**）。
    ///
    /// 端点必须是可解析的 http(s) 地址；`node_public_key_hex` 给出时必须是 64 位十六进制。
    /// 未配置时所有 `send_*` 返回稳定错误码 `NODE_UNAVAILABLE`，不伪造成功。
    pub fn configure_send_node(
        &self,
        endpoint_url: String,
        node_id: String,
        node_public_key_hex: Option<String>,
    ) -> Result<(), FfiError> {
        let mut core = self.lock()?;
        core.configure_send_node(&endpoint_url, &node_id, node_public_key_hex.as_deref())
            .map_err(|error| failure!(error))?;
        Ok(())
    }

    /// SND：显式指定发送所用的 MLS 群键（不设置时自动绑定本库唯一的持久 MLS 群）。
    pub fn configure_send_group(&self, mls_group_key: String) -> Result<(), FfiError> {
        let mut core = self.lock()?;
        core.configure_send_group(&mls_group_key)
            .map_err(|error| failure!(error))?;
        Ok(())
    }

    /// SND：prepare（必要时恰好加密一次；未配置节点/未加入群时显式失败）。
    pub fn send_prepare(&self, draft_id: String) -> Result<SendStepView, FfiError> {
        let mut core = self.lock()?;
        let view = core.send_prepare(&draft_id).map_err(|error| failure!(error))?;
        Ok(send_step_view(view))
    }

    /// SND：dispatch（真实节点；只走三态，结果未知绝不算成功）。
    pub fn send_dispatch(&self, draft_id: String) -> Result<SendStepView, FfiError> {
        let mut core = self.lock()?;
        let view = core.send_dispatch(&draft_id).map_err(|error| failure!(error))?;
        Ok(send_step_view(view))
    }

    /// SND：retry（被拒绝后重建，使用新的 SubmissionId）。
    pub fn send_retry(&self, draft_id: String) -> Result<SendStepView, FfiError> {
        let mut core = self.lock()?;
        let view = core.send_retry(&draft_id).map_err(|error| failure!(error))?;
        Ok(send_step_view(view))
    }

    /// SND：reconcile（结果未知时按 SubmissionId 对账；绝不重发）。
    pub fn send_reconcile(&self, draft_id: String) -> Result<SendStepView, FfiError> {
        let mut core = self.lock()?;
        let view = core
            .send_reconcile(&draft_id)
            .map_err(|error| failure!(error))?;
        Ok(send_step_view(view))
    }

    /// CORE-003（最小切片）：按单调序号读取内核事件。
    ///
    /// 事件只含 ID 与稳定状态文本（绝不含正文/密钥）；重复 poll 幂等；游标早于缓冲
    /// 起点或超前于本实例时返回 `resync_required = true` 与一条 `resync_required`
    /// 事件，绝不静默丢事件。`limit` 必须在 `1..=256`。
    pub fn poll_events(&self, since_seq: u64, limit: u32) -> Result<EventBatchFfi, FfiError> {
        let core = self.lock()?;
        let batch = core
            .poll_events(since_seq, limit)
            .map_err(|error| failure!(error))?;
        Ok(event_batch_view(batch))
    }

    /// 关闭存储并释放独占写锁；重复调用幂等。之后所有调用返回 `CORE_CLOSED`。
    ///
    /// 关闭时会发布一条 `core_closed` 事件（仍可通过 `poll_events` 读取）。
    ///
    /// 方法名用 `shutdown` 而不是 `close`：UniFFI 会为每个对象生成 `AutoCloseable.close()`，
    /// 同名会与生成代码产生 JVM 签名冲突（与 P0Core.shutdown 的既有做法一致）。
    pub fn shutdown(&self) {
        if let Ok(core) = self.core.lock() {
            core.close();
        }
    }

    /// 是否仍处于打开状态。
    pub fn is_open(&self) -> bool {
        match self.core.lock() {
            Ok(core) => core.is_open(),
            Err(_) => false,
        }
    }
}

fn draft_view(view: CoreDraftView) -> DraftView {
    DraftView {
        draft_id: view.draft_id,
        operation_id: view.operation_id,
        conversation_id: view.conversation_id,
        body: view.body,
        state: view.state,
        submission_id: view.submission_id,
    }
}

fn message_row_view(view: CoreMessageRowView) -> MessageRowView {
    MessageRowView {
        message_id: view.message_id,
        conversation_id: view.conversation_id,
        author: view.author,
        body: view.body,
        mine: view.mine,
        created_at_ms: view.created_at_ms,
    }
}

fn conversation_view(view: CoreConversationView) -> ConversationView {
    ConversationView {
        conversation_id: view.conversation_id,
        title: view.title,
        preview: view.preview,
        unread_count: view.unread_count,
        updated_at_ms: view.updated_at_ms,
    }
}

fn conversation_page_view(page: CoreConversationPageView) -> ConversationPageView {
    ConversationPageView {
        items: page.items.into_iter().map(conversation_view).collect(),
        next_cursor: page.next_cursor,
    }
}

fn message_page_view(page: CoreMessagePageView) -> MessagePageView {
    MessagePageView {
        items: page.items.into_iter().map(message_row_view).collect(),
        next_cursor: page.next_cursor,
    }
}

fn cancel_view(outcome: CancelOutcome) -> CancelViewFfi {
    match outcome {
        CancelOutcome::CancelRequested {
            draft_id,
            submission_id,
        } => CancelViewFfi {
            draft_id,
            outcome: "cancel_requested".to_owned(),
            submission_id,
        },
        CancelOutcome::CancelTooLate {
            draft_id,
            submission_id,
        } => CancelViewFfi {
            draft_id,
            outcome: "cancel_too_late".to_owned(),
            submission_id: Some(submission_id),
        },
        CancelOutcome::CancelUnknown {
            draft_id,
            submission_id,
        } => CancelViewFfi {
            draft_id,
            outcome: "cancel_unknown".to_owned(),
            submission_id: Some(submission_id),
        },
    }
}

fn send_step_view(view: CoreSendStepView) -> SendStepView {
    SendStepView {
        draft_id: view.draft_id,
        status: view.status,
        submission_id: view.submission_id,
        attempt_index: view.attempt_index,
        detail: view.detail,
        settled: view.settled,
    }
}

fn core_event_view(event: CoreEvent) -> CoreEventFfi {
    match event {
        CoreEvent::DraftChanged {
            draft_id,
            conversation_id,
            status,
            submission_id,
        } => CoreEventFfi {
            kind: "draft_changed".to_owned(),
            draft_id: Some(draft_id),
            conversation_id: Some(conversation_id),
            status: Some(status),
            submission_id,
            reason: None,
        },
        CoreEvent::ConversationChanged { conversation_id } => CoreEventFfi {
            kind: "conversation_changed".to_owned(),
            draft_id: None,
            conversation_id: Some(conversation_id),
            status: None,
            submission_id: None,
            reason: None,
        },
        CoreEvent::ResyncRequired { reason } => CoreEventFfi {
            kind: "resync_required".to_owned(),
            draft_id: None,
            conversation_id: None,
            status: None,
            submission_id: None,
            reason: Some(reason),
        },
        CoreEvent::CoreClosed => CoreEventFfi {
            kind: "core_closed".to_owned(),
            draft_id: None,
            conversation_id: None,
            status: None,
            submission_id: None,
            reason: None,
        },
    }
}

fn event_batch_view(batch: CoreEventBatch) -> EventBatchFfi {
    EventBatchFfi {
        events: batch.events.into_iter().map(core_event_view).collect(),
        next_seq: batch.next_seq,
        resync_required: batch.resync_required,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chat_protocol::{AccountId, ConversationId, DeviceId, MessageId, OperationId};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    const MASTER_KEY_HEX: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    struct Temp {
        dir: PathBuf,
    }

    impl Temp {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "chat-ffi-local-store-{}-{nanos}-{counter}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).expect("create temp dir");
            Self { dir }
        }

        fn database(&self) -> String {
            self.dir.join("chat.db").to_string_lossy().into_owned()
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn open(temp: &Temp) -> Arc<LocalStoreFfi> {
        LocalStoreFfi::open(temp.database(), MASTER_KEY_HEX.to_owned()).expect("open local store")
    }

    #[test]
    fn open_write_and_read_through_the_ffi_shape() {
        let temp = Temp::new();
        let store = open(&temp);
        assert!(store.is_open());
        let conversation_id = ConversationId::from_bytes([1u8; 32]).encode();
        let operation_id = OperationId::from_bytes([2u8; 32]).encode();
        let message_id = MessageId::from_bytes([3u8; 32]).encode();
        let account_id = AccountId::from_bytes([4u8; 32]).encode();
        let device_id = DeviceId::from_bytes([5u8; 32]).encode();

        let draft = store
            .create_draft(
                conversation_id.clone(),
                operation_id.clone(),
                "FFI 草稿".to_owned(),
            )
            .expect("create draft");
        assert_eq!(draft.state, "queued");
        assert_eq!(draft.body, "FFI 草稿");
        let status = store
            .draft_status(draft.draft_id.clone())
            .expect("draft status")
            .expect("draft exists");
        assert_eq!(status, draft);

        let applied = store
            .apply_verified_entry(
                conversation_id.clone(),
                "FFI 会话".to_owned(),
                message_id.clone(),
                account_id.clone(),
                device_id,
                "FFI 正式消息".to_owned(),
                true,
                1_700_000_000_000u64,
            )
            .expect("apply verified entry");
        assert_eq!(applied.message_id, message_id);
        assert_eq!(applied.body, "FFI 正式消息");
        assert!(applied.mine);

        let messages = store
            .list_messages(conversation_id.clone(), None, 10)
            .expect("message page");
        assert_eq!(messages.items.len(), 2, "pending draft + accepted message");
        assert_eq!(messages.items[1].message_id, message_id);

        let conversations = store
            .list_conversations(None, 10)
            .expect("conversation page");
        assert_eq!(conversations.items.len(), 1);
        assert_eq!(conversations.items[0].title, "FFI 会话");
        assert_eq!(conversations.items[0].preview, "FFI 正式消息");
        assert!(conversations.items[0].unread_count.is_none());

        store.shutdown();
        assert!(!store.is_open());
        let closed = store
            .list_conversations(None, 10)
            .expect_err("closed store must fail");
        match closed {
            FfiError::Store { code, .. } => assert_eq!(code, "CORE_CLOSED"),
        }
    }

    #[test]
    fn failures_are_typed_with_stable_codes() {
        let temp = Temp::new();
        let store = open(&temp);
        let error = store
            .create_draft(
                "conv-1".to_owned(),
                OperationId::from_bytes([7u8; 32]).encode(),
                "正文".to_owned(),
            )
            .expect_err("non-canonical conversation ID is rejected");
        match error {
            FfiError::Store { code, reason } => {
                assert_eq!(code, "INVALID_INPUT");
                assert!(reason.contains("PROTOCOL-001"), "reason: {reason}");
            }
        }
        // 时间戳越界显式拒绝，不静默截断。
        let error = store
            .apply_verified_entry(
                ConversationId::from_bytes([8u8; 32]).encode(),
                String::new(),
                MessageId::from_bytes([9u8; 32]).encode(),
                AccountId::from_bytes([10u8; 32]).encode(),
                DeviceId::from_bytes([11u8; 32]).encode(),
                "正文".to_owned(),
                true,
                u64::MAX,
            )
            .expect_err("out-of-range timestamp is rejected");
        match error {
            FfiError::Store { code, .. } => assert_eq!(code, "INVALID_INPUT"),
        }
    }

    #[test]
    fn cancel_draft_and_poll_events_through_the_ffi_shape() {
        let temp = Temp::new();
        let store = open(&temp);
        let conversation_id = ConversationId::from_bytes([21u8; 32]).encode();
        let operation_id = OperationId::from_bytes([22u8; 32]).encode();
        let draft = store
            .create_draft(
                conversation_id.clone(),
                operation_id,
                "FFI 取消正文 body-secret".to_owned(),
            )
            .expect("create draft");

        let cancel = store
            .cancel_draft(draft.draft_id.clone())
            .expect("cancel draft");
        assert_eq!(cancel.draft_id, draft.draft_id);
        assert_eq!(cancel.outcome, "cancel_requested");
        assert!(cancel.submission_id.is_none());

        let status = store
            .draft_status_view(draft.draft_id.clone())
            .expect("status")
            .expect("present");
        assert_eq!(status.status, "cancelled");
        assert!(!status.can_cancel);
        assert!(!status.can_retry);

        let batch = store.poll_events(0, 10).expect("poll events");
        assert!(!batch.resync_required);
        assert_eq!(batch.next_seq, 2);
        assert_eq!(batch.events.len(), 2);
        assert_eq!(batch.events[0].kind, "draft_changed");
        assert_eq!(batch.events[0].status.as_deref(), Some("cancelled"));
        assert_eq!(
            batch.events[0].draft_id.as_deref(),
            Some(draft.draft_id.as_str())
        );
        assert_eq!(
            batch.events[0].conversation_id.as_deref(),
            Some(conversation_id.as_str())
        );
        assert_eq!(batch.events[1].kind, "conversation_changed");
        // 事件只含 ID 与状态文本，不含正文。
        for event in &batch.events {
            let rendered = format!("{event:?}");
            assert!(
                !rendered.contains("body-secret"),
                "event must not carry the body: {rendered}"
            );
            if let Some(reason) = &event.reason {
                assert!(!reason.contains("body-secret"));
            }
        }
        // 只读：重复 poll 幂等；游标之后没有新事件。
        assert_eq!(store.poll_events(0, 10).expect("poll again"), batch);
        let empty = store
            .poll_events(batch.next_seq, 10)
            .expect("poll after cursor");
        assert!(empty.events.is_empty());
        assert_eq!(empty.next_seq, batch.next_seq);
    }

    #[test]
    fn failed_cancel_and_invalid_limit_surface_stable_codes() {
        let temp = Temp::new();
        let store = open(&temp);
        let error = store
            .cancel_draft("draft-missing".to_owned())
            .expect_err("missing draft must fail");
        match error {
            FfiError::Store { code, .. } => assert!(!code.is_empty()),
        }
        // 畸形 draft_id：INVALID_INPUT（不进入存储）。
        let error = store
            .cancel_draft("bad draft!".to_owned())
            .expect_err("malformed draft id must fail");
        match error {
            FfiError::Store { code, .. } => assert_eq!(code, "INVALID_INPUT"),
        }
        // 事件 limit 有上下界，越界显式拒绝。
        for limit in [0u32, chat_core::core_events::MAX_EVENT_BATCH + 1] {
            let error = store
                .poll_events(0, limit)
                .expect_err("invalid limit must fail");
            match error {
                FfiError::Store { code, .. } => assert_eq!(code, "INVALID_EVENT_LIMIT"),
            }
        }
        // 失败之后同一个 Store 仍可用。
        let draft = store
            .create_draft(
                ConversationId::from_bytes([23u8; 32]).encode(),
                OperationId::from_bytes([24u8; 32]).encode(),
                "失败后仍可写入".to_owned(),
            )
            .expect("store stays usable");
        assert_eq!(
            store
                .cancel_draft(draft.draft_id)
                .expect("cancel works")
                .outcome,
            "cancel_requested"
        );
    }

    #[test]
    fn shutdown_publishes_core_closed_and_events_stay_readable() {
        let temp = Temp::new();
        let store = open(&temp);
        store.shutdown();
        assert!(!store.is_open());
        let batch = store.poll_events(0, 10).expect("poll after shutdown");
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.events[0].kind, "core_closed");
        // 重复 shutdown 幂等：不会重复发布。
        store.shutdown();
        let again = store.poll_events(0, 10).expect("poll after second shutdown");
        assert_eq!(again.events.len(), 1);
        // 关闭后业务调用仍显式 CORE_CLOSED。
        let error = store
            .create_draft(
                ConversationId::from_bytes([25u8; 32]).encode(),
                OperationId::from_bytes([26u8; 32]).encode(),
                "关闭后".to_owned(),
            )
            .expect_err("closed store must fail");
        match error {
            FfiError::Store { code, .. } => assert_eq!(code, "CORE_CLOSED"),
        }
    }
}
