//! PROTOCOL-002：与语言无关的确定性规范编码。
//!
//! 冻结的线格式：
//!
//! ```text
//! "CPC" major:u8 minor:u8 body
//! ```
//!
//! body 的每个值以 1 字节 tag 开头：
//!
//! - 0x01 u64：8 字节大端；
//! - 0x02 bytes：u32 大端长度 + 原始字节；
//! - 0x03 text：u32 大端 UTF-8 长度 + 原始字节；
//! - 0x04 none：无负载（可选值缺失）；
//! - 0x05 some：紧跟一个完整值（可选值存在）；
//! - 0x06 list：u32 大端元素数 + 依次的元素；
//! - 0x07 struct：u16 大端字段数 + 每项 (u16 大端字段号 + 值)，字段号严格升序。
//!
//! 因此同一结构无论调用方以什么字段顺序传入，输出字节都相同；重复字段号和
//! 未知安全字段号在编码期就被拒绝。

use core::fmt;

use crate::digest::Digest;
use crate::error::{ErrorCode, ProtocolError, Result};

/// 规范编码魔数。
pub const CANONICAL_MAGIC: &[u8; 3] = b"CPC";
/// 规范编码总长上限。
pub const MAX_CANONICAL_BYTES: usize = 64 * 1024;
/// 嵌套深度上限。
pub const MAX_CANONICAL_DEPTH: usize = 8;
/// 单集合元素数上限。
pub const MAX_COLLECTION_ITEMS: usize = 512;
/// 单文本上限（UTF-8 字节）。
pub const MAX_CANONICAL_TEXT_BYTES: usize = 8 * 1024;

const TAG_U64: u8 = 0x01;
const TAG_BYTES: u8 = 0x02;
const TAG_TEXT: u8 = 0x03;
const TAG_NONE: u8 = 0x04;
const TAG_SOME: u8 = 0x05;
const TAG_LIST: u8 = 0x06;
const TAG_STRUCT: u8 = 0x07;

/// 冻结的线协议版本。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProtocolVersion {
    /// 主版本。
    pub major: u8,
    /// 次版本。
    pub minor: u8,
}

impl ProtocolVersion {
    /// 当前唯一支持的版本：1.0。
    pub const V1: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };

    /// 是否受支持；不支持时必须显式失败。
    pub const fn is_supported(self) -> bool {
        self.major == 1 && self.minor == 0
    }
}

