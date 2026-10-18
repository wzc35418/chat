//! PROTOCOL-003：有界解码。
//!
//! 解码只做结构校验：返回值类型 `WireFrame` 内嵌私有 `Unverified` 标记，
//! 类型系统上不能冒充已验证的候选/日志条目。成功解析不代表作者可信。
//!
//! 预算先于分配生效：总长、嵌套深度、集合元素数、文本长度都先检查；
//! 长度前缀必须落在剩余字节内，重复/乱序/未知字段号与尾随字节都显式失败。

use core::fmt;

use crate::canonical::{
    CANONICAL_MAGIC, MAX_CANONICAL_BYTES, MAX_CANONICAL_DEPTH, MAX_CANONICAL_TEXT_BYTES,
    MAX_COLLECTION_ITEMS, ProtocolVersion, WireField, WireValue, is_known_field,
};
use crate::error::{ProtocolError, Result};

const TAG_U64: u8 = 0x01;
const TAG_BYTES: u8 = 0x02;
const TAG_TEXT: u8 = 0x03;
const TAG_NONE: u8 = 0x04;
const TAG_SOME: u8 = 0x05;
const TAG_LIST: u8 = 0x06;
const TAG_STRUCT: u8 = 0x07;

/// 结构合法但未认证的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MalformedFrame {
    Truncated,
    TrailingBytes,
    BadMagic,
    UnknownTag(u8),
    UnknownField(u16),
    DuplicateField(u16),
    UnsortedField(u16),
    InvalidUtf8,
    TooDeep { limit: usize },
    CollectionTooLarge { limit: usize },
    TextTooLarge { limit: usize },
}

impl fmt::Display for MalformedFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MalformedFrame::Truncated => formatter.write_str("truncated"),
            MalformedFrame::TrailingBytes => formatter.write_str("trailing bytes"),
            MalformedFrame::BadMagic => formatter.write_str("bad magic"),
            MalformedFrame::UnknownTag(tag) => write!(formatter, "unknown tag {tag:#04x}"),
            MalformedFrame::UnknownField(id) => write!(formatter, "unknown field {id}"),
            MalformedFrame::DuplicateField(id) => write!(formatter, "duplicate field {id}"),
            MalformedFrame::UnsortedField(id) => write!(formatter, "unsorted field {id}"),
            MalformedFrame::InvalidUtf8 => formatter.write_str("invalid utf-8"),
            MalformedFrame::TooDeep { limit } => write!(formatter, "depth exceeds {limit}"),
            MalformedFrame::CollectionTooLarge { limit } => {
                write!(formatter, "collection exceeds {limit}")
            }
            MalformedFrame::TextTooLarge { limit } => write!(formatter, "text exceeds {limit}"),
        }
    }
}

/// 解码预算。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeLimits {
    /// 字节总长上限。
    pub max_bytes: usize,
    /// 嵌套深度上限。
    pub max_depth: usize,
    /// 单集合元素数上限。
    pub max_collection_items: usize,
    /// 单文本 UTF-8 字节上限。
    pub max_text_bytes: usize,
}

impl DecodeLimits {
    /// 冻结默认预算。
    pub const DEFAULT: DecodeLimits = DecodeLimits {
        max_bytes: MAX_CANONICAL_BYTES,
        max_depth: MAX_CANONICAL_DEPTH,
        max_collection_items: MAX_COLLECTION_ITEMS,
        max_text_bytes: MAX_CANONICAL_TEXT_BYTES,
    };

    /// 显式构造预算。
    pub const fn new(
        max_bytes: usize,
        max_depth: usize,
        max_collection_items: usize,
        max_text_bytes: usize,
    ) -> Self {
        Self {
            max_bytes,
            max_depth,
            max_collection_items,
            max_text_bytes,
        }
    }
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Unverified;

/// 只通过有界解码得到的未认证帧。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireFrame {
    version: ProtocolVersion,
    value: WireValue,
    unverified: Unverified,
}

impl WireFrame {
    /// 帧声明的协议版本。
    pub const fn version(&self) -> ProtocolVersion {
        self.version
    }

    /// 解码后的值。
    pub const fn value(&self) -> &WireValue {
        &self.value
    }

    /// 取出值。
    pub fn into_value(self) -> WireValue {
        self.value
    }
}

