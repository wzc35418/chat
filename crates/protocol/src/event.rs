//! PROTOCOL-008/009：MLS 内层应用事件的规范编码与认证。
//!
//! 事件字节是确定性的规范编码（含协议版本与内容类型），绑定群、会话、事件 ID 与
//! 作者设备；回复引用必须指向同一会话。返回值 [`SecretBytes`] 在析构时清零，
//! `Debug` 不打印内容。去重标识来自**内层认证事件**，不是外层 message ID。

use core::fmt;
use std::collections::BTreeMap;

use zeroize::Zeroize;

use crate::canonical::{ProtocolVersion, WireField, WireValue, encode_canonical, field};
use crate::decode::{DecodeLimits, decode_bounded};
use crate::digest::Digest;
use crate::error::{ProtocolError, Result};
use crate::extension::{
    ContentLimits, ExtensionContent, MAX_FALLBACK_BYTES, MAX_NAMESPACE_BYTES, ValidatedExtension,
    validate_extension,
};
use crate::ids::{AccountId, ConversationId, DeviceId, EventId, GroupId, MessageId, ObjectId};
use crate::signing::CandidateKind;
use crate::verify::VerifiedCandidate;

/// 单条内层事件的字节上限（完整规范序列化）。
pub const MAX_APPLICATION_EVENT_BYTES: usize = 64 * 1024;
/// 文本内容上限（UTF-8 完整字节）。
pub const MAX_EVENT_TEXT_BYTES: usize = 8 * 1024;
/// 事件摘要用途域（可信去重键）。
pub const EVENT_DIGEST_DOMAIN: &[u8] = b"chat/p1/application-event/v1";
/// 附件引用可选 metadata 的字节上限（密钥描述符等）。
pub const MAX_ATTACHMENT_METADATA_BYTES: usize = 512;

const SCHEMA_VERSION: u64 = 1;
const EVENT_FIELD_COUNT: usize = 16;
const CONTENT_KIND_TEXT: u64 = 1;
const CONTENT_KIND_ATTACHMENT: u64 = 2;
const CONTENT_KIND_EXTENSION: u64 = 3;
const EXTENSION_FIELD_COUNT: usize = 6;

/// 事件来源。由可信调用路径设置，**永远不能替代权限检查**。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageOrigin {
    /// 本机用户发起。
    UserInitiated = 1,
    /// 系统生成（例如本地同步标记）。
    SystemGenerated = 2,
}

impl MessageOrigin {
    /// 冻结数值。
    pub const fn as_u64(self) -> u64 {
        self as u64
    }

    /// 由冻结数值还原；未知值显式失败。
    pub const fn from_u64(value: u64) -> Option<Self> {
        match value {
            1 => Some(MessageOrigin::UserInitiated),
            2 => Some(MessageOrigin::SystemGenerated),
            _ => None,
        }
    }
}

/// 附件引用（已完成加密上传的文件描述 ID）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentRef {
    /// 文件对象 ID。
    pub file_id: ObjectId,
    /// 可选的附件 metadata（例如密钥描述符；1–512 字节，旧事件缺省为 `None`）。
    pub attachment_metadata: Option<Vec<u8>>,
}

impl AttachmentRef {
    /// 仅文件 ID 的附件引用（兼容旧调用点）。
    pub const fn new(file_id: ObjectId) -> Self {
        Self {
            file_id,
            attachment_metadata: None,
        }
    }

    /// 附带 metadata 的附件引用。
    pub fn with_metadata(mut self, attachment_metadata: Vec<u8>) -> Self {
        self.attachment_metadata = Some(attachment_metadata);
        self
    }
}

/// 回复目标。必须与事件同一会话。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplyTarget {
    /// 目标消息所属会话。
    pub conversation: ConversationId,
    /// 目标消息 ID。
    pub message_id: MessageId,
}

/// 内层消息内容。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplicationContent {
    /// 纯文本正文。
    Text {
        /// UTF-8 正文。
        text: String,
    },
    /// 附件引用（不含明文或文件密钥）。
    Attachment {
        /// 附件引用。
        attachment: AttachmentRef,
    },
    /// 已通过 PROTOCOL-010 校验的扩展内容。
    Extension {
        /// 校验后的扩展。
        extension: ValidatedExtension,
    },
}

/// 明文应用事件（尚未经 MLS 加密）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlainApplicationEvent {
    /// 协议版本。
    pub protocol_version: ProtocolVersion,
    /// 会话。
    pub conversation_id: ConversationId,
    /// 群。
    pub group_id: GroupId,
    /// 事件 ID（内层可信去重键的组成部分）。
    pub event_id: EventId,
    /// 作者账户。
    pub account_id: AccountId,
    /// 作者设备。
    pub device_id: DeviceId,
    /// 内容。
    pub content: ApplicationContent,
    /// 可选回复目标。
    pub reply: Option<ReplyTarget>,
    /// 来源（仅展示/审计，不是授权）。
    pub origin: MessageOrigin,
    /// 展示时间（UTC 毫秒，仅展示，不参与权限排序）。
    pub sent_at_unix_ms: u64,
}

/// 敏感事件字节；析构时清零，`Debug` 不打印内容。
#[derive(Clone, PartialEq, Eq)]
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    /// 只读字节；调用方必须按敏感数据处理。
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// 字节数。
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "SecretBytes(<redacted, {} bytes>)", self.0.len())
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// MLS 凭据确认的发送者身份。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MlsSender {
    /// 账户。
    pub account_id: AccountId,
    /// 设备。
    pub device_id: DeviceId,
}

/// MLS 解密后的内层事件字节。
#[derive(Clone, PartialEq, Eq)]
pub struct DecryptedEvent {
    bytes: Vec<u8>,
}

