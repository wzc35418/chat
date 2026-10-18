//! STORAGE-012/013 的内部认证分页游标。
//!
//! 契约（[docs/implementation/storage.md]）要求游标语义与 PROTOCOL-012/013 对齐：
//! 稳定键集（keyset）、认证、防跨范围重放、过期即显式失败、失败绝不退化为“跳到最新页”。
//! 线协议的 `encode_page_cursor`/`decode_page_cursor`（crates/protocol/src/cursor.rs）
//! 已实现，但它的作用域要求账户/设备/会话的 32 字节二进制身份，而正式存储当前只有
//! TEXT 会话标识、也不保存本机账户/设备身份。因此这里先提供**内部游标**：
//!
//! - 文本格式 `spg1.<hex>`，与线协议 `cur1` 前缀不同，不能混用或上网；
//! - 载荷（资源类别、排序版本、位置、签发/过期时间）用主密钥的独立用途子密钥
//!   ChaCha20-Poly1305 认证加密，AAD 绑定“会话 + 资源 + 排序版本”，因此换会话、
//!   换资源、换排序版本或任意改字节都会认证失败；
//! - 过期检查使用调用方注入的可信时间（生产为系统 UTC 时钟，测试可注入），
//!   过期/未来签发/超出 TTL 一律返回 `INVALID_CURSOR`。
//!
//! 限制：这不是线协议令牌。等核心层提供账户/设备二进制身份映射后，应改用
//! PROTOCOL-012/013 的 `cur1` 令牌，本模块只保留为内部实现细节。

use std::fmt;

use super::error::StorageError;
use super::seal::{ProtectedStorageKey, RecordSealer, SealPurpose};
use super::{Result, ensure_id, hex};

/// 内部游标文本前缀（与线协议 `cur1` 区分）。
pub const PAGE_CURSOR_PREFIX: &str = "spg1";
/// 游标文本长度上限（超过即拒绝，不解码）。
pub const MAX_PAGE_CURSOR_BYTES: usize = 1024;
/// 游标生命周期：10 分钟，与 PROTOCOL-012 的 `CURSOR_TTL_MS` 一致。
pub const PAGE_CURSOR_TTL_MS: i64 = 600_000;
/// 允许的时钟超前后缀：60 秒，与 PROTOCOL-012 一致。
pub const PAGE_CURSOR_MAX_CLOCK_SKEW_MS: i64 = 60_000;
/// 消息页排序版本（位置语义变化时必须递增）。
pub const MESSAGE_SORT_VERSION: u16 = 1;
/// 会话列表排序版本。
pub const CONVERSATION_SORT_VERSION: u16 = 1;

const PAYLOAD_VERSION: u8 = 1;
const POSITION_PENDING: u8 = 1;
const POSITION_FORMAL: u8 = 2;
const POSITION_CONVERSATION: u8 = 3;

/// 游标所属资源类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PageResource {
    /// 单会话消息页。
    Messages = 1,
    /// 会话列表页。
    Conversations = 2,
}

impl PageResource {
    const fn tag(self) -> u8 {
        self as u8
    }

    const fn from_tag(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Messages),
            2 => Some(Self::Conversations),
            _ => None,
        }
    }
}

/// 游标作用域：会话（会话列表为空）+ 资源 + 排序版本。
#[derive(Clone, Copy, Debug)]
pub(crate) struct CursorScope<'a> {
    /// 绑定会话（会话列表页传空字符串）。
    pub(crate) conversation_id: &'a str,
    /// 资源类别。
    pub(crate) resource: PageResource,
    /// 排序版本。
    pub(crate) sort_version: u16,
}

/// 稳定键集位置。三个变体与资源类别一一对应。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PagePosition {
    /// 待发意图段：(本地创建时间, 草稿 ID) 降序。
    Pending { created_at: i64, draft_id: String },
    /// 正式消息段：(正式序号, 消息 ID) 降序。
    Formal {
        group_seq: i64,
        message_id: String,
    },
    /// 会话列表：(最近活动时间, 会话 ID) 降序。
    Conversation {
        last_activity_at: i64,
        conversation_id: String,
    },
}

impl PagePosition {
    fn tag(&self) -> u8 {
        match self {
            Self::Pending { .. } => POSITION_PENDING,
            Self::Formal { .. } => POSITION_FORMAL,
            Self::Conversation { .. } => POSITION_CONVERSATION,
        }
    }