impl fmt::Debug for Unverified {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Unverified")
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
    limits: DecodeLimits,
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        if length > self.remaining() {
            return Err(ProtocolError::MalformedFrame(MalformedFrame::Truncated));
        }
        let start = self.offset;
        self.offset += length;
        Ok(&self.bytes[start..self.offset])
    }

    fn read_u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn read_u16(&mut self) -> Result<u16> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self) -> Result<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_length(&mut self, limit: usize) -> Result<usize> {
        let length = self.read_u32()? as usize;
        if length > limit {
            return Err(ProtocolError::ContentTooLarge { limit });
        }
        Ok(length)
    }

    fn value(&mut self, depth: usize) -> Result<WireValue> {
        if depth > self.limits.max_depth {
            return Err(ProtocolError::MalformedFrame(MalformedFrame::TooDeep {
                limit: self.limits.max_depth,
            }));
        }
        let tag = self.read_u8()?;
        match tag {
            TAG_U64 => {
                let bytes = self.take(8)?;
                Ok(WireValue::U64(u64::from_be_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ])))
            }
            TAG_BYTES => {
                let length = self.read_length(self.limits.max_bytes)?;
                Ok(WireValue::Bytes(self.take(length)?.to_vec()))
            }
            TAG_TEXT => {
                let length = self.read_length(self.limits.max_text_bytes)?;
                let bytes = self.take(length)?;
                let text = core::str::from_utf8(bytes)
                    .map_err(|_| ProtocolError::MalformedFrame(MalformedFrame::InvalidUtf8))?;
                Ok(WireValue::Text(text.to_owned()))
            }
            TAG_NONE => Ok(WireValue::None),
            TAG_SOME => Ok(WireValue::Some(Box::new(self.value(depth + 1)?))),
            TAG_LIST => {
                let count = self.read_u32()? as usize;
                if count > self.limits.max_collection_items {
                    return Err(ProtocolError::MalformedFrame(
                        MalformedFrame::CollectionTooLarge {
                            limit: self.limits.max_collection_items,
                        },
                    ));
                }
                // 每个元素至少 1 字节 tag；先筛掉不可能的计数，避免为伪造计数分配内存。
                if count > self.remaining() {
                    return Err(ProtocolError::MalformedFrame(MalformedFrame::Truncated));
                }
                let mut items = Vec::with_capacity(count);
                for _ in 0..count {
                    items.push(self.value(depth + 1)?);
                }
                Ok(WireValue::List(items))
            }
            TAG_STRUCT => {
                let count = self.read_u16()? as usize;
                if count > self.limits.max_collection_items {
                    return Err(ProtocolError::MalformedFrame(
                        MalformedFrame::CollectionTooLarge {
                            limit: self.limits.max_collection_items,
                        },
                    ));
                }
                // 每个字段至少 2 字节字段号 + 1 字节 tag。
                if count.saturating_mul(3) > self.remaining() {
                    return Err(ProtocolError::MalformedFrame(MalformedFrame::Truncated));
                }
                let mut fields = Vec::with_capacity(count);
                let mut previous: Option<u16> = None;
                for _ in 0..count {
                    let id = self.read_u16()?;
                    if !is_known_field(id) {
                        return Err(ProtocolError::MalformedFrame(MalformedFrame::UnknownField(
                            id,
                        )));
                    }
                    match previous {
                        Some(previous) if previous == id => {
                            return Err(ProtocolError::MalformedFrame(
                                MalformedFrame::DuplicateField(id),
                            ));
                        }
                        Some(previous) if previous > id => {
                            return Err(ProtocolError::MalformedFrame(
                                MalformedFrame::UnsortedField(id),
                            ));
                        }
                        _ => {}
                    }
                    previous = Some(id);
                    fields.push(WireField::new(id, self.value(depth + 1)?));
                }
                Ok(WireValue::Struct(fields))
            }
            other => Err(ProtocolError::MalformedFrame(MalformedFrame::UnknownTag(
                other,
            ))),
        }
    }
}

