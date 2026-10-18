//! PROTOCOL-001：强类型标识符与规范文本解析。
//!
//! 冻结的文本形式为 `<prefix>:<version>:<hex>`：
//!
//! - prefix 为每种 ID 固定的小写 ASCII 前缀；
//! - version 为十进制 `1`（不接受前导零或其它版本）；
//! - hex 为固定 32 字节负载的小写十六进制（64 字符）。
//!
//! 解析先限长、再校验前缀/版本/长度，最后要求规范形式（ASCII、小写 hex、无空白）。
//! 同一负载换个类型前缀即 `WrongKind`，所以 AccountId 不能当作 DeviceId 使用。

use core::fmt;
use std::str::FromStr;

use crate::error::{IdError, ProtocolError, Result};

/// 当前冻结的 ID 文本版本。
pub const ID_VERSION: u8 = 1;
/// ID 文本形式的最大长度。前缀最长 5 字节，因此实际上限为 72。
pub const MAX_ID_TEXT_BYTES: usize = 96;
/// 所有 ID 的负载固定为 32 字节。
pub const ID_PAYLOAD_BYTES: usize = 32;

const HEX_LEN: usize = ID_PAYLOAD_BYTES * 2;

/// ID 类型。不同类型互不兼容。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IdKind {
    Account,
    Device,
    Group,
    Conversation,
    Submission,
    Event,
    Message,
    Draft,
    Operation,
    Object,
    Coordinator,
}

impl IdKind {
    /// 全部冻结类型，用于测试和枚举。
    pub const ALL: [IdKind; 11] = [
        IdKind::Account,
        IdKind::Device,
        IdKind::Group,
        IdKind::Conversation,
        IdKind::Submission,
        IdKind::Event,
        IdKind::Message,
        IdKind::Draft,
        IdKind::Operation,
        IdKind::Object,
        IdKind::Coordinator,
    ];

    /// 冻结前缀。
    pub const fn prefix(self) -> &'static str {
        match self {
            IdKind::Account => "acct",
            IdKind::Device => "dev",
            IdKind::Group => "grp",
            IdKind::Conversation => "conv",
            IdKind::Submission => "sub",
            IdKind::Event => "evt",
            IdKind::Message => "msg",
            IdKind::Draft => "draft",
            IdKind::Operation => "op",
            IdKind::Object => "obj",
            IdKind::Coordinator => "node",
        }
    }

    /// 负载字节数；当前全部类型均为 32。
    pub const fn payload_bytes(self) -> usize {
        ID_PAYLOAD_BYTES
    }

    /// 由文本前缀反向解析类型。
    pub fn from_prefix(prefix: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.prefix() == prefix)
    }
}

fn decode_hex_exact(input: &str) -> Result<[u8; ID_PAYLOAD_BYTES]> {
    let bytes = input.as_bytes();
    if bytes.len() != HEX_LEN {
        return Err(ProtocolError::InvalidId(IdError::WrongLength {
            expected: HEX_LEN,
            found: bytes.len(),
        }));
    }
    let mut out = [0u8; ID_PAYLOAD_BYTES];
    let (pairs, remainder) = bytes.as_chunks::<2>();
    debug_assert!(remainder.is_empty());
    for (index, pair) in pairs.iter().enumerate() {
        let hi = canonical_hex_digit(pair[0])?;
        let lo = canonical_hex_digit(pair[1])?;
        out[index] = (hi << 4) | lo;
    }
    Ok(out)
}

fn canonical_hex_digit(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(ProtocolError::InvalidId(IdError::NonCanonicalHex)),
    }
}

const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";

fn encode_hex(bytes: &[u8; ID_PAYLOAD_BYTES]) -> String {
    let mut text = String::with_capacity(HEX_LEN);
    for byte in bytes {
        text.push(HEX_CHARS[(byte >> 4) as usize] as char);
        text.push(HEX_CHARS[(byte & 0x0f) as usize] as char);
    }
    text
}

fn encode_text(kind: IdKind, bytes: &[u8; ID_PAYLOAD_BYTES]) -> String {
    let mut text = String::with_capacity(MAX_ID_TEXT_BYTES);
    text.push_str(kind.prefix());
    text.push(':');
    text.push_str(&ID_VERSION.to_string());
    text.push(':');
    text.push_str(&encode_hex(bytes));
    text
}