    fn resource(&self) -> PageResource {
        match self {
            Self::Pending { .. } | Self::Formal { .. } => PageResource::Messages,
            Self::Conversation { .. } => PageResource::Conversations,
        }
    }
}

/// 对外只暴露不透明文本；构造本身不代表可信（认证与过期仍由解码校验）。
#[derive(Clone, PartialEq, Eq)]
pub struct PageCursor(String);

impl PageCursor {
    /// 游标文本。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 由调用方传入的文本构造（例如从 UI 状态或 FFI 参数读回）。
    pub fn from_text(text: impl Into<String>) -> Self {
        Self(text.into())
    }
}

impl fmt::Debug for PageCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "PageCursor({}…)",
            &self.0[..self.0.len().min(12)]
        )
    }
}

impl fmt::Display for PageCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn malformed(reason: &str) -> StorageError {
    StorageError::InvalidCursor(reason.to_owned())
}

fn push_str(buffer: &mut Vec<u8>, value: &str) -> Result<()> {
    let length = u16::try_from(value.len())
        .map_err(|_| StorageError::InvalidInput("page cursor field is too long".to_owned()))?;
    buffer.extend_from_slice(&length.to_be_bytes());
    buffer.extend_from_slice(value.as_bytes());
    Ok(())
}

fn position_payload(position: &PagePosition) -> Result<Vec<u8>> {
    let mut buffer = Vec::with_capacity(64);
    buffer.push(position.tag());
    match position {
        PagePosition::Pending {
            created_at,
            draft_id,
        } => {
            ensure_id("draft", draft_id)?;
            buffer.extend_from_slice(&created_at.to_be_bytes());
            push_str(&mut buffer, draft_id)?;
        }
        PagePosition::Formal {
            group_seq,
            message_id,
        } => {
            ensure_id("message", message_id)?;
            buffer.extend_from_slice(&group_seq.to_be_bytes());
            push_str(&mut buffer, message_id)?;
        }
        PagePosition::Conversation {
            last_activity_at,
            conversation_id,
        } => {
            ensure_id("conversation", conversation_id)?;
            buffer.extend_from_slice(&last_activity_at.to_be_bytes());
            push_str(&mut buffer, conversation_id)?;
        }
    }
    Ok(buffer)
}

/// 编码内部认证游标。`issued_at_ms` 必须由可信时间来源提供。
pub(crate) fn encode_page_cursor(
    key: &ProtectedStorageKey,
    scope: &CursorScope<'_>,
    position: &PagePosition,
    issued_at_ms: i64,
) -> Result<PageCursor> {
    if issued_at_ms < 0 {
        return Err(StorageError::InvalidInput(
            "cursor issue time must be a non-negative Unix millisecond timestamp".to_owned(),
        ));
    }
    if scope.sort_version == 0 {
        return Err(StorageError::InvalidInput(
            "cursor sort version must be non-zero".to_owned(),
        ));
    }
    if position.resource() != scope.resource {
        return Err(StorageError::InvalidInput(
            "cursor position does not belong to the requested resource".to_owned(),
        ));
    }
    let expires_at_ms = issued_at_ms
        .checked_add(PAGE_CURSOR_TTL_MS)
        .ok_or_else(|| StorageError::InvalidInput("cursor expiry overflows".to_owned()))?;

    let mut payload = Vec::with_capacity(96);
    payload.push(PAYLOAD_VERSION);
    payload.push(scope.resource.tag());
    payload.extend_from_slice(&scope.sort_version.to_be_bytes());
    payload.extend_from_slice(&issued_at_ms.to_be_bytes());
    payload.extend_from_slice(&expires_at_ms.to_be_bytes());
    payload.extend_from_slice(position_payload(position)?.as_slice());

    let sealed = seal_payload(key, scope, &payload)?;
    let token = format!("{PAGE_CURSOR_PREFIX}.{}", hex(&sealed));
    if token.len() > MAX_PAGE_CURSOR_BYTES {
        return Err(StorageError::Failure(
            "encoded page cursor exceeds the bounded token size".to_owned(),
        ));
    }
    Ok(PageCursor(token))
}