/// PROTOCOL-003：在预算内解码规范字节。
///
/// 顺序：总长检查 → 魔数与版本 → 有界递归解析（拒绝重复/乱序/未知字段与溢出）
/// → 拒绝尾随字节 → 返回带 `Unverified` 标记的帧。
pub fn decode_bounded(bytes: &[u8], limits: DecodeLimits) -> Result<WireFrame> {
    if bytes.len() > limits.max_bytes {
        return Err(ProtocolError::ContentTooLarge {
            limit: limits.max_bytes,
        });
    }
    if bytes.len() < CANONICAL_MAGIC.len() + 2 {
        return Err(ProtocolError::MalformedFrame(MalformedFrame::Truncated));
    }
    if &bytes[..CANONICAL_MAGIC.len()] != CANONICAL_MAGIC {
        return Err(ProtocolError::MalformedFrame(MalformedFrame::BadMagic));
    }
    let version = ProtocolVersion {
        major: bytes[CANONICAL_MAGIC.len()],
        minor: bytes[CANONICAL_MAGIC.len() + 1],
    };
    if !version.is_supported() {
        return Err(ProtocolError::UnsupportedProtocolVersion);
    }
    let mut reader = Reader {
        bytes,
        offset: CANONICAL_MAGIC.len() + 2,
        limits,
    };
    let value = reader.value(0)?;
    if reader.remaining() != 0 {
        return Err(ProtocolError::MalformedFrame(MalformedFrame::TrailingBytes));
    }
    Ok(WireFrame {
        version,
        value,
        unverified: Unverified,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::{encode_canonical, field};

    #[test]
    fn round_trips_encoded_value() {
        let value = WireValue::structure([
            WireField::new(field::GROUP_ID, WireValue::id_bytes(&[4; 32])),
            WireField::new(field::POLICY_VERSION, WireValue::U64(9)),
            WireField::new(
                field::CONTROL_OPERATION,
                WireValue::optional(Some(WireValue::U64(3))),
            ),
            WireField::new(
                field::EXTENSION_FALLBACK,
                WireValue::Text("fallback".to_owned()),
            ),
        ]);
        let encoded = encode_canonical(&value, ProtocolVersion::V1).unwrap();
        let frame = decode_bounded(encoded.as_bytes(), DecodeLimits::DEFAULT).unwrap();
        assert_eq!(frame.version(), ProtocolVersion::V1);
        assert_eq!(frame.value(), &value);
    }

    #[test]
    fn rejects_truncation_trailing_and_unsupported_version() {
        let encoded = encode_canonical(&WireValue::U64(5), ProtocolVersion::V1).unwrap();
        for cut in 0..encoded.len() {
            let error =
                decode_bounded(&encoded.as_bytes()[..cut], DecodeLimits::DEFAULT).unwrap_err();
            assert!(
                matches!(
                    error.code(),
                    crate::error::ErrorCode::MalformedFrame
                        | crate::error::ErrorCode::ContentTooLarge
                ),
                "cut {cut} gave {error}"
            );
        }
        let mut trailing = encoded.as_bytes().to_vec();
        trailing.push(0);
        assert_eq!(
            decode_bounded(&trailing, DecodeLimits::DEFAULT).unwrap_err(),
            ProtocolError::MalformedFrame(MalformedFrame::TrailingBytes)
        );
        let mut wrong_version = encoded.as_bytes().to_vec();
        wrong_version[3] = 2;
        assert_eq!(
            decode_bounded(&wrong_version, DecodeLimits::DEFAULT).unwrap_err(),
            ProtocolError::UnsupportedProtocolVersion
        );
        let mut bad_magic = encoded.as_bytes().to_vec();
        bad_magic[0] = b'X';
        assert_eq!(
            decode_bounded(&bad_magic, DecodeLimits::DEFAULT).unwrap_err(),
            ProtocolError::MalformedFrame(MalformedFrame::BadMagic)
        );
    }

    #[test]
    fn rejects_duplicate_unsorted_and_unknown_fields() {
        let struct_prefix = [b'C', b'P', b'C', 1, 0, 0x07];
        // 两个字段但字段号重复。
        let mut duplicate = struct_prefix.to_vec();
        duplicate.extend_from_slice(&2u16.to_be_bytes());
        duplicate.extend_from_slice(&field::POLICY_VERSION.to_be_bytes());
        duplicate.push(0x01);
        duplicate.extend_from_slice(&1u64.to_be_bytes());
        duplicate.extend_from_slice(&field::POLICY_VERSION.to_be_bytes());
        duplicate.push(0x01);
        duplicate.extend_from_slice(&2u64.to_be_bytes());
        assert_eq!(
            decode_bounded(&duplicate, DecodeLimits::DEFAULT).unwrap_err(),
            ProtocolError::MalformedFrame(MalformedFrame::DuplicateField(field::POLICY_VERSION))
        );
        // 字段号逆序。
        let mut unsorted = struct_prefix.to_vec();
        unsorted.extend_from_slice(&2u16.to_be_bytes());
        unsorted.extend_from_slice(&field::POLICY_VERSION.to_be_bytes());
        unsorted.push(0x01);
        unsorted.extend_from_slice(&1u64.to_be_bytes());
        unsorted.extend_from_slice(&field::SCHEMA_VERSION.to_be_bytes());
        unsorted.push(0x01);
        unsorted.extend_from_slice(&1u64.to_be_bytes());
        assert_eq!(
            decode_bounded(&unsorted, DecodeLimits::DEFAULT).unwrap_err(),
            ProtocolError::MalformedFrame(MalformedFrame::UnsortedField(field::SCHEMA_VERSION))
        );
        // 未知字段号 999。
        let mut unknown = struct_prefix.to_vec();
        unknown.extend_from_slice(&1u16.to_be_bytes());
        unknown.extend_from_slice(&999u16.to_be_bytes());
        unknown.push(0x01);
        unknown.extend_from_slice(&0u64.to_be_bytes());
        assert_eq!(
            decode_bounded(&unknown, DecodeLimits::DEFAULT).unwrap_err(),
            ProtocolError::MalformedFrame(MalformedFrame::UnknownField(999))
        );
    }

    #[test]
    fn rejects_double_excessive_counts_and_depth() {
        // 巨量数组：u32 计数远大于剩余字节。
        let mut huge_list = vec![b'C', b'P', b'C', 1, 0, 0x06];
        huge_list.extend_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            decode_bounded(&huge_list, DecodeLimits::DEFAULT).unwrap_err(),
            ProtocolError::MalformedFrame(MalformedFrame::CollectionTooLarge {
                limit: MAX_COLLECTION_ITEMS
            })
        );
        // 深嵌套：反复 Some。
        let mut deep = vec![b'C', b'P', b'C', 1, 0];
        deep.extend(std::iter::repeat_n(0x05, MAX_CANONICAL_DEPTH + 2));
        deep.push(0x01);
        deep.extend_from_slice(&0u64.to_be_bytes());
        assert!(matches!(
            decode_bounded(&deep, DecodeLimits::DEFAULT),
            Err(ProtocolError::MalformedFrame(
                MalformedFrame::TooDeep { .. }
            ))
        ));
        // 声明长度超过剩余字节。
        let mut overlong = vec![b'C', b'P', b'C', 1, 0, 0x02];
        overlong.extend_from_slice(&1024u32.to_be_bytes());
        assert_eq!(
            decode_bounded(&overlong, DecodeLimits::DEFAULT).unwrap_err(),
            ProtocolError::MalformedFrame(MalformedFrame::Truncated)
        );
        // 非 UTF-8 文本。
        let mut bad_text = vec![b'C', b'P', b'C', 1, 0, 0x03];
        bad_text.extend_from_slice(&1u32.to_be_bytes());
        bad_text.push(0xff);
        assert_eq!(
            decode_bounded(&bad_text, DecodeLimits::DEFAULT).unwrap_err(),
            ProtocolError::MalformedFrame(MalformedFrame::InvalidUtf8)
        );
    }

    #[test]
    fn random_malformed_input_never_panics() {
        let mut state = 0x1234_5678_9abc_def0u64;
        for length in 0..256usize {
            let mut bytes = vec![0u8; length];
            for byte in &mut bytes {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                *byte = (state >> 32) as u8;
            }
            // 一半带头，一半纯随机，确保既走魔数分支也走结构分支。
            if length % 2 == 0 && length >= 5 {
                bytes[..5].copy_from_slice(&[b'C', b'P', b'C', 1, 0]);
            }
            let limits = DecodeLimits::new(512, 4, 16, 64);
            let _ = decode_bounded(&bytes, limits);
        }
    }

    #[test]
    fn total_length_limit_is_enforced_before_parsing() {
        let bytes = vec![0u8; 32];
        assert_eq!(
            decode_bounded(&bytes, DecodeLimits::new(16, 4, 16, 16)).unwrap_err(),
            ProtocolError::ContentTooLarge { limit: 16 }
        );
    }
}
