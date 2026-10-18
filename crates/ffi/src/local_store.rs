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

    /// 关闭存储并释放独占写锁；重复调用幂等。之后所有调用返回 `CORE_CLOSED`。
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
}