/// 校验并解码内部游标。任何失败都是 `INVALID_CURSOR`，绝不返回“最新页”。
pub(crate) fn decode_page_cursor(
    key: &ProtectedStorageKey,
    scope: &CursorScope<'_>,
    cursor: &PageCursor,
    now_unix_ms: i64,
) -> Result<PagePosition> {
    if now_unix_ms < 0 {
        return Err(StorageError::InvalidInput(
            "cursor validation time must be a non-negative Unix millisecond timestamp".to_owned(),
        ));
    }
    let text = cursor.as_str();
    if text.len() > MAX_PAGE_CURSOR_BYTES {
        return Err(malformed("page cursor exceeds the bounded token size"));
    }
    let Some(rest) = text
        .strip_prefix(PAGE_CURSOR_PREFIX)
        .and_then(|rest| rest.strip_prefix('.'))
    else {
        return Err(malformed("page cursor has an unknown token format"));
    };
    let sealed = decode_hex(rest)?;
    let payload = open_payload(key, scope, &sealed)?;

    let mut reader = Reader::new(payload.as_slice());
    let version = reader.u8()?;
    if version != PAYLOAD_VERSION {
        return Err(malformed("page cursor payload version is unsupported"));
    }
    let resource = PageResource::from_tag(reader.u8()?)
        .ok_or_else(|| malformed("page cursor resource is unknown"))?;
    if resource != scope.resource {
        return Err(malformed("page cursor belongs to another resource"));
    }
    let sort_version = reader.u16()?;
    if sort_version != scope.sort_version {
        return Err(malformed("page cursor sort version is stale"));
    }
    let issued_at_ms = reader.i64()?;
    let expires_at_ms = reader.i64()?;
    if expires_at_ms <= now_unix_ms {
        return Err(malformed("page cursor has expired"));
    }
    if issued_at_ms > now_unix_ms.saturating_add(PAGE_CURSOR_MAX_CLOCK_SKEW_MS) {
        return Err(malformed("page cursor was issued in the future"));
    }
    if expires_at_ms > issued_at_ms.saturating_add(PAGE_CURSOR_TTL_MS) {
        return Err(malformed("page cursor lifetime exceeds the policy"));
    }

    let position = match reader.u8()? {
        POSITION_PENDING => {
            if resource != PageResource::Messages {
                return Err(malformed("page cursor position does not match the resource"));
            }
            let created_at = reader.i64()?;
            if created_at < 0 {
                return Err(malformed("page cursor position is out of range"));
            }
            let draft_id = reader.id("draft")?;
            PagePosition::Pending {
                created_at,
                draft_id,
            }
        }
        POSITION_FORMAL => {
            if resource != PageResource::Messages {
                return Err(malformed("page cursor position does not match the resource"));
            }
            let group_seq = reader.i64()?;
            if group_seq <= 0 {
                return Err(malformed("page cursor position is out of range"));
            }
            let message_id = reader.id("message")?;
            PagePosition::Formal {
                group_seq,
                message_id,
            }
        }
        POSITION_CONVERSATION => {
            if resource != PageResource::Conversations {
                return Err(malformed("page cursor position does not match the resource"));
            }
            let last_activity_at = reader.i64()?;
            if last_activity_at < 0 {
                return Err(malformed("page cursor position is out of range"));
            }
            let conversation_id = reader.id("conversation")?;
            PagePosition::Conversation {
                last_activity_at,
                conversation_id,
            }
        }
        _ => return Err(malformed("page cursor position tag is unknown")),
    };
    if !reader.is_empty() {
        return Err(malformed("page cursor payload has trailing bytes"));
    }
    Ok(position)
}

const CURSOR_AAD_DOMAIN: &[u8] = b"chat/storage/page-cursor/v1";

/// AAD 需要稳定的“资源 + 排序版本”字节序列；长度固定 3 字节，栈上保存。
fn scope_meta(scope: &CursorScope<'_>) -> [u8; 3] {
    let version = scope.sort_version.to_be_bytes();
    [scope.resource.tag(), version[0], version[1]]
}

/// 会话标识、资源标签与排序版本一起作为 AAD，跨会话/跨资源/旧排序版本都会认证失败。
fn seal_payload(
    key: &ProtectedStorageKey,
    scope: &CursorScope<'_>,
    payload: &[u8],
) -> Result<Vec<u8>> {
    let meta = scope_meta(scope);
    let sealer = RecordSealer::new(key, SealPurpose::PageCursor);
    sealer.seal(
        &[scope.conversation_id.as_bytes(), &meta, CURSOR_AAD_DOMAIN],
        payload,
    )
}

