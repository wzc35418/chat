//! 对端带附件的应用消息：把**已验证的内层事件**落成一条正式消息并登记附件引用。
//!
//! 边界（与 docs/implementation/attachments.md 的记录一致）：
//!
//! - 输入是 PROTOCOL-009 `verify_inner_event` 的产物 [AuthenticatedEvent]（外层候选已验签、
//!   内层事件已与 MLS 发送者/群/作者绑定）；本模块**不做协议校验**，也不解密；
//! - 正文与附件引用都从该事件里取：`ApplicationContent::Text` 只有正文，
//!   `ApplicationContent::Attachment` 只有一个协议层附件引用（`ObjectId`）；
//! - 协议层的 `ObjectId` 由调用方给的解析端口映射成**本地附件 ID**（解析不到 → 整条消息拒绝）；
//! - 落库走 `ConversationCore::apply_verified_entry_with_attachments`：消息与附件引用在
//!   **同一笔事务**里写入，任一附件记录不存在则整笔回滚（消息不落库、引用不留一半）；
//! - 应用消息只写消息/会话摘要，**不触碰群快照**（epoch/成员不变）。
//!
//! 消息 ID 的派生：内层事件没有线协议 `MessageId`，因此用**认证后的内层事件摘要**
//! （`AuthenticatedEvent::digest`，也就是可信去重键）派生 `msg:1:<hex>`。同一事件重放
//! 得到同一消息 ID，因此落库天然幂等；不同内容不可能撞同一 ID（同 ID 不同内容由 storage
//! 显式拒绝）。
//!
//! 已知限制：
//! - 附件消息的正文：`messages.protected_content` 目前只支持**非空文本**，附件消息因此落库为
//!   固定占位正文 [ATTACHMENT_ONLY_BODY]（真实附件展示走 `attachments_for_message`），
//!   PROTOCOL 附件消息与消息正文模型的对齐属消息模块；
//! - 扩展内容（PROTOCOL-010）不落库：显式 `PROTOCOL_UNSUPPORTED`；
//! - 单附件：协议内层事件当前只允许一个附件引用，因此每次登记 0 或 1 条引用。
use chat_protocol::event::{ApplicationContent, AuthenticatedEvent};
use chat_protocol::{MessageId, ObjectId};
use chat_storage::formal::VerifiedEntryInput;

use crate::attachments::AttachmentError;
use crate::service::{ConversationCore, MessageRowView};

/// 附件消息的占位正文（见模块文档的限制：正文列目前只接受非空文本）。
pub const ATTACHMENT_ONLY_BODY: &str = "[附件]";

/// 落库结果（全部来自真实写入；不含正文以外的任何秘密）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedApplicationMessage {
    /// 正式消息 ID（`msg:1:<hex>`，由认证事件摘要派生）。
    pub message_id: String,
    /// 会话 ID。
    pub conversation_id: String,
    /// 落库正文（附件消息是占位正文）。
    pub body: String,
    /// 该消息引用的本地附件 ID（按登记顺序）。
    pub attachment_ids: Vec<String>,
    /// 作者账户。
    pub author_account: String,
    /// 作者设备。
    pub author_device: String,
    /// 是否本机作者（依据本地身份投影，没有身份时为 false）。
    pub mine: bool,
    /// 事件声明的展示时间（Unix 毫秒）。
    pub sent_at_ms: u64,
}

/// 稳定错误码：先看附件层，再交给 core 的通用投影（storage/sync/group 的冻结码都透传）。
pub fn attachment_message_error_code(error: &anyhow::Error) -> &'static str {
    if let Some(attachment) = error.downcast_ref::<AttachmentError>() {
        return attachment.code();
    }
    crate::service::error_code(error)
}

/// 认证事件 → 正式消息 ID（`msg:1:<hex>`，取内层事件摘要）。
pub fn message_id_for_event(event: &AuthenticatedEvent) -> String {
    MessageId::from_bytes(*event.digest().as_bytes()).encode()
}

