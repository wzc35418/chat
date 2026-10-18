//! PROTOCOL-012/013：认证的不透明分页游标。
//!
//! 游标是 `cur1.<nonce hex>.<ciphertext hex>`，其中
//!
//! - 密文由 ChaCha20-Poly1305（RFC 8439）生成，nonce 为系统安全随机 12 字节；
//! - 关联数据（AAD）为冻结用途域字符串，防止跨用途重放；
//! - 明文是规范编码的 scope/position/时间戳，因此本地索引与位置不会以明文暴露；
//! - 任何篡改（含改位置、换会话、换设备、换账户）都会导致标签校验失败。
//!
//! 过期检查必须有可信时间来源，因此 `encode_page_cursor`/`decode_page_cursor`
//! 在契约签名上补了显式 `unix_ms` 参数（生产由系统 UTC 时钟提供，测试可注入）。
//! 失败绝不能默认跳到最新页。

use core::fmt;
use std::collections::BTreeMap;

use chacha20poly1305::aead::{Aead, Generate, Payload};
use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce};

use crate::canonical::{ProtocolVersion, WireField, WireValue, encode_canonical, field};
use crate::decode::{DecodeLimits, decode_bounded};
use crate::error::{ProtocolError, Result};
use crate::ids::{AccountId, ConversationId, DeviceId};

/// 游标文本版本前缀。
pub const CURSOR_TOKEN_VERSION: &str = "cur1";
/// 游标文本长度上限。
pub const MAX_CURSOR_TOKEN_BYTES: usize = 1024;
/// 冻结的游标生命周期：10 分钟。
pub const CURSOR_TTL_MS: u64 = 600_000;
/// 允许的时钟超前后缀：60 秒。
pub const CURSOR_MAX_CLOCK_SKEW_MS: u64 = 60_000;
/// 排序键上限。
pub const MAX_CURSOR_SORT_KEY_BYTES: usize = 64;
/// 稳定位置序号上限（存储层为 SQLite i64）。
pub const MAX_CURSOR_SEQ: u64 = i64::MAX as u64;
/// 平局决胜字节数。
pub const CURSOR_TIE_BYTES: usize = 16;
/// 游标用途域（作为 AEAD AAD）。
pub const CURSOR_DOMAIN: &[u8] = b"chat/p1/page-cursor/v1";

const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;
const SCHEMA_VERSION: u64 = 1;

/// 游标资源类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorResource {
    /// 会话消息。
    Messages = 1,
    /// 信箱。
    Mailbox = 2,
    /// 附件列表。
    Attachments = 3,
}

impl CursorResource {
    /// 冻结数值。
    pub const fn as_u64(self) -> u64 {
        self as u64
    }

    /// 由冻结数值还原。
    pub const fn from_u64(value: u64) -> Option<Self> {
        match value {
            1 => Some(CursorResource::Messages),
            2 => Some(CursorResource::Mailbox),
            3 => Some(CursorResource::Attachments),
            _ => None,
        }
    }
}

/// 游标作用域：账户/设备/会话/资源/排序版本。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorScope {
    /// 账户。
    pub account: AccountId,
    /// 设备。
    pub device: DeviceId,
    /// 会话。
    pub conversation: ConversationId,
    /// 资源类别。
    pub resource: CursorResource,
    /// 排序版本；旧版本必须要求重查。
    pub sort_version: u16,
}

/// 稳定键集位置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorPosition {
    /// 稳定排序键（非空）。
    pub sort_key: Vec<u8>,
    /// 正式序号。
    pub seq: u64,
    /// 平局决胜字节。
    pub tie: [u8; CURSOR_TIE_BYTES],
}

/// 游标失败原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorError {
    BadFormat,
    NonCanonicalHex,
    Truncated,
    TagVerification,
    Format,
    TimestampOverflow,
    StaleSortVersion,
    TtlExceedsPolicy,
    FutureTimestamp,
    EmptySortKey,
    SortKeyTooLong { limit: usize },
    SequenceOutOfRange { limit: u64 },
}