impl DecryptedEvent {
    /// 由解密明文构造；长度与结构在 [`verify_inner_event`] 中校验。
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// 只读字节。
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for DecryptedEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "DecryptedEvent(<redacted, {} bytes>)",
            self.bytes.len()
        )
    }
}

/// 通过内外绑定校验的认证事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedEvent {
    event: PlainApplicationEvent,
    digest: Digest,
    sender: MlsSender,
    outer_digest: Digest,
}

impl AuthenticatedEvent {
    /// 内层事件内容。
    pub const fn event(&self) -> &PlainApplicationEvent {
        &self.event
    }

    /// 认证后的事件 ID（可信去重键）。
    pub const fn event_id(&self) -> EventId {
        self.event.event_id
    }

    /// 会话。
    pub const fn conversation_id(&self) -> ConversationId {
        self.event.conversation_id
    }

    /// 作者账户。
    pub const fn account_id(&self) -> AccountId {
        self.event.account_id
    }

    /// 作者设备。
    pub const fn device_id(&self) -> DeviceId {
        self.event.device_id
    }

    /// 内层事件摘要（域分隔）；永久去重只能用这个值。
    pub const fn digest(&self) -> Digest {
        self.digest
    }

    /// 外层候选摘要，仅用于审计关联，**不能**作为去重键。
    pub const fn outer_digest(&self) -> Digest {
        self.outer_digest
    }

    /// 去重键：内层事件摘要。
    pub const fn dedupe_key(&self) -> Digest {
        self.digest
    }
}

fn is_zero_bytes(bytes: &[u8; 32]) -> bool {
    *bytes == [0u8; 32]
}

impl PlainApplicationEvent {
    fn validate(&self) -> Result<()> {
        if !self.protocol_version.is_supported() {
            return Err(ProtocolError::UnsupportedProtocolVersion);
        }
        if is_zero_bytes(self.group_id.as_bytes())
            || is_zero_bytes(self.conversation_id.as_bytes())
            || is_zero_bytes(self.event_id.as_bytes())
            || is_zero_bytes(self.account_id.as_bytes())
            || is_zero_bytes(self.device_id.as_bytes())
        {
            return Err(ProtocolError::InvalidEventBinding);
        }
        if let Some(reply) = &self.reply {
            if reply.conversation != self.conversation_id {
                return Err(ProtocolError::InvalidReply);
            }
            if is_zero_bytes(reply.message_id.as_bytes()) {
                return Err(ProtocolError::InvalidReply);
            }
        }
        match &self.content {
            ApplicationContent::Text { text } => {
                if text.trim().is_empty() {
                    return Err(ProtocolError::InvalidEventBinding);
                }
                if text.len() > MAX_EVENT_TEXT_BYTES {
                    return Err(ProtocolError::ContentTooLarge {
                        limit: MAX_EVENT_TEXT_BYTES,
                    });
                }
            }
            ApplicationContent::Attachment { attachment } => {
                if is_zero_bytes(attachment.file_id.as_bytes()) {
                    return Err(ProtocolError::InvalidEventBinding);
                }
                if let Some(metadata) = &attachment.attachment_metadata {
                    // 非本形态（空 metadata）与超长都显式拒绝。
                    if metadata.is_empty() || metadata.len() > MAX_ATTACHMENT_METADATA_BYTES {
                        return Err(ProtocolError::InvalidCandidate);
                    }
                }
            }
            ApplicationContent::Extension { extension } => {
                // 重新走 PROTOCOL-010：扩展内容必须仍然结构性合法。
                let revalidated = validate_extension(
                    ExtensionContent {
                        namespace: extension.namespace.clone(),
                        schema_version: extension.schema_version,
                        operation_id: extension.operation_id,
                        object_id: extension.object_id,
                        payload: extension.payload.clone(),
                        fallback_text: extension.fallback_text.clone(),
                    },
                    ContentLimits::DEFAULT,
                )
                .map_err(|_| ProtocolError::InvalidEventBinding)?;
                if revalidated != *extension {
                    return Err(ProtocolError::InvalidEventBinding);
                }
            }
        }
        Ok(())
    }
}

fn extension_value(extension: &ValidatedExtension) -> WireValue {
    WireValue::structure([
        WireField::new(
            field::EXTENSION_NAMESPACE,
            WireValue::Text(extension.namespace.clone()),
        ),
        WireField::new(
            field::EXTENSION_SCHEMA_VERSION,
            WireValue::U64(u64::from(extension.schema_version)),
        ),
        WireField::new(
            field::EXTENSION_OPERATION_ID,
            WireValue::id_bytes(extension.operation_id.as_bytes()),
        ),
        WireField::new(
            field::EXTENSION_OBJECT_ID,
            WireValue::id_bytes(extension.object_id.as_bytes()),
        ),
        WireField::new(
            field::EXTENSION_PAYLOAD,
            WireValue::Bytes(extension.payload.clone()),
        ),
        WireField::new(
            field::EXTENSION_FALLBACK,
            WireValue::Text(extension.fallback_text.clone()),
        ),
    ])
}

fn optional_id(value: Option<&[u8; 32]>) -> WireValue {
    WireValue::optional(value.map(WireValue::id_bytes))
}

