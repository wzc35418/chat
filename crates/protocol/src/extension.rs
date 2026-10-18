//! PROTOCOL-010：插件扩展内容校验。
//!
//! 扩展内容永远不是权限来源：未知命名空间的 payload 被保留并可显示 fallback，
//! 但核心/系统命名空间被拒绝——未知控制操作必须走 MLS 控制事件并停止该群分支，
//! 不能借扩展内容修改核心权限。

use core::fmt;

use crate::canonical::{ProtocolVersion, WireField, WireValue, encode_canonical, field};
use crate::error::{ProtocolError, Result};
use crate::ids::{ObjectId, OperationId};

/// 扩展文本字段上限：命名空间 64 字节、回退文本 1024 字节。
pub const MAX_NAMESPACE_BYTES: usize = 64;
/// 回退文本字节上限。
pub const MAX_FALLBACK_BYTES: usize = 1024;
/// 单个扩展 payload 的硬上限。
pub const MAX_EXTENSION_PAYLOAD_BYTES: usize = 32 * 1024;

/// 内容预算。完整消息仍受 `max_message_bytes` 约束。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentLimits {
    /// 完整规范消息字节上限。
    pub max_message_bytes: usize,
    /// 扩展 payload 字节上限。
    pub max_payload_bytes: usize,
    /// 回退文本字节上限。
    pub max_fallback_bytes: usize,
}

impl ContentLimits {
    /// 显式构造预算。
    pub const fn new(
        max_message_bytes: usize,
        max_payload_bytes: usize,
        max_fallback_bytes: usize,
    ) -> Self {
        Self {
            max_message_bytes,
            max_payload_bytes,
            max_fallback_bytes,
        }
    }

    /// 冻结默认预算：消息 64 KiB、payload 32 KiB、回退 1 KiB。
    pub const DEFAULT: ContentLimits = ContentLimits {
        max_message_bytes: 64 * 1024,
        max_payload_bytes: MAX_EXTENSION_PAYLOAD_BYTES,
        max_fallback_bytes: MAX_FALLBACK_BYTES,
    };
}

impl Default for ContentLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// 校验失败原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionError {
    EmptyNamespace,
    NamespaceTooLong { limit: usize },
    NamespaceNotCanonical,
    ReservedNamespace,
    ZeroSchemaVersion,
    IdNotCanonical,
    PayloadTooLarge { limit: usize },
    EmptyFallback,
    FallbackTooLong { limit: usize },
    FallbackControlCharacter,
}

impl fmt::Display for ExtensionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExtensionError::EmptyNamespace => formatter.write_str("namespace is empty"),
            ExtensionError::NamespaceTooLong { limit } => {
                write!(formatter, "namespace exceeds {limit} bytes")
            }
            ExtensionError::NamespaceNotCanonical => {
                formatter.write_str("namespace is not canonical")
            }
            ExtensionError::ReservedNamespace => formatter.write_str("namespace is reserved"),
            ExtensionError::ZeroSchemaVersion => formatter.write_str("schemaVersion must be >= 1"),
            ExtensionError::IdNotCanonical => {
                formatter.write_str("operation/object id is not canonical")
            }
            ExtensionError::PayloadTooLarge { limit } => {
                write!(formatter, "payload exceeds {limit} bytes")
            }
            ExtensionError::EmptyFallback => formatter.write_str("fallback text is empty"),
            ExtensionError::FallbackTooLong { limit } => {
                write!(formatter, "fallback text exceeds {limit} bytes")
            }
            ExtensionError::FallbackControlCharacter => {
                formatter.write_str("fallback text contains control characters")
            }
        }
    }
}

/// 原始扩展内容。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionContent {
    /// 小写点分命名空间，例如 `notes.v1`。
    pub namespace: String,
    /// 内容 schema 版本，必须 ≥ 1。
    pub schema_version: u16,
    /// 幂等操作 ID。
    pub operation_id: OperationId,
    /// 内容对象 ID。
    pub object_id: ObjectId,
    /// 未知内容被保留的原始 payload。
    pub payload: Vec<u8>,
    /// 未安装插件时可显示的纯文本回退，不得为空。
    pub fallback_text: String,
}

/// 通过结构校验的扩展内容。字段齐全，未知 payload 原样保留。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedExtension {
    /// 规范化后的命名空间。
    pub namespace: String,
    /// schema 版本。
    pub schema_version: u16,
    /// 幂等操作 ID。
    pub operation_id: OperationId,
    /// 对象 ID。
    pub object_id: ObjectId,
    /// 保留的 payload。
    pub payload: Vec<u8>,
    /// 回退文本。
    pub fallback_text: String,
}

