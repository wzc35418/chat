//! 摘要与控制头。二者都是 32 字节，但类型不同，禁止互相替换。
//!
//! `Digest` 是用途域内的 SHA-256 摘要；`ControlHead` 是正式控制状态标识，
//! 与日志头 `previous_log_hash` 属于不同语义，不能混用。

use core::fmt;

use sha2::{Digest as _, Sha256};

use crate::error::{IdError, ProtocolError, Result};

/// 摘要字节数。
pub const DIGEST_BYTES: usize = 32;

/// SHA-256 摘要。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest([u8; DIGEST_BYTES]);

impl Digest {
    /// 全零摘要，仅用于测试向量与占位。
    pub const ZERO: Digest = Digest([0; DIGEST_BYTES]);

    /// 由固定字节构造。
    pub const fn from_bytes(bytes: [u8; DIGEST_BYTES]) -> Self {
        Self(bytes)
    }

    /// 只读字节。
    pub const fn as_bytes(&self) -> &[u8; DIGEST_BYTES] {
        &self.0
    }

    /// 对一段字节做 SHA-256。调用方负责在输入里自行加入用途域分隔。
    pub fn sha256(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Self(hasher.finalize().into())
    }

    /// 严格解析 64 个小写十六进制字符。
    pub fn from_hex(text: &str) -> Result<Self> {
        if text.len() != DIGEST_BYTES * 2 {
            return Err(ProtocolError::InvalidId(IdError::WrongLength {
                expected: DIGEST_BYTES * 2,
                found: text.len(),
            }));
        }
        let mut out = [0u8; DIGEST_BYTES];
        let (pairs, remainder) = text.as_bytes().as_chunks::<2>();
        debug_assert!(remainder.is_empty());
        for (index, pair) in pairs.iter().enumerate() {
            out[index] = (hex_digit(pair[0])? << 4) | hex_digit(pair[1])?;
        }
        Ok(Self(out))
    }

    /// 小写十六进制。
    pub fn to_hex(self) -> String {
        let mut text = String::with_capacity(DIGEST_BYTES * 2);
        for byte in self.0 {
            text.push(char::from(HEX[(byte >> 4) as usize]));
            text.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
        text
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Digest({})", self.to_hex())
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

/// 正式控制头标识。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ControlHead([u8; DIGEST_BYTES]);

impl ControlHead {
    /// 全零控制头（新群初始值）。
    pub const ZERO: ControlHead = ControlHead([0; DIGEST_BYTES]);

    /// 由固定字节构造。
    pub const fn from_bytes(bytes: [u8; DIGEST_BYTES]) -> Self {
        Self(bytes)
    }

    /// 只读字节。
    pub const fn as_bytes(&self) -> &[u8; DIGEST_BYTES] {
        &self.0
    }

    /// 严格解析 64 个小写十六进制字符。
    pub fn from_hex(text: &str) -> Result<Self> {
        Digest::from_hex(text).map(|digest| Self(*digest.as_bytes()))
    }

    /// 小写十六进制。
    pub fn to_hex(self) -> String {
        Digest::from_bytes(self.0).to_hex()
    }
}

impl fmt::Debug for ControlHead {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ControlHead({})", self.to_hex())
    }
}

impl fmt::Display for ControlHead {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

const HEX: [u8; 16] = *b"0123456789abcdef";

fn hex_digit(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(ProtocolError::InvalidId(IdError::NonCanonicalHex)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vector() {
        // FIPS 180-4 的空消息摘要。
        assert_eq!(
            Digest::sha256(b"").to_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // "abc"。
        assert_eq!(
            Digest::sha256(b"abc").to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hex_round_trip_is_canonical() {
        let digest = Digest::sha256(b"x");
        let text = digest.to_hex();
        assert_eq!(Digest::from_hex(&text).unwrap(), digest);
        assert!(Digest::from_hex(&text.to_uppercase()).is_err());
        assert!(Digest::from_hex("ab").is_err());
    }

    #[test]
    fn control_head_and_digest_are_not_interchangeable() {
        let digest = Digest::from_bytes([1; 32]);
        let head = ControlHead::from_bytes([1; 32]);
        assert_eq!(head.to_hex(), digest.to_hex());
        // 类型不同：下面这行如果取消注释将无法编译。
        // let _: ControlHead = digest;
        assert_eq!(head.as_bytes(), digest.as_bytes());
    }
}