fn event_body(event: &PlainApplicationEvent) -> WireValue {
    let content_kind;
    let mut text = WireValue::None;
    let mut attachment = WireValue::None;
    let mut extension = WireValue::None;
    match &event.content {
        ApplicationContent::Text { text: body } => {
            content_kind = CONTENT_KIND_TEXT;
            text = WireValue::Some(Box::new(WireValue::Text(body.clone())));
        }

        ApplicationContent::Attachment {
            attachment: reference,
        } => {
            content_kind = CONTENT_KIND_ATTACHMENT;
            attachment =
                WireValue::optional(Some(WireValue::id_bytes(reference.file_id.as_bytes())));
        }
        ApplicationContent::Extension { extension: content } => {
            content_kind = CONTENT_KIND_EXTENSION;
            extension = WireValue::optional(Some(extension_value(content)));
        }
    }
    let mut body_fields = vec![
        WireField::new(field::SCHEMA_VERSION, WireValue::U64(SCHEMA_VERSION)),
        WireField::new(
            field::PROTOCOL_MAJOR,
            WireValue::U64(u64::from(event.protocol_version.major)),
        ),
        WireField::new(
            field::PROTOCOL_MINOR,
            WireValue::U64(u64::from(event.protocol_version.minor)),
        ),
        WireField::new(
            field::EVENT_CONVERSATION_ID,
            WireValue::id_bytes(event.conversation_id.as_bytes()),
        ),
        WireField::new(
            field::GROUP_ID,
            WireValue::id_bytes(event.group_id.as_bytes()),
        ),
        WireField::new(
            field::EVENT_ID,
            WireValue::id_bytes(event.event_id.as_bytes()),
        ),
        WireField::new(
            field::AUTHOR_ACCOUNT_ID,
            WireValue::id_bytes(event.account_id.as_bytes()),
        ),
        WireField::new(
            field::AUTHOR_DEVICE_ID,
            WireValue::id_bytes(event.device_id.as_bytes()),
        ),
        WireField::new(field::CONTENT_KIND, WireValue::U64(content_kind)),
        WireField::new(field::CONTENT_TEXT, text),
        WireField::new(field::CONTENT_ATTACHMENT_FILE_ID, attachment),
        WireField::new(field::CONTENT_EXTENSION, extension),
        WireField::new(
            field::REPLY_CONVERSATION_ID,
            optional_id(
                event
                    .reply
                    .as_ref()
                    .map(|reply| reply.conversation.as_bytes()),
            ),
        ),
        WireField::new(
            field::REPLY_MESSAGE_ID,
            optional_id(
                event
                    .reply
                    .as_ref()
                    .map(|reply| reply.message_id.as_bytes()),
            ),
        ),
        WireField::new(field::MESSAGE_ORIGIN, WireValue::U64(event.origin.as_u64())),
        WireField::new(field::EVENT_SENT_AT, WireValue::U64(event.sent_at_unix_ms)),
    ];
    // 旧事件（无 metadata）不写新字段，既有固定向量逐字节不变。
    if let ApplicationContent::Attachment { attachment } = &event.content
        && let Some(metadata) = &attachment.attachment_metadata
    {
        body_fields.push(WireField::new(
            field::CONTENT_ATTACHMENT_METADATA,
            WireValue::Bytes(metadata.clone()),
        ));
    }
    WireValue::Struct(body_fields)
}

fn canonical_event_bytes(event: &PlainApplicationEvent) -> Result<Vec<u8>> {
    event.validate()?;
    let encoded = encode_canonical(&event_body(event), event.protocol_version)?;
    if encoded.len() > MAX_APPLICATION_EVENT_BYTES {
        return Err(ProtocolError::ContentTooLarge {
            limit: MAX_APPLICATION_EVENT_BYTES,
        });
    }
    Ok(encoded.into_vec())
}

/// PROTOCOL-008：编码内层应用事件。
///
/// 时间只用于展示；完整规范序列化超过 64 KiB 或文本超过 8 KiB 显式
/// `CONTENT_TOO_LARGE`；跨会话回复 `INVALID_REPLY`；结构性非法 16 字段绑定
/// `INVALID_EVENT_BINDING`。返回缓冲区在析构时清零。
pub fn encode_application_event(event: PlainApplicationEvent) -> Result<SecretBytes> {
    canonical_event_bytes(&event).map(SecretBytes)
}

fn take_value(map: &mut BTreeMap<u16, WireValue>, id: u16) -> Result<WireValue> {
    map.remove(&id).ok_or(ProtocolError::InvalidEventBinding)
}

fn take_u64(map: &mut BTreeMap<u16, WireValue>, id: u16) -> Result<u64> {
    match take_value(map, id)? {
        WireValue::U64(value) => Ok(value),
        _ => Err(ProtocolError::InvalidEventBinding),
    }
}

fn take_bytes(map: &mut BTreeMap<u16, WireValue>, id: u16) -> Result<Vec<u8>> {
    match take_value(map, id)? {
        WireValue::Bytes(value) => Ok(value),
        _ => Err(ProtocolError::InvalidEventBinding),
    }
}

fn take_text(map: &mut BTreeMap<u16, WireValue>, id: u16) -> Result<String> {
    match take_value(map, id)? {
        WireValue::Text(value) => Ok(value),
        _ => Err(ProtocolError::InvalidEventBinding),
    }
}

fn take_optional(map: &mut BTreeMap<u16, WireValue>, id: u16) -> Result<Option<WireValue>> {
    match take_value(map, id)? {
        WireValue::None => Ok(None),
        WireValue::Some(inner) => Ok(Some(*inner)),
        _ => Err(ProtocolError::InvalidEventBinding),
    }
}

fn take_id<const N: usize>(map: &mut BTreeMap<u16, WireValue>, id: u16) -> Result<[u8; N]> {
    take_bytes(map, id)?
        .try_into()
        .map_err(|_| ProtocolError::InvalidEventBinding)
}