impl fmt::Display for CursorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CursorError::BadFormat => formatter.write_str("bad token format"),
            CursorError::NonCanonicalHex => formatter.write_str("token hex is not canonical"),
            CursorError::Truncated => formatter.write_str("token is truncated"),
            CursorError::TagVerification => formatter.write_str("token authentication failed"),
            CursorError::Format => formatter.write_str("token payload is malformed"),
            CursorError::TimestampOverflow => formatter.write_str("timestamp overflow"),
            CursorError::StaleSortVersion => formatter.write_str("cursor sort version is stale"),
            CursorError::TtlExceedsPolicy => formatter.write_str("cursor ttl exceeds policy"),
            CursorError::FutureTimestamp => formatter.write_str("cursor issued in the future"),
            CursorError::EmptySortKey => formatter.write_str("cursor sort key is empty"),
            CursorError::SortKeyTooLong { limit } => {
                write!(formatter, "cursor sort key exceeds {limit} bytes")
            }
            CursorError::SequenceOutOfRange { limit } => {
                write!(formatter, "cursor sequence exceeds {limit}")
            }
        }
    }
}

/// 游标认证密钥。`Debug` 不打印密钥材料。
#[derive(Clone)]
pub struct CursorKey {
    key: [u8; 32],
}

impl CursorKey {
    /// 由 32 字节密钥材料构造。
    pub const fn from_bytes(key: [u8; 32]) -> Self {
        Self { key }
    }
}

impl fmt::Debug for CursorKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CursorKey(<redacted>)")
    }
}

/// 对外只暴露不透明文本。
#[derive(Clone, PartialEq, Eq)]
pub struct OpaqueCursor(String);

impl OpaqueCursor {
    /// 令牌文本。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 由外部文本构造（例如从 HTTP query 读回）。长度、格式与认证标签仍由
    /// `decode_page_cursor` 校验，构造本身不代表可信。
    pub fn from_text(text: impl Into<String>) -> Self {
        Self(text.into())
    }
}

impl fmt::Debug for OpaqueCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "OpaqueCursor({}…)",
            &self.0[..self.0.len().min(12)]
        )
    }
}

impl fmt::Display for OpaqueCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: [u8; 16] = *b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[(byte >> 4) as usize]));
        text.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    text
}

fn hex_decode(text: &str) -> Result<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return Err(ProtocolError::InvalidCursor(CursorError::BadFormat));
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let (pairs, remainder) = bytes.as_chunks::<2>();
    debug_assert!(remainder.is_empty());
    for pair in pairs {
        out.push((hex_digit(pair[0])? << 4) | hex_digit(pair[1])?);
    }
    Ok(out)
}

fn hex_digit(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(ProtocolError::InvalidCursor(CursorError::NonCanonicalHex)),
    }
}

fn cipher(key: &CursorKey) -> Result<ChaCha20Poly1305> {
    ChaCha20Poly1305::new_from_slice(&key.key)
        .map_err(|_| ProtocolError::InvalidCursor(CursorError::Format))
}

fn validate_scope(scope: &CursorScope) -> Result<()> {
    if scope.sort_version == 0 {
        return Err(ProtocolError::InvalidCursorScope);
    }
    Ok(())
}

fn validate_position(position: &CursorPosition) -> Result<()> {
    if position.sort_key.is_empty() {
        return Err(ProtocolError::InvalidCursor(CursorError::EmptySortKey));
    }
    if position.sort_key.len() > MAX_CURSOR_SORT_KEY_BYTES {
        return Err(ProtocolError::InvalidCursor(CursorError::SortKeyTooLong {
            limit: MAX_CURSOR_SORT_KEY_BYTES,
        }));
    }
    if position.seq > MAX_CURSOR_SEQ {
        return Err(ProtocolError::InvalidCursor(
            CursorError::SequenceOutOfRange {
                limit: MAX_CURSOR_SEQ,
            },
        ));
    }
    Ok(())
}