/// 冻结字段号。新增字段必须走版本流程，不能临时占用编号。
pub mod field {
    pub const SCHEMA_VERSION: u16 = 1;
    pub const PROTOCOL_MAJOR: u16 = 2;
    pub const PROTOCOL_MINOR: u16 = 3;
    pub const SUBMISSION_ID: u16 = 4;
    pub const GROUP_ID: u16 = 5;
    pub const AUTHOR_DEVICE_ID: u16 = 6;
    pub const KIND: u16 = 7;
    pub const CONTROL_HEAD: u16 = 8;
    pub const POLICY_VERSION: u16 = 9;
    pub const MLS_EPOCH: u16 = 10;
    pub const PAYLOAD_DIGEST: u16 = 11;
    pub const CONTROL_OPERATION: u16 = 12;
    pub const CONTROL_TARGET_DIGEST: u16 = 13;
    pub const RECEIPT_SEQ: u16 = 14;
    pub const PREVIOUS_LOG_HASH: u16 = 15;
    pub const CANDIDATE_DIGEST: u16 = 16;
    pub const CURSOR_ACCOUNT_ID: u16 = 17;
    pub const CURSOR_DEVICE_ID: u16 = 18;
    pub const CURSOR_CONVERSATION_ID: u16 = 19;
    pub const CURSOR_RESOURCE: u16 = 20;
    pub const CURSOR_SORT_VERSION: u16 = 21;
    pub const CURSOR_SORT_KEY: u16 = 22;
    pub const CURSOR_SEQ: u16 = 23;
    pub const CURSOR_TIE: u16 = 24;
    pub const CURSOR_ISSUED_AT: u16 = 25;
    pub const CURSOR_EXPIRES_AT: u16 = 26;
    pub const EXTENSION_NAMESPACE: u16 = 27;
    pub const EXTENSION_SCHEMA_VERSION: u16 = 28;
    pub const EXTENSION_OPERATION_ID: u16 = 29;
    pub const EXTENSION_OBJECT_ID: u16 = 30;
    pub const EXTENSION_PAYLOAD: u16 = 31;
    pub const EXTENSION_FALLBACK: u16 = 32;
    // U 包新增（P1-01 补充，字段号只增不改）：日志条目、应用事件、策略操作与会话能力。
    pub const LOG_COORDINATOR_SIGNATURE: u16 = 33;
    pub const LOG_ENTRY_HASH: u16 = 34;
    pub const EVENT_ID: u16 = 35;
    pub const AUTHOR_ACCOUNT_ID: u16 = 36;
    pub const CONTENT_KIND: u16 = 37;
    pub const CONTENT_TEXT: u16 = 38;
    pub const CONTENT_EXTENSION: u16 = 39;
    pub const CONTENT_ATTACHMENT_FILE_ID: u16 = 40;
    pub const REPLY_CONVERSATION_ID: u16 = 41;
    pub const REPLY_MESSAGE_ID: u16 = 42;
    pub const MESSAGE_ORIGIN: u16 = 43;
    pub const EVENT_CONVERSATION_ID: u16 = 44;
    pub const EVENT_SENT_AT: u16 = 45;
    pub const POLICY_OPERATION_ACCOUNT: u16 = 46;
    pub const POLICY_OPERATION_ROLE: u16 = 47;
    pub const POLICY_OPERATION_MUTED: u16 = 48;
    pub const POLICY_OPERATION_DEVICES: u16 = 49;
    pub const POLICY_OPERATION_NAME_DIGEST: u16 = 50;
    pub const SESSION_FEATURES: u16 = 51;
    pub const SESSION_MAX_FRAME_BYTES: u16 = 52;
    pub const SESSION_MAX_MESSAGE_BYTES: u16 = 53;
    pub const SESSION_MAX_CIPHERTEXT_BYTES: u16 = 54;
    // P1 补充（字段号只增不改）：控制候选的可选领域操作声明。
    pub const CONTROL_DOMAIN_OPERATION: u16 = 55;
    pub const CONTROL_DOMAIN_TARGET_DIGEST: u16 = 56;
    // 附件引用可选 metadata（密钥描述符等，≤512 字节）。
    pub const CONTENT_ATTACHMENT_METADATA: u16 = 57;
    // 控制提交携带的成员凭据（UTF-8 列表，单个 ≤128B、总数 ≤64）。
    pub const CONTROL_ADDED_CREDENTIALS: u16 = 59;
    pub const CONTROL_REMOVED_CREDENTIALS: u16 = 60;
    // 控制提交携带的撤销设备 ID 列表（UTF-8，单个 ≤128B、总数 ≤64，仅 RevokeDevice）。
    pub const CONTROL_REVOKED_DEVICES: u16 = 61;
}

/// 已知安全字段号表。编码/解码都拒绝表外字段号。
pub const KNOWN_FIELD_IDS: &[u16] = &[
    field::SCHEMA_VERSION,
    field::PROTOCOL_MAJOR,
    field::PROTOCOL_MINOR,
    field::SUBMISSION_ID,
    field::GROUP_ID,
    field::AUTHOR_DEVICE_ID,
    field::KIND,
    field::CONTROL_HEAD,
    field::POLICY_VERSION,
    field::MLS_EPOCH,
    field::PAYLOAD_DIGEST,
    field::CONTROL_OPERATION,
    field::CONTROL_TARGET_DIGEST,
    field::RECEIPT_SEQ,
    field::PREVIOUS_LOG_HASH,
    field::CANDIDATE_DIGEST,
    field::CURSOR_ACCOUNT_ID,
    field::CURSOR_DEVICE_ID,
    field::CURSOR_CONVERSATION_ID,
    field::CURSOR_RESOURCE,
    field::CURSOR_SORT_VERSION,
    field::CURSOR_SORT_KEY,
    field::CURSOR_SEQ,
    field::CURSOR_TIE,
    field::CURSOR_ISSUED_AT,
    field::CURSOR_EXPIRES_AT,
    field::EXTENSION_NAMESPACE,
    field::EXTENSION_SCHEMA_VERSION,
    field::EXTENSION_OPERATION_ID,
    field::EXTENSION_OBJECT_ID,
    field::EXTENSION_PAYLOAD,
    field::EXTENSION_FALLBACK,
    field::LOG_COORDINATOR_SIGNATURE,
    field::LOG_ENTRY_HASH,
    field::EVENT_ID,
    field::AUTHOR_ACCOUNT_ID,
    field::CONTENT_KIND,
    field::CONTENT_TEXT,
    field::CONTENT_EXTENSION,
    field::CONTENT_ATTACHMENT_FILE_ID,
    field::REPLY_CONVERSATION_ID,
    field::REPLY_MESSAGE_ID,
    field::MESSAGE_ORIGIN,
    field::EVENT_CONVERSATION_ID,
    field::EVENT_SENT_AT,
    field::POLICY_OPERATION_ACCOUNT,
    field::POLICY_OPERATION_ROLE,
    field::POLICY_OPERATION_MUTED,
    field::POLICY_OPERATION_DEVICES,
    field::POLICY_OPERATION_NAME_DIGEST,
    field::SESSION_FEATURES,
    field::SESSION_MAX_FRAME_BYTES,
    field::SESSION_MAX_MESSAGE_BYTES,
    field::SESSION_MAX_CIPHERTEXT_BYTES,
    field::CONTROL_DOMAIN_OPERATION,
    field::CONTROL_DOMAIN_TARGET_DIGEST,
    field::CONTENT_ATTACHMENT_METADATA,
    field::CONTROL_ADDED_CREDENTIALS,
    field::CONTROL_REMOVED_CREDENTIALS,
    field::CONTROL_REVOKED_DEVICES,
];