/// 把一条已验证的应用事件落库（含附件引用登记）。
///
/// `resolve` 把协议层附件引用（`ObjectId`）解析成本地附件 ID：解析不到返回 `None`，
/// 本函数在**写库之前**返回 `ATTACHMENT_NOT_FOUND`（整条消息拒绝）。解析到的 ID 若在正式库里
/// 不存在，storage 的事务会整体回滚（同样不留半落库）。
pub fn apply_authenticated_application_event(
    core: &mut ConversationCore,
    event: &AuthenticatedEvent,
    mut resolve: impl FnMut(ObjectId) -> Option<String>,
) -> anyhow::Result<AppliedApplicationMessage> {
    let plain = event.event();
    let (body, file_ids): (String, Vec<ObjectId>) = match &plain.content {
        ApplicationContent::Text { text } => (text.clone(), Vec::new()),
        ApplicationContent::Attachment { attachment } => (
            ATTACHMENT_ONLY_BODY.to_owned(),
            vec![attachment.file_id],
        ),
        ApplicationContent::Extension { .. } => {
            return Err(anyhow::Error::new(chat_sync::SyncError::new(
                chat_sync::ErrorCode::ProtocolUnsupported,
                "extension events are not persisted as messages",
            )));
        }
    };
    let mut attachment_ids = Vec::with_capacity(file_ids.len());
    for file_id in file_ids {
        match resolve(file_id) {
            Some(attachment_id) => attachment_ids.push(attachment_id),
            // 解析不到 = 本机没有这个附件：整条消息拒绝，绝不落一条引用不了的消息。
            None => return Err(anyhow::Error::new(AttachmentError::AttachmentNotFound)),
        }
    }
    let conversation_id = plain.conversation_id.encode();
    let author_account = plain.account_id.encode();
    let author_device = plain.device_id.encode();
    let sent_at_ms = plain.sent_at_unix_ms;
    let created_at_ms = i64::try_from(sent_at_ms).map_err(|_| {
        anyhow::Error::new(chat_sync::SyncError::new(
            chat_sync::ErrorCode::InvalidTarget,
            "event timestamp is out of the supported range",
        ))
    })?;
    let mine = match core.load_identity_account() {
        Ok(Some(identity)) => identity.account_id == author_account,
        Ok(None) => false,
        Err(error) => return Err(error),
    };
    let message_id = message_id_for_event(event);
    let input = VerifiedEntryInput {
        conversation_id: conversation_id.clone(),
        // 会话标题由后续已验证的控制条目补写；这里不臆造。
        title: String::new(),
        message_id: message_id.clone(),
        author_account: author_account.clone(),
        author_device: author_device.clone(),
        body: body.clone(),
        mine,
        created_at_ms,
    };
    let row: MessageRowView = core.apply_verified_entry_with_attachments(input, attachment_ids.clone())?;
    Ok(AppliedApplicationMessage {
        message_id: row.message_id,
        conversation_id: row.conversation_id,
        body: row.body,
        attachment_ids,
        author_account,
        author_device,
        mine,
        sent_at_ms,
    })
}

/// SECD：把**已由描述符复现**的附件消息落成正式消息。
///
/// 分工（存储是单写者独占：附件库与内核不能同时持句柄）：
///
/// 1. 调用方先用 [`crate::attachments::PersistedAttachmentStore::adopt_attachment_message`]
///    在附件库里核对绑定、开箱描述符、写附件记录与 0007 密钥记录；
/// 2. 再用本函数（内核句柄）把消息与引用落库。
///
/// 因此“描述符不可用/被篡改”一定发生在任何消息写入之前（绝不留半状态）。
pub fn apply_attachment_message_event(
    core: &mut ConversationCore,
    event: &AuthenticatedEvent,
    attachment_id: &str,
) -> anyhow::Result<AppliedApplicationMessage> {
    let plain = event.event();
    let ApplicationContent::Text { text } = &plain.content else {
        return Err(anyhow::Error::new(AttachmentError::AttachmentCorrupted(
            "attachment descriptor event must carry a text body",
        )));
    };
    let body = crate::attachments::parse_attachment_message_body(text)?
        .ok_or_else(|| {
            anyhow::Error::new(AttachmentError::AttachmentCorrupted(
                "message body is not an attachment descriptor body",
            ))
        })?;
    if body.attachment_id != attachment_id {
        return Err(anyhow::Error::new(AttachmentError::AttachmentCorrupted(
            "adopted attachment does not match the message body",
        )));
    }
    let conversation_id = plain.conversation_id.encode();
    let author_account = plain.account_id.encode();
    let author_device = plain.device_id.encode();
    let sent_at_ms = plain.sent_at_unix_ms;
    let created_at_ms = i64::try_from(sent_at_ms).map_err(|_| {
        anyhow::Error::new(chat_sync::SyncError::new(
            chat_sync::ErrorCode::InvalidTarget,
            "event timestamp is out of the supported range",
        ))
    })?;
    let mine = match core.load_identity_account() {
        Ok(Some(identity)) => identity.account_id == author_account,
        Ok(None) => false,
        Err(error) => return Err(error),
    };
    let message_id = message_id_for_event(event);
    let input = VerifiedEntryInput {
        conversation_id: conversation_id.clone(),
        title: String::new(),
        message_id: message_id.clone(),
        author_account: author_account.clone(),
        author_device: author_device.clone(),
        body: ATTACHMENT_ONLY_BODY.to_owned(),
        mine,
        created_at_ms,
    };
    let row = core.apply_verified_entry_with_attachments(input, vec![attachment_id.to_owned()])?;
    Ok(AppliedApplicationMessage {
        message_id: row.message_id,
        conversation_id: row.conversation_id,
        body: row.body,
        attachment_ids: vec![attachment_id.to_owned()],
        author_account,
        author_device,
        mine,
        sent_at_ms,
    })
}