macro_rules! typed_id {
    ($name:ident, $variant:ident, $kind:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name([u8; ID_PAYLOAD_BYTES]);

        impl $name {
            /// 该类型的 ID 种类。
            pub const KIND: IdKind = IdKind::$kind;

            /// 由固定 32 字节负载构造；不检查外部文本。
            pub const fn from_bytes(bytes: [u8; ID_PAYLOAD_BYTES]) -> Self {
                Self(bytes)
            }

            /// 只读负载。
            pub const fn as_bytes(&self) -> &[u8; ID_PAYLOAD_BYTES] {
                &self.0
            }

            /// 规范文本形式。
            pub fn encode(&self) -> String {
                encode_text(Self::KIND, &self.0)
            }

            /// 按本类型解析外部文本；其它类型前缀会被拒绝。
            pub fn parse(input: &str) -> Result<Self> {
                match parse_id(Self::KIND, input)? {
                    TypedId::$variant(id) => Ok(id),
                    other => Err(ProtocolError::InvalidId(IdError::WrongKind {
                        expected: Self::KIND,
                        found: Some(other.kind()),
                    })),
                }
            }
        }

        impl core::fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.encode())
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.encode())
            }
        }

        impl FromStr for $name {
            type Err = ProtocolError;
            fn from_str(input: &str) -> Result<Self> {
                Self::parse(input)
            }
        }
    };
}

typed_id!(AccountId, Account, Account, "长期账户身份 ID。");
typed_id!(DeviceId, Device, Device, "账户签发的设备身份 ID。");
typed_id!(GroupId, Group, Group, "协议群 ID。");
typed_id!(ConversationId, Conversation, Conversation, "UI 会话 ID。");
typed_id!(SubmissionId, Submission, Submission, "固定密文提交 ID。");
typed_id!(EventId, Event, Event, "MLS 内层认证事件 ID。");
typed_id!(MessageId, Message, Message, "正式消息 ID。");
typed_id!(DraftId, Draft, Draft, "本地发送意图 ID。");
typed_id!(OperationId, Operation, Operation, "业务幂等操作 ID。");
typed_id!(ObjectId, Object, Object, "插件内容对象 ID。");
typed_id!(CoordinatorId, Coordinator, Coordinator, "固定协调节点 ID。");

/// 已解析的强类型 ID。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TypedId {
    Account(AccountId),
    Device(DeviceId),
    Group(GroupId),
    Conversation(ConversationId),
    Submission(SubmissionId),
    Event(EventId),
    Message(MessageId),
    Draft(DraftId),
    Operation(OperationId),
    Object(ObjectId),
    Coordinator(CoordinatorId),
}

impl TypedId {
    /// 实际类型。
    pub const fn kind(self) -> IdKind {
        match self {
            TypedId::Account(_) => IdKind::Account,
            TypedId::Device(_) => IdKind::Device,
            TypedId::Group(_) => IdKind::Group,
            TypedId::Conversation(_) => IdKind::Conversation,
            TypedId::Submission(_) => IdKind::Submission,
            TypedId::Event(_) => IdKind::Event,
            TypedId::Message(_) => IdKind::Message,
            TypedId::Draft(_) => IdKind::Draft,
            TypedId::Operation(_) => IdKind::Operation,
            TypedId::Object(_) => IdKind::Object,
            TypedId::Coordinator(_) => IdKind::Coordinator,
        }
    }

    /// 类型无关的 32 字节负载。
    pub const fn as_bytes(&self) -> &[u8; ID_PAYLOAD_BYTES] {
        match self {
            TypedId::Account(id) => id.as_bytes(),
            TypedId::Device(id) => id.as_bytes(),
            TypedId::Group(id) => id.as_bytes(),
            TypedId::Conversation(id) => id.as_bytes(),
            TypedId::Submission(id) => id.as_bytes(),
            TypedId::Event(id) => id.as_bytes(),
            TypedId::Message(id) => id.as_bytes(),
            TypedId::Draft(id) => id.as_bytes(),
            TypedId::Operation(id) => id.as_bytes(),
            TypedId::Object(id) => id.as_bytes(),
            TypedId::Coordinator(id) => id.as_bytes(),
        }
    }

    /// 规范文本形式。
    pub fn encode(&self) -> String {
        encode_text(self.kind(), self.as_bytes())
    }
}