fn cursor_body(
    scope: &CursorScope,
    position: &CursorPosition,
    issued_at: u64,
    expires_at: u64,
) -> WireValue {
    WireValue::structure([
        WireField::new(field::SCHEMA_VERSION, WireValue::U64(SCHEMA_VERSION)),
        WireField::new(
            field::CURSOR_ACCOUNT_ID,
            WireValue::id_bytes(scope.account.as_bytes()),
        ),
        WireField::new(
            field::CURSOR_DEVICE_ID,
            WireValue::id_bytes(scope.device.as_bytes()),
        ),
        WireField::new(
            field::CURSOR_CONVERSATION_ID,
            WireValue::id_bytes(scope.conversation.as_bytes()),
        ),
        WireField::new(
            field::CURSOR_RESOURCE,
            WireValue::U64(scope.resource.as_u64()),
        ),
        WireField::new(
            field::CURSOR_SORT_VERSION,
            WireValue::U64(u64::from(scope.sort_version)),
        ),
        WireField::new(
            field::CURSOR_SORT_KEY,
            WireValue::Bytes(position.sort_key.clone()),
        ),
        WireField::new(field::CURSOR_SEQ, WireValue::U64(position.seq)),
        WireField::new(field::CURSOR_TIE, WireValue::Bytes(position.tie.to_vec())),
        WireField::new(field::CURSOR_ISSUED_AT, WireValue::U64(issued_at)),
        WireField::new(field::CURSOR_EXPIRES_AT, WireValue::U64(expires_at)),
    ])
}

/// PROTOCOL-012：生成认证的有界游标。
///
/// scope 与 position 先做范围校验，再规范编码、用系统安全随机 nonce 做
/// ChaCha20-Poly1305 认证加密，最后输出不透明 token。
pub fn encode_page_cursor(
    scope: &CursorScope,
    position: &CursorPosition,
    key: &CursorKey,
    issued_at_unix_ms: u64,
) -> Result<OpaqueCursor> {
    validate_scope(scope)?;
    validate_position(position)?;
    let expires_at = issued_at_unix_ms
        .checked_add(CURSOR_TTL_MS)
        .ok_or(ProtocolError::InvalidCursor(CursorError::TimestampOverflow))?;
    let plaintext = encode_canonical(
        &cursor_body(scope, position, issued_at_unix_ms, expires_at),
        ProtocolVersion::V1,
    )?;
    let nonce_bytes = <[u8; NONCE_BYTES] as Generate>::try_generate()
        .map_err(|_| ProtocolError::RandomUnavailable)?;
    let nonce = Nonce::from(nonce_bytes);
    let ciphertext = cipher(key)?
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext.as_bytes(),
                aad: CURSOR_DOMAIN,
            },
        )
        .map_err(|_| ProtocolError::InvalidCursor(CursorError::Format))?;
    let token = format!(
        "{}.{}.{}",
        CURSOR_TOKEN_VERSION,
        hex_encode(&nonce_bytes),
        hex_encode(&ciphertext)
    );
    if token.len() > MAX_CURSOR_TOKEN_BYTES {
        return Err(ProtocolError::ContentTooLarge {
            limit: MAX_CURSOR_TOKEN_BYTES,
        });
    }
    Ok(OpaqueCursor(token))
}

fn fields_map(value: WireValue) -> Result<BTreeMap<u16, WireValue>> {
    let WireValue::Struct(fields) = value else {
        return Err(ProtocolError::InvalidCursor(CursorError::Format));
    };
    let mut map = BTreeMap::new();
    for field in fields {
        if map.insert(field.id, field.value).is_some() {
            return Err(ProtocolError::InvalidCursor(CursorError::Format));
        }
    }
    Ok(map)
}

fn take_u64(map: &mut BTreeMap<u16, WireValue>, id: u16) -> Result<u64> {
    match map.remove(&id) {
        Some(WireValue::U64(value)) => Ok(value),
        _ => Err(ProtocolError::InvalidCursor(CursorError::Format)),
    }
}