fn take_optional_id<const N: usize>(
    map: &mut BTreeMap<u16, WireValue>,
    id: u16,
) -> Result<Option<[u8; N]>> {
    match take_optional(map, id)? {
        Some(WireValue::Bytes(bytes)) => Ok(Some(
            bytes
                .try_into()
                .map_err(|_| ProtocolError::InvalidEventBinding)?,
        )),
        Some(_) => Err(ProtocolError::InvalidEventBinding),
        None => Ok(None),
    }
}

fn parse_extension(value: WireValue) -> Result<ValidatedExtension> {
    let WireValue::Struct(fields) = value else {
        return Err(ProtocolError::InvalidEventBinding);
    };
    if fields.len() != EXTENSION_FIELD_COUNT {
        return Err(ProtocolError::InvalidEventBinding);
    }
    let mut map: BTreeMap<u16, WireValue> = BTreeMap::new();
    for field in fields {
        if map.insert(field.id, field.value).is_some() {
            return Err(ProtocolError::InvalidEventBinding);
        }
    }
    let namespace = take_text(&mut map, field::EXTENSION_NAMESPACE)?;
    let schema_version = take_u64(&mut map, field::EXTENSION_SCHEMA_VERSION)?;
    let schema_version =
        u16::try_from(schema_version).map_err(|_| ProtocolError::InvalidEventBinding)?;
    let operation_id = crate::ids::OperationId::from_bytes(take_id::<32>(
        &mut map,
        field::EXTENSION_OPERATION_ID,
    )?);
    let object_id = ObjectId::from_bytes(take_id::<32>(&mut map, field::EXTENSION_OBJECT_ID)?);
    let payload = take_bytes(&mut map, field::EXTENSION_PAYLOAD)?;
    let fallback_text = take_text(&mut map, field::EXTENSION_FALLBACK)?;
    if !map.is_empty() {
        return Err(ProtocolError::InvalidEventBinding);
    }
    if namespace.len() > MAX_NAMESPACE_BYTES || fallback_text.len() > MAX_FALLBACK_BYTES {
        return Err(ProtocolError::InvalidEventBinding);
    }
    validate_extension(
        ExtensionContent {
            namespace,
            schema_version,
            operation_id,
            object_id,
            payload,
            fallback_text,
        },
        ContentLimits::DEFAULT,
    )
    .map_err(|_| ProtocolError::InvalidEventBinding)
}

/// PROTOCOL-009 的解码步骤：有界解码并做规范往返校验。
///
/// 返回结构合法的事件；调用方仍须完成内外绑定（[`verify_inner_event`]）。
pub fn decode_application_event(
    bytes: &[u8],
    limits: DecodeLimits,
) -> Result<PlainApplicationEvent> {
    if bytes.len() > MAX_APPLICATION_EVENT_BYTES {
        return Err(ProtocolError::ContentTooLarge {
            limit: MAX_APPLICATION_EVENT_BYTES,
        });
    }
    let frame = decode_bounded(bytes, limits).map_err(|_| ProtocolError::InvalidEventBinding)?;
    let WireValue::Struct(fields) = frame.into_value() else {
        return Err(ProtocolError::InvalidEventBinding);
    };
    if !(fields.len() == EVENT_FIELD_COUNT || fields.len() == EVENT_FIELD_COUNT + 1) {
        return Err(ProtocolError::InvalidEventBinding);
    }
    let mut map: BTreeMap<u16, WireValue> = BTreeMap::new();
    for field in fields {
        if map.insert(field.id, field.value).is_some() {
            return Err(ProtocolError::InvalidEventBinding);
        }
    }
    let schema_version = take_u64(&mut map, field::SCHEMA_VERSION)?;
    if schema_version != SCHEMA_VERSION {
        return Err(ProtocolError::InvalidEventBinding);
    }
    let major = u8::try_from(take_u64(&mut map, field::PROTOCOL_MAJOR)?)
        .map_err(|_| ProtocolError::InvalidEventBinding)?;
    let minor = u8::try_from(take_u64(&mut map, field::PROTOCOL_MINOR)?)
        .map_err(|_| ProtocolError::InvalidEventBinding)?;
    let protocol_version = ProtocolVersion { major, minor };
    if !protocol_version.is_supported() {
        return Err(ProtocolError::UnsupportedProtocolVersion);
    }
    let conversation_id =
        ConversationId::from_bytes(take_id::<32>(&mut map, field::EVENT_CONVERSATION_ID)?);
    let group_id = GroupId::from_bytes(take_id::<32>(&mut map, field::GROUP_ID)?);
    let event_id = EventId::from_bytes(take_id::<32>(&mut map, field::EVENT_ID)?);
    let account_id = AccountId::from_bytes(take_id::<32>(&mut map, field::AUTHOR_ACCOUNT_ID)?);
    let device_id = DeviceId::from_bytes(take_id::<32>(&mut map, field::AUTHOR_DEVICE_ID)?);
    let content_kind = take_u64(&mut map, field::CONTENT_KIND)?;
    let text = match take_optional(&mut map, field::CONTENT_TEXT)? {
        Some(WireValue::Text(text)) => Some(text),
        Some(_) => return Err(ProtocolError::InvalidEventBinding),
        None => None,
    };
    let attachment = take_optional_id::<32>(&mut map, field::CONTENT_ATTACHMENT_FILE_ID)?;
    let extension = match take_optional(&mut map, field::CONTENT_EXTENSION)? {
        Some(value) => Some(parse_extension(value)?),
        None => None,
    };
    // 新字段可缺席（旧字节 → None）；出现时必须是 1–512 字节的 bytes。
    let attachment_metadata = match map.remove(&field::CONTENT_ATTACHMENT_METADATA) {
        None => None,
        Some(WireValue::Bytes(bytes)) => {
            if bytes.is_empty() || bytes.len() > MAX_ATTACHMENT_METADATA_BYTES {
                return Err(ProtocolError::InvalidCandidate);
            }
            Some(bytes)
        }
        Some(_) => return Err(ProtocolError::InvalidCandidate),
    };
    let reply_conversation = take_optional_id::<32>(&mut map, field::REPLY_CONVERSATION_ID)?;
    let reply_message = take_optional_id::<32>(&mut map, field::REPLY_MESSAGE_ID)?;
    let origin = MessageOrigin::from_u64(take_u64(&mut map, field::MESSAGE_ORIGIN)?)
        .ok_or(ProtocolError::InvalidEventBinding)?;
    let sent_at_unix_ms = take_u64(&mut map, field::EVENT_SENT_AT)?;
    if !map.is_empty() {
        return Err(ProtocolError::InvalidEventBinding);
    }

    let content = match content_kind {
        CONTENT_KIND_TEXT => {
            let (Some(text), None, None) = (text, attachment, extension) else {
                return Err(ProtocolError::InvalidEventBinding);
            };
            if attachment_metadata.is_some() {
                return Err(ProtocolError::InvalidCandidate);
            }
            ApplicationContent::Text { text }
        }
        CONTENT_KIND_ATTACHMENT => {
            let (None, Some(file_id), None) = (text, attachment, extension) else {
                return Err(ProtocolError::InvalidEventBinding);
            };
            ApplicationContent::Attachment {
                attachment: AttachmentRef {
                    file_id: ObjectId::from_bytes(file_id),
                    attachment_metadata,
                },
            }
        }
        CONTENT_KIND_EXTENSION => {
            let (None, None, Some(extension)) = (text, attachment, extension) else {
                return Err(ProtocolError::InvalidEventBinding);
            };
            if attachment_metadata.is_some() {
                return Err(ProtocolError::InvalidCandidate);
            }
            ApplicationContent::Extension { extension }
        }
        _ => return Err(ProtocolError::InvalidEventBinding),
    };
    let reply = match (reply_conversation, reply_message) {
        (Some(conversation), Some(message_id)) => Some(ReplyTarget {
            conversation: ConversationId::from_bytes(conversation),
            message_id: MessageId::from_bytes(message_id),
        }),
        (None, None) => None,
        _ => return Err(ProtocolError::InvalidEventBinding),
    };

    let event = PlainApplicationEvent {
        protocol_version,
        conversation_id,
        group_id,
        event_id,
        account_id,
        device_id,
        content,
        reply,
        origin,
        sent_at_unix_ms,
    };
    // 规范往返：重新编码必须与输入字节完全一致，否则拒绝（非规范/未知表示）。
    let reencoded = canonical_event_bytes(&event).map_err(|error| match error {
        ProtocolError::ContentTooLarge { .. } => error,
        _ => ProtocolError::InvalidEventBinding,
    })?;
    if reencoded != bytes {
        return Err(ProtocolError::InvalidEventBinding);
    }
    Ok(event)
}