/// PROTOCOL-001：解析外部 ID 文本。
///
/// 处理顺序：先限长（含空串），再检查 ASCII 与 `prefix:version:hex` 结构，
/// 最后要求前缀与请求类型一致、版本为 1、hex 长度与字符均规范。
/// 非法输入绝不改写为合法值。
pub fn parse_id(kind: IdKind, input: &str) -> Result<TypedId> {
    if input.is_empty() {
        return Err(ProtocolError::InvalidId(IdError::Empty));
    }
    if input.len() > MAX_ID_TEXT_BYTES {
        return Err(ProtocolError::InvalidId(IdError::TooLong {
            limit: MAX_ID_TEXT_BYTES,
        }));
    }
    if !input.is_ascii() {
        return Err(ProtocolError::InvalidId(IdError::NonAscii));
    }
    let mut parts = input.split(':');
    let (Some(prefix), Some(version), Some(payload), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(ProtocolError::InvalidId(IdError::Malformed));
    };
    let found = IdKind::from_prefix(prefix);
    let Some(found_kind) = found else {
        return Err(ProtocolError::InvalidId(IdError::WrongKind {
            expected: kind,
            found: None,
        }));
    };
    if found_kind != kind {
        return Err(ProtocolError::InvalidId(IdError::WrongKind {
            expected: kind,
            found: Some(found_kind),
        }));
    }
    if version != "1" {
        return Err(ProtocolError::InvalidId(IdError::UnsupportedVersion));
    }
    if payload.len() != HEX_LEN {
        return Err(ProtocolError::InvalidId(IdError::WrongLength {
            expected: HEX_LEN,
            found: payload.len(),
        }));
    }
    let bytes = decode_hex_exact(payload)?;
    Ok(match kind {
        IdKind::Account => TypedId::Account(AccountId::from_bytes(bytes)),
        IdKind::Device => TypedId::Device(DeviceId::from_bytes(bytes)),
        IdKind::Group => TypedId::Group(GroupId::from_bytes(bytes)),
        IdKind::Conversation => TypedId::Conversation(ConversationId::from_bytes(bytes)),
        IdKind::Submission => TypedId::Submission(SubmissionId::from_bytes(bytes)),
        IdKind::Event => TypedId::Event(EventId::from_bytes(bytes)),
        IdKind::Message => TypedId::Message(MessageId::from_bytes(bytes)),
        IdKind::Draft => TypedId::Draft(DraftId::from_bytes(bytes)),
        IdKind::Operation => TypedId::Operation(OperationId::from_bytes(bytes)),
        IdKind::Object => TypedId::Object(ObjectId::from_bytes(bytes)),
        IdKind::Coordinator => TypedId::Coordinator(CoordinatorId::from_bytes(bytes)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_round_trip_for_every_kind() {
        let bytes = [0x5a; ID_PAYLOAD_BYTES];
        for kind in IdKind::ALL {
            let text = match kind {
                IdKind::Account => AccountId::from_bytes(bytes).encode(),
                IdKind::Device => DeviceId::from_bytes(bytes).encode(),
                IdKind::Group => GroupId::from_bytes(bytes).encode(),
                IdKind::Conversation => ConversationId::from_bytes(bytes).encode(),
                IdKind::Submission => SubmissionId::from_bytes(bytes).encode(),
                IdKind::Event => EventId::from_bytes(bytes).encode(),
                IdKind::Message => MessageId::from_bytes(bytes).encode(),
                IdKind::Draft => DraftId::from_bytes(bytes).encode(),
                IdKind::Operation => OperationId::from_bytes(bytes).encode(),
                IdKind::Object => ObjectId::from_bytes(bytes).encode(),
                IdKind::Coordinator => CoordinatorId::from_bytes(bytes).encode(),
            };
            let parsed = parse_id(kind, &text).expect("canonical id must parse");
            assert_eq!(parsed.kind(), kind);
            assert_eq!(parsed.as_bytes(), &bytes);
        }
    }

    #[test]
    fn account_text_is_not_a_device_id() {
        let text = AccountId::from_bytes([7; ID_PAYLOAD_BYTES]).encode();
        assert!(DeviceId::parse(&text).is_err());
        assert_eq!(
            parse_id(IdKind::Device, &text).unwrap_err().code(),
            crate::error::ErrorCode::InvalidId
        );
    }

    #[test]
    fn rejects_non_canonical_encodings() {
        let good = AccountId::from_bytes([0x0a; ID_PAYLOAD_BYTES]).encode();
        let upper = good.to_uppercase();
        assert!(
            AccountId::parse(&upper).is_err(),
            "uppercase hex is not canonical"
        );
        let no_version = good.replacen(":1:", ":", 1);
        assert!(AccountId::parse(&no_version).is_err());
        let leading_zero_version = good.replacen(":1:", ":01:", 1);
        assert!(AccountId::parse(&leading_zero_version).is_err());
        let trailing = format!("{good}0");
        assert!(AccountId::parse(&trailing).is_err());
        let padded = format!(" {good}");
        assert!(AccountId::parse(&padded).is_err());
        assert!(AccountId::parse("").is_err());
        assert!(AccountId::parse(&"a".repeat(MAX_ID_TEXT_BYTES + 1)).is_err());
        assert!(AccountId::parse(&format!("acct:1:{}", "g".repeat(HEX_LEN))).is_err());
    }
}