/// 附件消息的**协议字段 57**（attachment_metadata）：新消息把描述符放在这里，正文只留人类可读摘要。
pub fn attachment_metadata_from_event(event: &AuthenticatedEvent) -> Option<&[u8]> {
    match &event.event().content {
        ApplicationContent::Attachment { attachment } => attachment.attachment_metadata.as_deref(),
        _ => None,
    }
}

/// 旧格式（描述符在正文里）的正文；新消息（字段 57）返回 None。
pub fn attachment_legacy_body_from_event(event: &AuthenticatedEvent) -> Option<&str> {
    match &event.event().content {
        ApplicationContent::Text { text } => Some(text.as_str()),
        _ => None,
    }
}

/// SECD/EXPD：把**已由描述符复现**的附件消息落成正式消息（字段 57 与旧正文两种形态都接受）。
///
/// 存储是单写者独占：调用方先用
/// [`crate::attachments::PersistedAttachmentStore::adopt_attachment_event`] 在附件库里完成
/// “优先 57 / 回退正文”的采用，再用本函数（内核句柄）写消息与引用。
pub fn apply_attachment_event(
    core: &mut ConversationCore,
    event: &AuthenticatedEvent,
    attachment_id: &str,
) -> anyhow::Result<AppliedApplicationMessage> {
    let plain = event.event();
    let has_metadata = attachment_metadata_from_event(event).is_some();
    let has_legacy_body = attachment_legacy_body_from_event(event).is_some();
    if !has_metadata && !has_legacy_body {
        return Err(anyhow::Error::new(AttachmentError::AttachmentCorrupted(
            "attachment event carries neither field 57 metadata nor a legacy body",
        )));
    }
    let conversation_id = plain.conversation_id.encode();
    let author_account = plain.account_id.encode();
    let author_device = plain.device_id.encode();
    let sent_at_ms = plain.sent_at_unix_ms;
    let created_at_ms = i64::try_from(sent_at_ms).map_err(|_| {
        anyhow::Error::new(chat_sync::SyncError::new(
            chat_sync::ErrorCode::InvalidTarget,
            "event timestamp is out of the supported range",
        ))
    })?;
    let mine = match core.load_identity_account() {
        Ok(Some(identity)) => identity.account_id == author_account,
        Ok(None) => false,
        Err(error) => return Err(error),
    };
    let message_id = message_id_for_event(event);
    let input = VerifiedEntryInput {
        conversation_id: conversation_id.clone(),
        title: String::new(),
        message_id: message_id.clone(),
        author_account: author_account.clone(),
        author_device: author_device.clone(),
        // 正文只留人类可读摘要（描述符在字段 57 / 旧正文里，从不进消息正文）。
        body: ATTACHMENT_ONLY_BODY.to_owned(),
        mine,
        created_at_ms,
    };
    let row = core.apply_verified_entry_with_attachments(input, vec![attachment_id.to_owned()])?;
    Ok(AppliedApplicationMessage {
        message_id: row.message_id,
        conversation_id: row.conversation_id,
        body: row.body,
        attachment_ids: vec![attachment_id.to_owned()],
        author_account,
        author_device,
        mine,
        sent_at_ms,
    })
}