fn open_payload(
    key: &ProtectedStorageKey,
    scope: &CursorScope<'_>,
    sealed: &[u8],
) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    let meta = scope_meta(scope);
    let sealer = RecordSealer::new(key, SealPurpose::PageCursor);
    sealer
        .open(&[scope.conversation_id.as_bytes(), &meta, CURSOR_AAD_DOMAIN], sealed)
        .map_err(|_| malformed("page cursor authentication failed"))
}

fn decode_hex(text: &str) -> Result<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(malformed("page cursor encoding has an odd length"));
    }
    let mut decoded = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        decoded.push((hex_digit(pair[0])? << 4) | hex_digit(pair[1])?);
    }
    Ok(decoded)
}

fn hex_digit(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(malformed("page cursor encoding is not canonical")),
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or_else(|| malformed("page cursor payload is truncated"))?;
        if end > self.bytes.len() {
            return Err(malformed("page cursor payload is truncated"));
        }
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn i64(&mut self) -> Result<i64> {
        let bytes = self.take(8)?;
        let mut fixed = [0u8; 8];
        fixed.copy_from_slice(bytes);
        Ok(i64::from_be_bytes(fixed))
    }

    fn id(&mut self, kind: &str) -> Result<String> {
        let bytes = self.take(2)?;
        let length = usize::from(u16::from_be_bytes([bytes[0], bytes[1]]));
        if length == 0 || length > super::MAX_ID_BYTES {
            return Err(malformed("page cursor identifier length is out of range"));
        }
        let raw = self.take(length)?;
        let value = std::str::from_utf8(raw)
            .map_err(|_| malformed("page cursor identifier is not valid UTF-8"))?
            .to_owned();
        ensure_id(kind, &value)
            .map_err(|_| malformed("page cursor identifier violates the ID contract"))?;
        Ok(value)
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> ProtectedStorageKey {
        ProtectedStorageKey::from_bytes([0x27; 32])
    }

    fn messages_scope() -> CursorScope<'static> {
        CursorScope {
            conversation_id: "conv-1",
            resource: PageResource::Messages,
            sort_version: MESSAGE_SORT_VERSION,
        }
    }

    fn formal_position() -> PagePosition {
        PagePosition::Formal {
            group_seq: 42,
            message_id: "msg-42".to_owned(),
        }
    }

    #[test]
    fn cursor_round_trip_and_prefix_are_stable() {
        let cursor = encode_page_cursor(&key(), &messages_scope(), &formal_position(), 1_000_000)
            .expect("encode");
        assert!(cursor.as_str().starts_with("spg1."));
        assert!(!cursor.as_str().contains("msg-42"), "positions must not leak");
        let position = decode_page_cursor(&key(), &messages_scope(), &cursor, 1_000_001).unwrap();
        assert_eq!(position, formal_position());
    }

    #[test]
    fn cursor_fails_closed_for_tamper_scope_and_expiry() {
        let cursor = encode_page_cursor(&key(), &messages_scope(), &formal_position(), 1_000_000)
            .expect("encode");
        let mut tampered: Vec<char> = cursor.as_str().chars().collect();
        let last = tampered.len() - 1;
        tampered[last] = if tampered[last] == '0' { '1' } else { '0' };
        let tampered = PageCursor::from_text(tampered.into_iter().collect::<String>());
        assert_eq!(
            decode_page_cursor(&key(), &messages_scope(), &tampered, 1_000_001)
                .unwrap_err()
                .code(),
            "INVALID_CURSOR"
        );
        let other_conversation = CursorScope {
            conversation_id: "conv-2",
            ..messages_scope()
        };
        assert_eq!(
            decode_page_cursor(&key(), &other_conversation, &cursor, 1_000_001)
                .unwrap_err()
                .code(),
            "INVALID_CURSOR"
        );
        let conversations = CursorScope {
            conversation_id: "",
            resource: PageResource::Conversations,
            sort_version: CONVERSATION_SORT_VERSION,
        };
        assert_eq!(
            decode_page_cursor(&key(), &conversations, &cursor, 1_000_001)
                .unwrap_err()
                .code(),
            "INVALID_CURSOR"
        );
        assert_eq!(
            decode_page_cursor(
                &key(),
                &messages_scope(),
                &cursor,
                1_000_000 + PAGE_CURSOR_TTL_MS
            )
            .unwrap_err()
            .code(),
            "INVALID_CURSOR"
        );
        assert_eq!(
            decode_page_cursor(&key(), &messages_scope(), &cursor, 1_000).unwrap_err().code(),
            "INVALID_CURSOR"
        );
    }
}