/// 判断字段号是否属于冻结安全字段集合。
pub fn is_known_field(id: u16) -> bool {
    KNOWN_FIELD_IDS.contains(&id)
}

/// 结构字段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireField {
    /// 冻结字段号。
    pub id: u16,
    /// 字段值。
    pub value: WireValue,
}

impl WireField {
    /// 构造字段。
    pub const fn new(id: u16, value: WireValue) -> Self {
        Self { id, value }
    }
}

/// 规范编码的抽象值。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireValue {
    /// 大端 u64。
    U64(u64),
    /// 有界字节串。
    Bytes(Vec<u8>),
    /// 有界 UTF-8 文本。
    Text(String),
    /// 可选值缺失。
    None,
    /// 可选值存在。
    Some(Box<WireValue>),
    /// 有序集合。
    List(Vec<WireValue>),
    /// 结构体；编码时按字段号升序重排。
    Struct(Vec<WireField>),
}

impl WireValue {
    /// 由字段迭代器构造结构体。
    pub fn structure(fields: impl IntoIterator<Item = WireField>) -> Self {
        WireValue::Struct(fields.into_iter().collect())
    }

    /// 可选值。
    pub fn optional(value: Option<WireValue>) -> Self {
        match value {
            Some(inner) => WireValue::Some(Box::new(inner)),
            None => WireValue::None,
        }
    }

    /// 32 字节摘要值。
    pub fn digest(value: Digest) -> Self {
        WireValue::Bytes(value.as_bytes().to_vec())
    }

    /// 32 字节标识负载。
    pub fn id_bytes(bytes: &[u8; 32]) -> Self {
        WireValue::Bytes(bytes.to_vec())
    }
}

/// 规范编码后的不可变字节。
#[derive(Clone, PartialEq, Eq)]
pub struct CanonicalBytes(Vec<u8>);

impl CanonicalBytes {
    /// 只读字节。
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// 长度。
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 取出底层字节。
    pub fn into_vec(self) -> Vec<u8> {
        self.0
    }

    /// 由已经包含域分隔的原始字节构造（仅 crate 内部签名原文使用）。
    pub(crate) fn from_raw(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// 小写十六进制，用于固定测试向量或调试。
    pub fn to_hex(&self) -> String {
        let mut text = String::with_capacity(self.0.len() * 2);
        for byte in &self.0 {
            text.push(char::from(HEX[(byte >> 4) as usize]));
            text.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
        text
    }
}

impl fmt::Debug for CanonicalBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "CanonicalBytes({})", self.to_hex())
    }
}

const HEX: [u8; 16] = *b"0123456789abcdef";