fn take_bytes(map: &mut BTreeMap<u16, WireValue>, id: u16) -> Result<Vec<u8>> {
    match map.remove(&id) {
        Some(WireValue::Bytes(value)) => Ok(value),
        _ => Err(ProtocolError::InvalidCursor(CursorError::Format)),
    }
}

fn take_id<const N: usize>(map: &mut BTreeMap<u16, WireValue>, id: u16) -> Result<[u8; N]> {
    let bytes = take_bytes(map, id)?;
    bytes
        .try_into()
        .map_err(|_| ProtocolError::InvalidCursor(CursorError::Format))
}

/// PROTOCOL-013：校验并解码游标。
///
/// 顺序：长度上限 → 文本结构 → 严格 hex → 认证标签 → 规范编码 payload →
/// scope 匹配（含排序版本）→ 过期与时钟检查 → 位置范围。任何失败都不会
/// 退化为「跳到最新页」。
pub fn decode_page_cursor(
    token: &OpaqueCursor,
    expected: &CursorScope,
    key: &CursorKey,
    now_unix_ms: u64,
) -> Result<CursorPosition> {
    validate_scope(expected)?;
    let text = token.as_str();
    if text.len() > MAX_CURSOR_TOKEN_BYTES {
        return Err(ProtocolError::ContentTooLarge {
            limit: MAX_CURSOR_TOKEN_BYTES,
        });
    }
    let mut parts = text.split('.');
    let (Some(version), Some(nonce_text), Some(ciphertext_text), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(ProtocolError::InvalidCursor(CursorError::BadFormat));
    };
    if version != CURSOR_TOKEN_VERSION {
        return Err(ProtocolError::InvalidCursor(CursorError::BadFormat));
    }
    let nonce_bytes: [u8; NONCE_BYTES] = hex_decode(nonce_text)?
        .try_into()
        .map_err(|_| ProtocolError::InvalidCursor(CursorError::Truncated))?;
    let ciphertext = hex_decode(ciphertext_text)?;
    if ciphertext.len() < TAG_BYTES {
        return Err(ProtocolError::InvalidCursor(CursorError::Truncated));
    }
    let nonce = Nonce::from(nonce_bytes);
    let plaintext = cipher(key)?
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext.as_slice(),
                aad: CURSOR_DOMAIN,
            },
        )
        .map_err(|_| ProtocolError::InvalidCursor(CursorError::TagVerification))?;

    let limits = DecodeLimits::new(512, 4, 16, 128);
    let frame = decode_bounded(&plaintext, limits)
        .map_err(|_| ProtocolError::InvalidCursor(CursorError::Format))?;
    let mut map = fields_map(frame.into_value())?;
    // 恰好 11 个字段；多出或缺失都显式失败。
    if map.len() != 11 {
        return Err(ProtocolError::InvalidCursor(CursorError::Format));
    }
    if take_u64(&mut map, field::SCHEMA_VERSION)? != SCHEMA_VERSION {
        return Err(ProtocolError::InvalidCursor(CursorError::Format));
    }
    let account = AccountId::from_bytes(take_id::<32>(&mut map, field::CURSOR_ACCOUNT_ID)?);
    let device = DeviceId::from_bytes(take_id::<32>(&mut map, field::CURSOR_DEVICE_ID)?);
    let conversation =
        ConversationId::from_bytes(take_id::<32>(&mut map, field::CURSOR_CONVERSATION_ID)?);
    let resource = CursorResource::from_u64(take_u64(&mut map, field::CURSOR_RESOURCE)?)
        .ok_or(ProtocolError::InvalidCursor(CursorError::Format))?;
    let sort_version_raw = take_u64(&mut map, field::CURSOR_SORT_VERSION)?;
    let sort_version = u16::try_from(sort_version_raw)
        .map_err(|_| ProtocolError::InvalidCursor(CursorError::Format))?;
    let sort_key = take_bytes(&mut map, field::CURSOR_SORT_KEY)?;
    let seq = take_u64(&mut map, field::CURSOR_SEQ)?;
    let tie: [u8; CURSOR_TIE_BYTES] = take_id::<CURSOR_TIE_BYTES>(&mut map, field::CURSOR_TIE)?;
    let issued_at = take_u64(&mut map, field::CURSOR_ISSUED_AT)?;
    let expires_at = take_u64(&mut map, field::CURSOR_EXPIRES_AT)?;
    if !map.is_empty() {
        return Err(ProtocolError::InvalidCursor(CursorError::Format));
    }

    // 跨范围复用：账户/设备/会话/资源任一不符即拒绝。
    if account != expected.account
        || device != expected.device
        || conversation != expected.conversation
        || resource != expected.resource
    {
        return Err(ProtocolError::InvalidCursorScope);
    }
    // 旧排序版本必须显式要求重查。
    if sort_version != expected.sort_version {
        return Err(ProtocolError::InvalidCursor(CursorError::StaleSortVersion));
    }
    // 过期检查先于位置范围，避免把过期游标当成可用位置。
    if expires_at <= now_unix_ms {
        return Err(ProtocolError::CursorExpired);
    }
    if issued_at > now_unix_ms.saturating_add(CURSOR_MAX_CLOCK_SKEW_MS) {
        return Err(ProtocolError::InvalidCursor(CursorError::FutureTimestamp));
    }
    if expires_at > issued_at.saturating_add(CURSOR_TTL_MS) {
        return Err(ProtocolError::InvalidCursor(CursorError::TtlExceedsPolicy));
    }
    let position = CursorPosition { sort_key, seq, tie };
    validate_position(&position)?;
    Ok(position)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    fn scope() -> CursorScope {
        CursorScope {
            account: AccountId::from_bytes([1; 32]),
            device: DeviceId::from_bytes([2; 32]),
            conversation: ConversationId::from_bytes([3; 32]),
            resource: CursorResource::Messages,
            sort_version: 4,
        }
    }

    fn position() -> CursorPosition {
        CursorPosition {
            sort_key: b"2026-10-18T00:00:00Z".to_vec(),
            seq: 42,
            tie: [9; CURSOR_TIE_BYTES],
        }
    }

    fn key() -> CursorKey {
        CursorKey::from_bytes([0x5a; 32])
    }

    #[test]
    fn round_trip_and_no_plaintext_leak() {
        let token = encode_page_cursor(&scope(), &position(), &key(), 1_000_000).unwrap();
        assert!(token.as_str().starts_with("cur1."));
        assert!(!token.as_str().contains("2026-10-18"));
        let decoded = decode_page_cursor(&token, &scope(), &key(), 1_000_001).unwrap();
        assert_eq!(decoded, position());
    }

    #[test]
    fn tampered_position_and_ciphertext_are_rejected() {
        let token = encode_page_cursor(&scope(), &position(), &key(), 1_000_000).unwrap();
        let text = token.as_str().to_owned();
        // 翻转密文中一个 hex 字符。
        let mut chars: Vec<char> = text.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == '0' { '1' } else { '0' };
        let tampered = OpaqueCursor(chars.into_iter().collect());
        assert_eq!(
            decode_page_cursor(&tampered, &scope(), &key(), 1_000_001).unwrap_err(),
            ProtocolError::InvalidCursor(CursorError::TagVerification)
        );
        // 大写 hex 不是规范形式（版本前缀保持小写）。
        let (version, rest) = text.split_once('.').unwrap();
        let upper = OpaqueCursor(format!("{version}.{}", rest.to_uppercase()));
        assert_eq!(
            decode_page_cursor(&upper, &scope(), &key(), 1_000_001).unwrap_err(),
            ProtocolError::InvalidCursor(CursorError::NonCanonicalHex)
        );
    }

    #[test]
    fn cross_scope_cursors_are_rejected() {
        let token = encode_page_cursor(&scope(), &position(), &key(), 1_000_000).unwrap();
        let mut other_account = scope();
        other_account.account = AccountId::from_bytes([8; 32]);
        assert_eq!(
            decode_page_cursor(&token, &other_account, &key(), 1_000_001).unwrap_err(),
            ProtocolError::InvalidCursorScope
        );
        let mut other_device = scope();
        other_device.device = DeviceId::from_bytes([8; 32]);
        assert_eq!(
            decode_page_cursor(&token, &other_device, &key(), 1_000_001).unwrap_err(),
            ProtocolError::InvalidCursorScope
        );
        let mut other_conversation = scope();
        other_conversation.conversation = ConversationId::from_bytes([8; 32]);
        assert_eq!(
            decode_page_cursor(&token, &other_conversation, &key(), 1_000_001).unwrap_err(),
            ProtocolError::InvalidCursorScope
        );
        let mut other_resource = scope();
        other_resource.resource = CursorResource::Attachments;
        assert_eq!(
            decode_page_cursor(&token, &other_resource, &key(), 1_000_001).unwrap_err(),
            ProtocolError::InvalidCursorScope
        );
    }

    #[test]
    fn stale_sort_version_requires_requery() {
        let token = encode_page_cursor(&scope(), &position(), &key(), 1_000_000).unwrap();
        let mut newer = scope();
        newer.sort_version += 1;
        let error = decode_page_cursor(&token, &newer, &key(), 1_000_001).unwrap_err();
        assert_eq!(
            error,
            ProtocolError::InvalidCursor(CursorError::StaleSortVersion)
        );
        assert_eq!(error.code(), ErrorCode::InvalidCursor);
    }

    #[test]
    fn expired_and_truncated_tokens_fail_closed() {
        let token = encode_page_cursor(&scope(), &position(), &key(), 1_000_000).unwrap();
        assert_eq!(
            decode_page_cursor(&token, &scope(), &key(), 1_000_000 + CURSOR_TTL_MS + 1)
                .unwrap_err(),
            ProtocolError::CursorExpired
        );
        let truncated = OpaqueCursor(token.as_str()[..token.as_str().len() - 1].to_owned());
        assert!(decode_page_cursor(&truncated, &scope(), &key(), 1_000_001).is_err());
        let garbage = OpaqueCursor("cur1.zz.zz".to_owned());
        assert!(decode_page_cursor(&garbage, &scope(), &key(), 1_000_001).is_err());
        let wrong_key = CursorKey::from_bytes([0x11; 32]);
        assert_eq!(
            decode_page_cursor(&token, &scope(), &wrong_key, 1_000_001).unwrap_err(),
            ProtocolError::InvalidCursor(CursorError::TagVerification)
        );
    }

    #[test]
    fn position_bounds_are_enforced() {
        let mut empty = position();
        empty.sort_key.clear();
        assert_eq!(
            encode_page_cursor(&scope(), &empty, &key(), 1_000_000).unwrap_err(),
            ProtocolError::InvalidCursor(CursorError::EmptySortKey)
        );
        let mut long = position();
        long.sort_key = vec![0; MAX_CURSOR_SORT_KEY_BYTES + 1];
        assert!(encode_page_cursor(&scope(), &long, &key(), 1_000_000).is_err());
        let mut huge = position();
        huge.seq = MAX_CURSOR_SEQ + 1;
        assert!(encode_page_cursor(&scope(), &huge, &key(), 1_000_000).is_err());
        // 时间戳溢出。
        assert_eq!(
            encode_page_cursor(&scope(), &position(), &key(), u64::MAX).unwrap_err(),
            ProtocolError::InvalidCursor(CursorError::TimestampOverflow)
        );
        // 排序版本 0 不接受。
        let mut zero_version = scope();
        zero_version.sort_version = 0;
        assert_eq!(
            encode_page_cursor(&zero_version, &position(), &key(), 1_000_000).unwrap_err(),
            ProtocolError::InvalidCursorScope
        );
    }

    #[test]
    fn key_debug_is_redacted() {
        let rendered = format!("{:?}", key());
        assert!(!rendered.contains("5a"));
        assert!(rendered.contains("redacted"));
    }
}