/// PROTOCOL-009：校验内层事件与 MLS 发送者、外层候选的绑定。
///
/// 顺序：有界解码 → 外层类别为应用候选 → MLS sender 与外层凭据作者一致
/// （否则 `INVALID_CREDENTIAL`）→ 内层 group/作者设备与外层一致 →
/// 返回带内层去重键的认证事件。外层 message/候选摘要只作审计关联。
pub fn verify_inner_event(
    decrypted: &DecryptedEvent,
    sender: MlsSender,
    outer: &VerifiedCandidate,
) -> Result<AuthenticatedEvent> {
    let bytes = decrypted.bytes();
    if bytes.len() > MAX_APPLICATION_EVENT_BYTES {
        return Err(ProtocolError::ContentTooLarge {
            limit: MAX_APPLICATION_EVENT_BYTES,
        });
    }
    let event = decode_application_event(bytes, DecodeLimits::DEFAULT)?;
    // 内外事件类别匹配：外层必须是应用候选（控制候选不能携带应用正文）。
    if outer.kind() != CandidateKind::Application {
        return Err(ProtocolError::InvalidEventBinding);
    }
    // MLS sender 必须与外层已验证作者完全一致。
    if sender.account_id != outer.author_account() || sender.device_id != outer.author_device() {
        return Err(ProtocolError::InvalidCredential);
    }
    // 内层事件必须绑定同一群与同一作者设备。
    if event.group_id != outer.group_id() {
        return Err(ProtocolError::InvalidEventBinding);
    }
    if event.account_id != sender.account_id || event.device_id != sender.device_id {
        return Err(ProtocolError::InvalidEventBinding);
    }
    let mut preimage = Vec::with_capacity(EVENT_DIGEST_DOMAIN.len() + bytes.len());
    preimage.extend_from_slice(EVENT_DIGEST_DOMAIN);
    preimage.extend_from_slice(bytes);
    Ok(AuthenticatedEvent {
        event,
        digest: Digest::sha256(&preimage),
        sender,
        outer_digest: outer.candidate_digest(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use crate::verify::SignedCandidate;
    use crate::verify::test_support::{
        DEVICE_SEED, context, credential, sign_with, unsigned_candidate, verified_application,
        verified_control,
    };
    use crate::verify::verify_candidate;

    fn text_event() -> PlainApplicationEvent {
        PlainApplicationEvent {
            protocol_version: ProtocolVersion::V1,
            conversation_id: ConversationId::from_bytes([0xc1; 32]),
            group_id: context().group_id,
            event_id: EventId::from_bytes([0xe1; 32]),
            account_id: credential(&DEVICE_SEED).account_id,
            device_id: credential(&DEVICE_SEED).device_id,
            content: ApplicationContent::Text {
                text: "hello".to_owned(),
            },
            reply: None,
            origin: MessageOrigin::UserInitiated,
            sent_at_unix_ms: 1_700_000_000_000,
        }
    }

    fn extension() -> ValidatedExtension {
        validate_extension(
            ExtensionContent {
                namespace: "notes.v1".to_owned(),
                schema_version: 1,
                operation_id: crate::ids::OperationId::from_bytes([1; 32]),
                object_id: ObjectId::from_bytes([2; 32]),
                payload: b"opaque".to_vec(),
                fallback_text: "插件未安装".to_owned(),
            },
            ContentLimits::DEFAULT,
        )
        .unwrap()
    }

    fn sender() -> MlsSender {
        MlsSender {
            account_id: credential(&DEVICE_SEED).account_id,
            device_id: credential(&DEVICE_SEED).device_id,
        }
    }

    #[test]
    fn text_event_round_trips_deterministically() {
        let first = encode_application_event(text_event()).unwrap();
        let second = encode_application_event(text_event()).unwrap();
        assert_eq!(first.as_bytes(), second.as_bytes());
        let decoded = decode_application_event(first.as_bytes(), DecodeLimits::DEFAULT).unwrap();
        assert_eq!(decoded, text_event());
        // 时间变化只影响字节，不影响事件身份绑定。
        let mut later = text_event();
        later.sent_at_unix_ms += 1;
        assert_ne!(
            encode_application_event(later).unwrap().as_bytes(),
            first.as_bytes()
        );
        // 敏感缓冲区不打印内容。
        let rendered = format!("{first:?}");
        assert!(!rendered.contains("hello"));
        assert!(rendered.contains("redacted"));
    }

    #[test]
    fn extension_and_attachment_content_round_trip() {
        let mut event = text_event();
        event.content = ApplicationContent::Attachment {
            attachment: AttachmentRef {
                file_id: ObjectId::from_bytes([3; 32]),
                attachment_metadata: None,
            },
        };
        let bytes = encode_application_event(event.clone()).unwrap();
        assert_eq!(
            decode_application_event(bytes.as_bytes(), DecodeLimits::DEFAULT).unwrap(),
            event
        );
        let mut event = text_event();
        event.content = ApplicationContent::Extension {
            extension: extension(),
        };
        let bytes = encode_application_event(event.clone()).unwrap();
        assert_eq!(
            decode_application_event(bytes.as_bytes(), DecodeLimits::DEFAULT).unwrap(),
            event
        );
    }

    #[test]
    fn reply_must_stay_in_the_same_conversation() {
        let mut event = text_event();
        event.reply = Some(ReplyTarget {
            conversation: ConversationId::from_bytes([0x99; 32]),
            message_id: MessageId::from_bytes([4; 32]),
        });
        assert_eq!(
            encode_application_event(event).unwrap_err(),
            ProtocolError::InvalidReply
        );
        let mut ok = text_event();
        ok.reply = Some(ReplyTarget {
            conversation: ok.conversation_id,
            message_id: MessageId::from_bytes([4; 32]),
        });
        let bytes = encode_application_event(ok.clone()).unwrap();
        assert_eq!(
            decode_application_event(bytes.as_bytes(), DecodeLimits::DEFAULT).unwrap(),
            ok
        );
        // 零消息 ID 也拒绝。
        let mut zero = text_event();
        zero.reply = Some(ReplyTarget {
            conversation: zero.conversation_id,
            message_id: MessageId::from_bytes([0; 32]),
        });
        assert!(encode_application_event(zero).is_err());
    }

    #[test]
    fn malformed_or_oversized_events_are_bounded() {
        let mut empty_text = text_event();
        empty_text.content = ApplicationContent::Text {
            text: "   ".to_owned(),
        };
        assert_eq!(
            encode_application_event(empty_text).unwrap_err(),
            ProtocolError::InvalidEventBinding
        );
        let mut zero_event = text_event();
        zero_event.event_id = EventId::from_bytes([0; 32]);
        assert_eq!(
            encode_application_event(zero_event).unwrap_err(),
            ProtocolError::InvalidEventBinding
        );
        let mut oversized = text_event();
        oversized.content = ApplicationContent::Text {
            text: "a".repeat(MAX_EVENT_TEXT_BYTES + 1),
        };
        assert_eq!(
            encode_application_event(oversized).unwrap_err().code(),
            ErrorCode::ContentTooLarge
        );
        // 恰好 8 KiB 文本允许（UTF-8 完整字节）。
        let mut boundary = text_event();
        boundary.content = ApplicationContent::Text {
            text: "a".repeat(MAX_EVENT_TEXT_BYTES),
        };
        assert!(encode_application_event(boundary).is_ok());

        let bytes = encode_application_event(text_event()).unwrap();
        // 截断。
        assert!(
            decode_application_event(&bytes.as_bytes()[..bytes.len() - 1], DecodeLimits::DEFAULT)
                .is_err()
        );
        // 多余已知字段 → 字段数不符。
        let body = WireValue::structure([
            WireField::new(field::SCHEMA_VERSION, WireValue::U64(1)),
            WireField::new(field::PROTOCOL_MAJOR, WireValue::U64(1)),
            WireField::new(field::PROTOCOL_MINOR, WireValue::U64(0)),
            WireField::new(field::POLICY_VERSION, WireValue::U64(7)),
        ]);
        let extra = encode_canonical(&body, ProtocolVersion::V1).unwrap();
        assert_eq!(
            decode_application_event(extra.as_bytes(), DecodeLimits::DEFAULT).unwrap_err(),
            ProtocolError::InvalidEventBinding
        );
        // 超大输入在解析前拒绝。
        let huge = vec![0u8; MAX_APPLICATION_EVENT_BYTES + 1];
        assert_eq!(
            decode_application_event(&huge, DecodeLimits::DEFAULT)
                .unwrap_err()
                .code(),
            ErrorCode::ContentTooLarge
        );
    }

    #[test]
    fn inner_event_binds_sender_and_outer_candidate() {
        let bytes = encode_application_event(text_event()).unwrap();
        let decrypted = DecryptedEvent::from_bytes(bytes.as_bytes().to_vec());
        let outer = verified_application();
        let authenticated = verify_inner_event(&decrypted, sender(), &outer).unwrap();
        assert_eq!(authenticated.event_id(), text_event().event_id);
        assert_eq!(authenticated.digest(), authenticated.dedupe_key());
        assert_eq!(authenticated.outer_digest(), outer.candidate_digest());
        // 内层摘要与裸字节摘要不同（域分隔）。
        assert_ne!(authenticated.digest(), Digest::sha256(bytes.as_bytes()));
        // MLS sender 与外层设备不一致 → INVALID_CREDENTIAL。
        let wrong_sender = MlsSender {
            account_id: credential(&DEVICE_SEED).account_id,
            device_id: DeviceId::from_bytes([0x99; 32]),
        };
        assert_eq!(
            verify_inner_event(&decrypted, wrong_sender, &outer).unwrap_err(),
            ProtocolError::InvalidCredential
        );
        // 控制候选不能当应用候选。
        assert_eq!(
            verify_inner_event(&decrypted, sender(), &verified_control()).unwrap_err(),
            ProtocolError::InvalidEventBinding
        );
    }

    #[test]
    fn cross_group_inner_event_is_rejected() {
        let mut event = text_event();
        event.group_id = GroupId::from_bytes([0x99; 32]);
        let bytes = encode_application_event(event).unwrap();
        let decrypted = DecryptedEvent::from_bytes(bytes.as_bytes().to_vec());
        assert_eq!(
            verify_inner_event(&decrypted, sender(), &verified_application()).unwrap_err(),
            ProtocolError::InvalidEventBinding
        );
    }

    #[test]
    fn forged_outer_candidate_does_not_change_inner_dedupe_key() {
        let bytes = encode_application_event(text_event()).unwrap();
        let decrypted = DecryptedEvent::from_bytes(bytes.as_bytes().to_vec());
        let first = verify_inner_event(&decrypted, sender(), &verified_application()).unwrap();
        // 另一个外层候选（不同密文）仍得到同一内层去重键。
        let mut other_unsigned = unsigned_candidate();
        other_unsigned.encrypted_payload = b"different-ciphertext".to_vec();
        let other = verify_candidate(
            sign_with(&other_unsigned, &DEVICE_SEED),
            credential(&DEVICE_SEED),
            context(),
        )
        .unwrap();
        let second = verify_inner_event(&decrypted, sender(), &other).unwrap();
        assert_eq!(first.dedupe_key(), second.dedupe_key());
        assert_ne!(first.outer_digest(), second.outer_digest());
    }

    #[test]
    fn tampered_or_non_canonical_decrypted_bytes_are_rejected() {
        let bytes = encode_application_event(text_event()).unwrap();
        // 破坏魔数：结构解码失败。
        let mut bad_magic = bytes.as_bytes().to_vec();
        bad_magic[0] ^= 0xff;
        let decrypted = DecryptedEvent::from_bytes(bad_magic);
        assert_eq!(
            verify_inner_event(&decrypted, sender(), &verified_application()).unwrap_err(),
            ProtocolError::InvalidEventBinding
        );
        // 尾随字节不是规范编码。
        let mut trailing = bytes.as_bytes().to_vec();
        trailing.push(0);
        assert_eq!(
            verify_inner_event(
                &DecryptedEvent::from_bytes(trailing),
                sender(),
                &verified_application()
            )
            .unwrap_err(),
            ProtocolError::InvalidEventBinding
        );
        // 截断。
        let truncated = bytes.as_bytes()[..bytes.len() - 1].to_vec();
        assert_eq!(
            verify_inner_event(
                &DecryptedEvent::from_bytes(truncated),
                sender(),
                &verified_application()
            )
            .unwrap_err(),
            ProtocolError::InvalidEventBinding
        );
    }

    fn attachment_event(metadata: Option<Vec<u8>>) -> PlainApplicationEvent {
        let mut event = text_event();
        event.content = ApplicationContent::Attachment {
            attachment: AttachmentRef {
                file_id: ObjectId::from_bytes([3; 32]),
                attachment_metadata: metadata,
            },
        };
        event
    }

    #[test]
    fn attachment_metadata_round_trips_old_and_new_bytes() {
        // 旧事件：无 metadata，编码/解码往返且不带新字段。
        let old_event = attachment_event(None);
        let old_bytes = encode_application_event(old_event.clone()).unwrap();
        assert_eq!(
            decode_application_event(old_bytes.as_bytes(), DecodeLimits::DEFAULT).unwrap(),
            old_event
        );
        let WireValue::Struct(old_fields) = event_body(&old_event) else {
            panic!("event body must be a struct");
        };
        assert!(
            old_fields
                .iter()
                .all(|field| field.id != field::CONTENT_ATTACHMENT_METADATA)
        );
        // 新事件：带 metadata，往返一致且字节不同。
        let new_event = attachment_event(Some(vec![0xaa; 8]));
        let new_bytes = encode_application_event(new_event.clone()).unwrap();
        assert_ne!(old_bytes.as_bytes(), new_bytes.as_bytes());
        assert_eq!(
            decode_application_event(new_bytes.as_bytes(), DecodeLimits::DEFAULT).unwrap(),
            new_event
        );
        // 恰好 512 字节可接受；空与超长显式拒绝。
        let max_event = attachment_event(Some(vec![7; MAX_ATTACHMENT_METADATA_BYTES]));
        let max_bytes = encode_application_event(max_event.clone()).unwrap();
        assert_eq!(
            decode_application_event(max_bytes.as_bytes(), DecodeLimits::DEFAULT).unwrap(),
            max_event
        );
        assert_eq!(
            encode_application_event(attachment_event(Some(Vec::new())))
                .unwrap_err()
                .code(),
            crate::error::ErrorCode::InvalidCandidate
        );
        assert_eq!(
            encode_application_event(attachment_event(Some(vec![
                7;
                MAX_ATTACHMENT_METADATA_BYTES
                    + 1
            ])))
            .unwrap_err()
            .code(),
            crate::error::ErrorCode::InvalidCandidate
        );
    }

    #[test]
    fn attachment_metadata_shape_is_validated_on_decode() {
        // 字段值类型不对（不是 bytes）→ INVALID_CANDIDATE。
        let mut body = event_body(&attachment_event(Some(vec![1; 4])));
        if let WireValue::Struct(fields) = &mut body {
            for field in fields.iter_mut() {
                if field.id == field::CONTENT_ATTACHMENT_METADATA {
                    field.value = WireValue::U64(1);
                }
            }
        }
        let bytes = encode_canonical(&body, ProtocolVersion::V1).unwrap();
        assert_eq!(
            decode_application_event(bytes.as_bytes(), DecodeLimits::DEFAULT)
                .unwrap_err()
                .code(),
            crate::error::ErrorCode::InvalidCandidate
        );
        // 空 metadata 字段 → INVALID_CANDIDATE。
        let mut body = event_body(&attachment_event(Some(vec![1; 4])));
        if let WireValue::Struct(fields) = &mut body {
            for field in fields.iter_mut() {
                if field.id == field::CONTENT_ATTACHMENT_METADATA {
                    field.value = WireValue::Bytes(Vec::new());
                }
            }
        }
        let bytes = encode_canonical(&body, ProtocolVersion::V1).unwrap();
        assert_eq!(
            decode_application_event(bytes.as_bytes(), DecodeLimits::DEFAULT)
                .unwrap_err()
                .code(),
            crate::error::ErrorCode::InvalidCandidate
        );
        // 无附件内容里出现 metadata → INVALID_CANDIDATE。
        let mut body = event_body(&text_event());
        if let WireValue::Struct(fields) = &mut body {
            fields.push(WireField::new(
                field::CONTENT_ATTACHMENT_METADATA,
                WireValue::Bytes(vec![1; 4]),
            ));
            fields.sort_by_key(|field| field.id);
        }
        let bytes = encode_canonical(&body, ProtocolVersion::V1).unwrap();
        assert_eq!(
            decode_application_event(bytes.as_bytes(), DecodeLimits::DEFAULT)
                .unwrap_err()
                .code(),
            crate::error::ErrorCode::InvalidCandidate
        );
        // 字段数超过 16/17 → INVALID_EVENT_BINDING。
        let mut body = event_body(&text_event());
        if let WireValue::Struct(fields) = &mut body {
            fields.push(WireField::new(
                field::CONTROL_HEAD,
                WireValue::Bytes(vec![0; 32]),
            ));
            fields.push(WireField::new(
                field::SUBMISSION_ID,
                WireValue::Bytes(vec![0; 32]),
            ));
            fields.sort_by_key(|field| field.id);
        }
        let bytes = encode_canonical(&body, ProtocolVersion::V1).unwrap();
        assert_eq!(
            decode_application_event(bytes.as_bytes(), DecodeLimits::DEFAULT).unwrap_err(),
            ProtocolError::InvalidEventBinding
        );
    }

    #[test]
    fn attachment_metadata_tampering_breaks_candidate_signature() {
        // metadata 在 MLS 密文内；外层候选签名覆盖密文摘要，因此换 metadata 会让原签名失效。
        let original_event = attachment_event(Some(vec![0xaa; 8]));
        let original_bytes = encode_application_event(original_event).unwrap();
        let mut unsigned = unsigned_candidate();
        unsigned.encrypted_payload = original_bytes.as_bytes().to_vec();
        let signed = sign_with(&unsigned, &DEVICE_SEED);
        let verified = verify_candidate(signed.clone(), credential(&DEVICE_SEED), context())
            .expect("original candidate must verify");
        assert_eq!(verified.kind(), CandidateKind::Application);

        let tampered_event = attachment_event(Some(vec![0xab; 8]));
        let tampered_bytes = encode_application_event(tampered_event).unwrap();
        let mut tampered = unsigned.clone();
        tampered.encrypted_payload = tampered_bytes.as_bytes().to_vec();
        assert_eq!(
            verify_candidate(
                SignedCandidate {
                    unsigned: tampered,
                    author_signature: signed.author_signature,
                },
                credential(&DEVICE_SEED),
                context(),
            )
            .unwrap_err(),
            ProtocolError::InvalidSignature
        );
        // 内层去重键也随 metadata 改变。
        let first = verify_inner_event(
            &DecryptedEvent::from_bytes(original_bytes.as_bytes().to_vec()),
            sender(),
            &verified_application(),
        )
        .unwrap();
        let second = verify_inner_event(
            &DecryptedEvent::from_bytes(tampered_bytes.as_bytes().to_vec()),
            sender(),
            &verified_application(),
        )
        .unwrap();
        assert_ne!(first.digest(), second.digest());
    }
}