struct Writer {
    buffer: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(256),
        }
    }

    fn ensure_capacity(&self, extra: usize) -> Result<()> {
        let total = self
            .buffer
            .len()
            .checked_add(extra)
            .ok_or(ProtocolError::ContentTooLarge {
                limit: MAX_CANONICAL_BYTES,
            })?;
        if total > MAX_CANONICAL_BYTES {
            return Err(ProtocolError::ContentTooLarge {
                limit: MAX_CANONICAL_BYTES,
            });
        }
        Ok(())
    }

    fn push(&mut self, byte: u8) {
        self.buffer.push(byte);
    }

    fn push_slice(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    fn write_value(&mut self, value: &WireValue, depth: usize) -> Result<()> {
        if depth > MAX_CANONICAL_DEPTH {
            return Err(ProtocolError::TooDeep {
                limit: MAX_CANONICAL_DEPTH,
            });
        }
        self.ensure_capacity(1)?;
        match value {
            WireValue::U64(number) => {
                self.push(TAG_U64);
                self.ensure_capacity(8)?;
                self.push_slice(&number.to_be_bytes());
            }
            WireValue::Bytes(bytes) => {
                self.push(TAG_BYTES);
                self.write_length(bytes.len())?;
                self.ensure_capacity(bytes.len())?;
                self.push_slice(bytes);
            }
            WireValue::Text(text) => {
                if text.len() > MAX_CANONICAL_TEXT_BYTES {
                    return Err(ProtocolError::ContentTooLarge {
                        limit: MAX_CANONICAL_TEXT_BYTES,
                    });
                }
                self.push(TAG_TEXT);
                self.write_length(text.len())?;
                self.ensure_capacity(text.len())?;
                self.push_slice(text.as_bytes());
            }
            WireValue::None => self.push(TAG_NONE),
            WireValue::Some(inner) => {
                self.push(TAG_SOME);
                self.write_value(inner, depth + 1)?;
            }
            WireValue::List(items) => {
                if items.len() > MAX_COLLECTION_ITEMS {
                    return Err(ProtocolError::ContentTooLarge {
                        limit: MAX_COLLECTION_ITEMS,
                    });
                }
                self.push(TAG_LIST);
                let count =
                    u32::try_from(items.len()).map_err(|_| ProtocolError::ContentTooLarge {
                        limit: MAX_COLLECTION_ITEMS,
                    })?;
                self.ensure_capacity(4)?;
                self.push_slice(&count.to_be_bytes());
                for item in items {
                    self.write_value(item, depth + 1)?;
                }
            }
            WireValue::Struct(fields) => {
                if fields.len() > MAX_COLLECTION_ITEMS {
                    return Err(ProtocolError::ContentTooLarge {
                        limit: MAX_COLLECTION_ITEMS,
                    });
                }
                // 编码前先验证字段号：未知、零与重复都显式失败。
                let mut ids: Vec<u16> = fields.iter().map(|field| field.id).collect();
                for id in &ids {
                    if !is_known_field(*id) {
                        return Err(ProtocolError::UnknownSafetyField(*id));
                    }
                }
                ids.sort_unstable();
                if let Some(duplicate) = ids.windows(2).find(|pair| pair[0] == pair[1]) {
                    return Err(ProtocolError::DuplicateSafetyField(duplicate[0]));
                }
                // 按字段号重排，保证输入顺序不影响字节。
                let mut ordered: Vec<&WireField> = fields.iter().collect();
                ordered.sort_by_key(|field| field.id);
                self.push(TAG_STRUCT);
                let count =
                    u16::try_from(ordered.len()).map_err(|_| ProtocolError::ContentTooLarge {
                        limit: MAX_COLLECTION_ITEMS,
                    })?;
                self.ensure_capacity(2)?;
                self.push_slice(&count.to_be_bytes());
                for field in ordered {
                    self.ensure_capacity(2)?;
                    self.push_slice(&field.id.to_be_bytes());
                    self.write_value(&field.value, depth + 1)?;
                }
            }
        }
        Ok(())
    }

    fn write_length(&mut self, length: usize) -> Result<()> {
        let length = u32::try_from(length).map_err(|_| ProtocolError::ContentTooLarge {
            limit: MAX_CANONICAL_BYTES,
        })?;
        self.ensure_capacity(4)?;
        self.push_slice(&length.to_be_bytes());
        Ok(())
    }
}

/// PROTOCOL-002：规范编码。
///
/// 版本不受支持返回 `PROTOCOL_SUPPORTED` 对应的 `ProtocolError`；超限返回
/// `CONTENT_TOO_LARGE`；未知/重复安全字段返回 `PROTOCOL_UNSUPPORTED`。
/// 输出不含签名自身，也不允许直接对任意 JSON 字符串签名。
pub fn encode_canonical(value: &WireValue, version: ProtocolVersion) -> Result<CanonicalBytes> {
    if !version.is_supported() {
        return Err(ProtocolError::UnsupportedProtocolVersion);
    }
    let mut writer = Writer::new();
    writer.push_slice(CANONICAL_MAGIC);
    writer.push(version.major);
    writer.push(version.minor);
    writer.write_value(value, 0)?;
    Ok(CanonicalBytes(writer.buffer))
}