/// 保留给核心与系统控制逻辑的命名空间前缀。
const RESERVED_NAMESPACES: [&str; 2] = ["core", "sys"];

fn is_canonical_namespace(namespace: &str) -> bool {
    // 1..=4 段，每段 1..=32 字节，仅 [a-z0-9-]，首字符小写字母或数字，末字符非 '-'。
    let segments: Vec<&str> = namespace.split('.').collect();
    if segments.is_empty() || segments.len() > 4 {
        return false;
    }
    segments.iter().all(|segment| {
        let bytes = segment.as_bytes();
        if bytes.is_empty() || bytes.len() > 32 {
            return false;
        }
        if !bytes
            .iter()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-'))
        {
            return false;
        }
        if bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
            return false;
        }
        !segment.is_empty()
    })
}

/// PROTOCOL-010：校验扩展内容。
///
/// 顺序：命名空间结构（非空/长度/规范形式/非保留）→ schemaVersion → operation/object
/// ID 规范形式 → payload 与回退文本上限 → 完整规范消息上限。
/// 未知命名空间（非保留）允许通过并保留 payload，供未安装插件显示 fallback。
pub fn validate_extension(
    content: ExtensionContent,
    limits: ContentLimits,
) -> Result<ValidatedExtension> {
    if content.namespace.is_empty() {
        return Err(ProtocolError::InvalidExtension(
            ExtensionError::EmptyNamespace,
        ));
    }
    if content.namespace.len() > MAX_NAMESPACE_BYTES {
        return Err(ProtocolError::InvalidExtension(
            ExtensionError::NamespaceTooLong {
                limit: MAX_NAMESPACE_BYTES,
            },
        ));
    }
    if !is_canonical_namespace(&content.namespace) {
        return Err(ProtocolError::InvalidExtension(
            ExtensionError::NamespaceNotCanonical,
        ));
    }
    let first_segment = content
        .namespace
        .split('.')
        .next()
        .unwrap_or(&content.namespace);
    if RESERVED_NAMESPACES.contains(&first_segment) {
        return Err(ProtocolError::InvalidExtension(
            ExtensionError::ReservedNamespace,
        ));
    }
    if content.schema_version == 0 {
        return Err(ProtocolError::InvalidExtension(
            ExtensionError::ZeroSchemaVersion,
        ));
    }
    // 类型已保证是强类型 ID；这里再做一次规范往返，避免绕过 parse_id 构造的非法值。
    if content
        .operation_id
        .encode()
        .parse::<OperationId>()
        .is_err()
        || content.object_id.encode().parse::<ObjectId>().is_err()
    {
        return Err(ProtocolError::InvalidExtension(
            ExtensionError::IdNotCanonical,
        ));
    }
    if content.payload.len() > limits.max_payload_bytes {
        return Err(ProtocolError::InvalidExtension(
            ExtensionError::PayloadTooLarge {
                limit: limits.max_payload_bytes,
            },
        ));
    }
    if content.fallback_text.trim().is_empty() {
        return Err(ProtocolError::InvalidExtension(
            ExtensionError::EmptyFallback,
        ));
    }
    if content.fallback_text.len() > limits.max_fallback_bytes {
        return Err(ProtocolError::InvalidExtension(
            ExtensionError::FallbackTooLong {
                limit: limits.max_fallback_bytes,
            },
        ));
    }
    if content
        .fallback_text
        .chars()
        .any(|character| character.is_control())
    {
        return Err(ProtocolError::InvalidExtension(
            ExtensionError::FallbackControlCharacter,
        ));
    }

    let body = WireValue::structure([
        WireField::new(field::SCHEMA_VERSION, WireValue::U64(1)),
        WireField::new(
            field::EXTENSION_NAMESPACE,
            WireValue::Text(content.namespace.clone()),
        ),
        WireField::new(
            field::EXTENSION_SCHEMA_VERSION,
            WireValue::U64(u64::from(content.schema_version)),
        ),
        WireField::new(
            field::EXTENSION_OPERATION_ID,
            WireValue::id_bytes(content.operation_id.as_bytes()),
        ),
        WireField::new(
            field::EXTENSION_OBJECT_ID,
            WireValue::id_bytes(content.object_id.as_bytes()),
        ),
        WireField::new(
            field::EXTENSION_PAYLOAD,
            WireValue::Bytes(content.payload.clone()),
        ),
        WireField::new(
            field::EXTENSION_FALLBACK,
            WireValue::Text(content.fallback_text.clone()),
        ),
    ]);
    let encoded = encode_canonical(&body, ProtocolVersion::V1)?;
    if encoded.len() > limits.max_message_bytes {
        return Err(ProtocolError::ContentTooLarge {
            limit: limits.max_message_bytes,
        });
    }

    Ok(ValidatedExtension {
        namespace: content.namespace,
        schema_version: content.schema_version,
        operation_id: content.operation_id,
        object_id: content.object_id,
        payload: content.payload,
        fallback_text: content.fallback_text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    fn content() -> ExtensionContent {
        ExtensionContent {
            namespace: "notes.v1".to_owned(),
            schema_version: 1,
            operation_id: OperationId::from_bytes([1; 32]),
            object_id: ObjectId::from_bytes([2; 32]),
            payload: b"opaque-payload".to_vec(),
            fallback_text: "插件未安装".to_owned(),
        }
    }

    #[test]
    fn unknown_namespace_is_preserved_with_fallback() {
        let validated = validate_extension(content(), ContentLimits::DEFAULT).unwrap();
        assert_eq!(validated.namespace, "notes.v1");
        assert_eq!(validated.payload, b"opaque-payload");
        assert_eq!(validated.fallback_text, "插件未安装");
        assert_eq!(validated.schema_version, 1);
        assert_eq!(validated.operation_id, OperationId::from_bytes([1; 32]));
        assert_eq!(validated.object_id, ObjectId::from_bytes([2; 32]));
    }

    #[test]
    fn rejects_invalid_namespace_and_fallback() {
        let mut bad = content();
        bad.namespace = String::new();
        assert_eq!(
            validate_extension(bad, ContentLimits::DEFAULT).unwrap_err(),
            ProtocolError::InvalidExtension(ExtensionError::EmptyNamespace)
        );
        let mut bad = content();
        bad.namespace = "Notes.V1".to_owned();
        assert_eq!(
            validate_extension(bad, ContentLimits::DEFAULT).unwrap_err(),
            ProtocolError::InvalidExtension(ExtensionError::NamespaceNotCanonical)
        );
        let mut bad = content();
        bad.namespace = "notes..v1".to_owned();
        assert!(validate_extension(bad, ContentLimits::DEFAULT).is_err());
        let mut bad = content();
        bad.namespace = "core.control".to_owned();
        assert_eq!(
            validate_extension(bad, ContentLimits::DEFAULT).unwrap_err(),
            ProtocolError::InvalidExtension(ExtensionError::ReservedNamespace)
        );
        let mut bad = content();
        bad.fallback_text = "   ".to_owned();
        assert_eq!(
            validate_extension(bad, ContentLimits::DEFAULT).unwrap_err(),
            ProtocolError::InvalidExtension(ExtensionError::EmptyFallback)
        );
        let mut bad = content();
        bad.schema_version = 0;
        assert_eq!(
            validate_extension(bad, ContentLimits::DEFAULT).unwrap_err(),
            ProtocolError::InvalidExtension(ExtensionError::ZeroSchemaVersion)
        );
    }

    #[test]
    fn oversized_payload_and_fallback_are_bounded() {
        let mut big = content();
        big.payload = vec![0; ContentLimits::DEFAULT.max_payload_bytes + 1];
        assert_eq!(
            validate_extension(big, ContentLimits::DEFAULT).unwrap_err(),
            ProtocolError::InvalidExtension(ExtensionError::PayloadTooLarge {
                limit: ContentLimits::DEFAULT.max_payload_bytes
            })
        );
        let mut big = content();
        big.fallback_text = "a".repeat(ContentLimits::DEFAULT.max_fallback_bytes + 1);
        assert_eq!(
            validate_extension(big, ContentLimits::DEFAULT).unwrap_err(),
            ProtocolError::InvalidExtension(ExtensionError::FallbackTooLong {
                limit: ContentLimits::DEFAULT.max_fallback_bytes
            })
        );
        // 完整消息预算更低时按更低预算拒绝。
        let tight = ContentLimits::new(64, MAX_EXTENSION_PAYLOAD_BYTES, MAX_FALLBACK_BYTES);
        assert_eq!(
            validate_extension(content(), tight).unwrap_err().code(),
            ErrorCode::ContentTooLarge
        );
    }

    #[test]
    fn repeated_validation_is_byte_identical() {
        let first = validate_extension(content(), ContentLimits::DEFAULT).unwrap();
        let second = validate_extension(content(), ContentLimits::DEFAULT).unwrap();
        assert_eq!(first, second);
    }
}