/// 编码后字节的稳定错误码，便于调用方断言。
pub fn canonical_error_code(error: &ProtocolError) -> ErrorCode {
    error.code()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> WireValue {
        WireValue::structure([
            WireField::new(field::GROUP_ID, WireValue::id_bytes(&[9; 32])),
            WireField::new(field::POLICY_VERSION, WireValue::U64(7)),
            WireField::new(field::SCHEMA_VERSION, WireValue::U64(1)),
        ])
    }

    #[test]
    fn field_order_does_not_change_bytes() {
        let forward = encode_canonical(&sample(), ProtocolVersion::V1).unwrap();
        let mut fields = vec![
            WireField::new(field::POLICY_VERSION, WireValue::U64(7)),
            WireField::new(field::SCHEMA_VERSION, WireValue::U64(1)),
            WireField::new(field::GROUP_ID, WireValue::id_bytes(&[9; 32])),
        ];
        fields.reverse();
        let reversed = encode_canonical(&WireValue::Struct(fields), ProtocolVersion::V1).unwrap();
        assert_eq!(forward.as_bytes(), reversed.as_bytes());
    }

    #[test]
    fn integer_encoding_is_fixed_width_big_endian() {
        let value =
            WireValue::structure([WireField::new(field::POLICY_VERSION, WireValue::U64(1))]);
        let bytes = encode_canonical(&value, ProtocolVersion::V1).unwrap();
        // "CPC" 1 0 | tag=0x07 count=0x0001 | id=0x0009 | tag=0x01 | 8 字节大端
        let expected = [
            b'C', b'P', b'C', 1, 0, 0x07, 0x00, 0x01, 0x00, 0x09, 0x01, 0, 0, 0, 0, 0, 0, 0, 1,
        ];
        assert_eq!(bytes.as_bytes(), expected);
        // 边界值。
        for number in [0u64, 1, u64::MAX] {
            let value = WireValue::structure([WireField::new(
                field::POLICY_VERSION,
                WireValue::U64(number),
            )]);
            let encoded = encode_canonical(&value, ProtocolVersion::V1).unwrap();
            let tail = &encoded.as_bytes()[encoded.len() - 8..];
            assert_eq!(tail, number.to_be_bytes());
        }
    }

    #[test]
    fn rejects_unsupported_version_duplicate_and_unknown_fields() {
        assert_eq!(
            encode_canonical(&WireValue::U64(1), ProtocolVersion { major: 2, minor: 0 })
                .unwrap_err(),
            ProtocolError::UnsupportedProtocolVersion
        );
        let duplicate = WireValue::structure([
            WireField::new(field::POLICY_VERSION, WireValue::U64(1)),
            WireField::new(field::POLICY_VERSION, WireValue::U64(2)),
        ]);
        assert_eq!(
            encode_canonical(&duplicate, ProtocolVersion::V1).unwrap_err(),
            ProtocolError::DuplicateSafetyField(field::POLICY_VERSION)
        );
        let unknown = WireValue::structure([WireField::new(999, WireValue::U64(1))]);
        assert_eq!(
            encode_canonical(&unknown, ProtocolVersion::V1).unwrap_err(),
            ProtocolError::UnknownSafetyField(999)
        );
    }

    #[test]
    fn rejects_oversized_payloads() {
        let big = WireValue::Bytes(vec![0; MAX_CANONICAL_BYTES]);
        assert_eq!(
            encode_canonical(&big, ProtocolVersion::V1)
                .unwrap_err()
                .code(),
            ErrorCode::ContentTooLarge
        );
        let many = WireValue::List(vec![WireValue::U64(1); MAX_COLLECTION_ITEMS + 1]);
        assert_eq!(
            encode_canonical(&many, ProtocolVersion::V1)
                .unwrap_err()
                .code(),
            ErrorCode::ContentTooLarge
        );
        let long_text = WireValue::Text("a".repeat(MAX_CANONICAL_TEXT_BYTES + 1));
        assert_eq!(
            encode_canonical(&long_text, ProtocolVersion::V1)
                .unwrap_err()
                .code(),
            ErrorCode::ContentTooLarge
        );
    }

    #[test]
    fn optional_values_have_explicit_representation() {
        let absent = WireValue::structure([WireField::new(
            field::CONTROL_OPERATION,
            WireValue::optional(None),
        )]);
        let bytes = encode_canonical(&absent, ProtocolVersion::V1).unwrap();
        assert!(bytes.as_bytes().ends_with(&[0x00, 0x0c, TAG_NONE]));
        let present = WireValue::structure([WireField::new(
            field::CONTROL_OPERATION,
            WireValue::optional(Some(WireValue::U64(2))),
        )]);
        let bytes = encode_canonical(&present, ProtocolVersion::V1).unwrap();
        assert!(
            bytes
                .as_bytes()
                .ends_with(&[0x00, 0x0c, TAG_SOME, TAG_U64, 0, 0, 0, 0, 0, 0, 0, 2])
        );
    }
}
