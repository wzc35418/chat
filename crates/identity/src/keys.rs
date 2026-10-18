//! 账户根密钥、设备密钥、账户/设备 ID 派生与人工核验指纹。
//!
//! 派生规则（本 crate 冻结）：
//!
//! - 账户根密钥 = Ed25519，私钥由 32 字节种子确定；长期公钥即账户身份公钥；
//! - AccountId 负载 = SHA-256("chat/p1/account-id/v1" || 账户公钥)；
//! - DeviceId  负载 = SHA-256("chat/p1/device-id/v1" || AccountId || 设备公钥)；
//! - 指纹 = SHA-256("chat/p1/account-fingerprint/v1" || 账户公钥)，只用于人工核对。
//!
//! 三者用途域分离：同一公钥的 ID 与指纹互不相同，改节点地址/显示名不改变它们。
//! 私钥类型不派生 Clone/Copy，Debug 恒为脱敏占位；种子缓冲用 zeroize 清理，
//! ed25519-dalek 的 SigningKey 在 drop 时自行清零。

use core::fmt;

use chat_protocol::signing::{sign_bytes, signing_key_from_seed, verify_ed25519};
use chat_protocol::{AccountId, DeviceId, Digest};
use ed25519_dalek::{SigningKey, VerifyingKey};
use zeroize::Zeroizing;

use crate::error::{IdentityError, Result};

/// 密钥种子字节数。
pub const KEY_SEED_BYTES: usize = 32;
/// Ed25519 公钥字节数。
pub const PUBLIC_KEY_BYTES: usize = 32;
/// Ed25519 签名字节数。
pub const SIGNATURE_BYTES: usize = 64;
/// 账户 ID 派生域。
pub const ACCOUNT_ID_DOMAIN: &[u8] = b"chat/p1/account-id/v1";
/// 设备 ID 派生域。
pub const DEVICE_ID_DOMAIN: &[u8] = b"chat/p1/device-id/v1";
/// 人工核验指纹派生域。
pub const FINGERPRINT_DOMAIN: &[u8] = b"chat/p1/account-fingerprint/v1";
/// 指纹文本（含分隔空格）允许的最大长度。
pub const MAX_FINGERPRINT_TEXT_BYTES: usize = 96;
/// 指纹每组的十六进制字符数。
pub const FINGERPRINT_GROUP_HEX_CHARS: usize = 4;

/// 用途域哈希：SHA-256(domain || 各分段)。
fn domain_digest(domain: &[u8], parts: &[&[u8]]) -> Digest {
    let extra: usize = parts.iter().map(|part| part.len()).sum();
    let mut buffer = Vec::with_capacity(domain.len() + extra);
    buffer.extend_from_slice(domain);
    for part in parts {
        buffer.extend_from_slice(part);
    }
    Digest::sha256(&buffer)
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: [u8; 16] = *b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[(byte >> 4) as usize]));
        text.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    text
}

/// 由账户长期公钥派生账户 ID。
///
/// 该函数不做公钥合法性检查（派生本身是纯哈希）；外部公钥进入信任记录前必须
/// 经过 [TrustAnchor::from_public_key] 的曲线点校验。
pub fn account_id_from_public_key(public_key: &[u8; PUBLIC_KEY_BYTES]) -> AccountId {
    AccountId::from_bytes(*domain_digest(ACCOUNT_ID_DOMAIN, &[public_key]).as_bytes())
}

/// 由（账户, 设备公钥）派生设备 ID。
pub fn device_id_for(account: AccountId, device_public_key: &[u8; PUBLIC_KEY_BYTES]) -> DeviceId {
    DeviceId::from_bytes(
        *domain_digest(DEVICE_ID_DOMAIN, &[account.as_bytes(), device_public_key]).as_bytes(),
    )
}

fn key_signing_key(seed: &[u8; KEY_SEED_BYTES]) -> SigningKey {
    signing_key_from_seed(seed)
}

fn generate_seed() -> Zeroizing<[u8; KEY_SEED_BYTES]> {
    let mut seed = Zeroizing::new([0u8; KEY_SEED_BYTES]);
    rand::fill(seed.as_mut());
    seed
}

/// 账户根密钥对。持有长期账户私钥；Debug 恒为脱敏占位。
pub struct AccountKeys {
    signing: SigningKey,
}

impl AccountKeys {
    /// 从 32 字节种子派生（同一硬币种子恒得同一账户）。
    pub fn from_seed(seed: &[u8; KEY_SEED_BYTES]) -> Self {
        Self {
            signing: key_signing_key(seed),
        }
    }

    /// 用系统安全随机源生成新账户根密钥。
    pub fn generate() -> Self {
        let seed = generate_seed();
        Self::from_seed(&seed)
    }

    /// 账户长期公钥。
    pub fn public_key(&self) -> [u8; PUBLIC_KEY_BYTES] {
        self.signing.verifying_key().to_bytes()
    }

    /// 由公钥派生的账户 ID。
    pub fn account_id(&self) -> AccountId {
        account_id_from_public_key(&self.public_key())
    }

    /// 人工核验指纹。
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of_public_key(&self.public_key())
    }

    /// 账户信任锚（公钥 + 派生 ID）。
    pub fn trust_anchor(&self) -> TrustAnchor {
        TrustAnchor {
            account_id: self.account_id(),
            account_public_key: self.public_key(),
        }
    }

    /// 用账户根密钥签名；原文由调用方按各自用途域构造。
    pub(crate) fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_BYTES] {
        sign_bytes(&self.signing, message)
    }
}

impl fmt::Debug for AccountKeys {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccountKeys(<redacted>)")
    }
}

/// 设备签名密钥对。Debug 恒为脱敏占位。
pub struct DeviceKeys {
    signing: SigningKey,
}

impl DeviceKeys {
    /// 从 32 字节种子派生。
    pub fn from_seed(seed: &[u8; KEY_SEED_BYTES]) -> Self {
        Self {
            signing: key_signing_key(seed),
        }
    }

    /// 用系统安全随机源生成新设备密钥。
    pub fn generate() -> Self {
        let seed = generate_seed();
        Self::from_seed(&seed)
    }

    /// 设备公钥。
    pub fn public_key(&self) -> [u8; PUBLIC_KEY_BYTES] {
        self.signing.verifying_key().to_bytes()
    }

    /// 该设备在指定账户下的设备 ID。
    pub fn device_id(&self, account: AccountId) -> DeviceId {
        device_id_for(account, &self.public_key())
    }
}

impl fmt::Debug for DeviceKeys {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DeviceKeys(<redacted>)")
    }
}

/// 人工核验指纹（32 字节摘要，按 4 位十六进制分组展示）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    /// 由账户公钥派生指纹。
    pub fn of_public_key(public_key: &[u8; PUBLIC_KEY_BYTES]) -> Self {
        Self(*domain_digest(FINGERPRINT_DOMAIN, &[public_key]).as_bytes())
    }

    /// 由固定字节构造（只用于已持久化的指纹）。
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// 只读字节。
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// 连续小写十六进制（64 字符）。
    pub fn compact_hex(&self) -> String {
        hex_lower(&self.0)
    }

    /// 分组展示：16 组、每组 4 位大写十六进制，单空格分隔（79 字符）。
    pub fn grouped(&self) -> String {
        let text = hex_lower(&self.0).to_uppercase();
        let mut out = String::with_capacity(MAX_FINGERPRINT_TEXT_BYTES.min(79));
        for (index, chunk) in text.as_bytes().chunks(FINGERPRINT_GROUP_HEX_CHARS).enumerate() {
            if index > 0 {
                out.push(' ');
            }
            for byte in chunk {
                out.push(char::from(*byte));
            }
        }
        out
    }

    /// 解析分组形式或连续十六进制形式（大小写不敏感，允许空格分组）。
    ///
    /// 先限长，再要求只剩十六进制字符，最后要求恰好 64 位。
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() > MAX_FINGERPRINT_TEXT_BYTES {
            return Err(IdentityError::TokenTooLong {
                limit: MAX_FINGERPRINT_TEXT_BYTES,
            });
        }
        let mut digits = String::with_capacity(64);
        for character in text.chars() {
            if character == ' ' {
                continue;
            }
            if !character.is_ascii_hexdigit() {
                return Err(IdentityError::MalformedToken);
            }
            digits.push(character.to_ascii_lowercase());
        }
        if digits.len() != 64 {
            return Err(IdentityError::MalformedToken);
        }
        let mut bytes = [0u8; 32];
        let (pairs, remainder) = digits.as_bytes().as_chunks::<2>();
        debug_assert!(remainder.is_empty());
        for (index, pair) in pairs.iter().enumerate() {
            let hi = hex_digit(pair[0])?;
            let lo = hex_digit(pair[1])?;
            bytes[index] = (hi << 4) | lo;
        }
        Ok(Self(bytes))
    }
}

fn hex_digit(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(IdentityError::MalformedToken),
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Fingerprint({})", self.grouped())
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.grouped())
    }
}

/// 已固定的账户信任锚：账户 ID 与账户公钥必须自洽。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrustAnchor {
    account_id: AccountId,
    account_public_key: [u8; PUBLIC_KEY_BYTES],
}

impl TrustAnchor {
    /// 校验公钥是合法 Ed25519 点，并由它派生账户 ID（推荐入口）。
    pub fn from_public_key(account_public_key: [u8; PUBLIC_KEY_BYTES]) -> Result<Self> {
        let verifying_key = VerifyingKey::from_bytes(&account_public_key)
            .map_err(|_| IdentityError::InvalidAccountPublicKey)?;
        // 低阶公钥可以伪造「任意消息都成立」的签名，信任锚必须拒绝。
        if verifying_key.is_weak() {
            return Err(IdentityError::InvalidAccountPublicKey);
        }
        Ok(Self {
            account_id: account_id_from_public_key(&account_public_key),
            account_public_key,
        })
    }

    /// 校验（账户 ID, 公钥）自洽后构造；不自洽即显式失败。
    pub fn pinned(
        account_id: AccountId,
        account_public_key: [u8; PUBLIC_KEY_BYTES],
    ) -> Result<Self> {
        let anchor = Self::from_public_key(account_public_key)?;
        if anchor.account_id != account_id {
            return Err(IdentityError::AccountKeyMismatch);
        }
        Ok(anchor)
    }

    /// 账户 ID。
    pub const fn account_id(&self) -> AccountId {
        self.account_id
    }

    /// 账户公钥。
    pub const fn account_public_key(&self) -> &[u8; PUBLIC_KEY_BYTES] {
        &self.account_public_key
    }

    /// 该信任锚的人内核验指纹。
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of_public_key(&self.account_public_key)
    }
}

/// 用已验证公钥验证一段 Ed25519 签名（签名原文由调用方按用途域构造）。
pub(crate) fn verify_signature(
    public_key: &[u8; PUBLIC_KEY_BYTES],
    message: &[u8],
    signature: &[u8; SIGNATURE_BYTES],
) -> Result<()> {
    verify_ed25519(public_key, message, signature).map_err(|_| IdentityError::InvalidSignature)
}
#[cfg(test)]
mod tests {
    use super::*;
    use chat_protocol::Digest;

    fn hex(bytes: &[u8]) -> String {
        hex_lower(bytes)
    }

    fn vector_account() -> AccountKeys {
        AccountKeys::from_seed(&[0x01; KEY_SEED_BYTES])
    }

    #[test]
    fn account_id_and_fingerprint_match_independent_vectors() {
        let keys = vector_account();
        // 以下常量由独立实现（OpenSSL/Node WebCrypto 的 Ed25519 + SHA-256）算出，
        // 用于冻结 ID 派生域与指纹展示格式。
        assert_eq!(
            hex(&keys.public_key()),
            "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c"
        );
        assert_eq!(
            keys.account_id().encode(),
            "acct:1:804d17d85c433dc00878aceda0dff5d4d6d72dd6dbdabda188e4ae177304bffa"
        );
        assert_eq!(
            keys.fingerprint().compact_hex(),
            "7ec8c5e1a37cc538afcba0b0bd28c9e15a4b1a3e53dc0f18be096df947428eb4"
        );
        assert_eq!(
            keys.fingerprint().grouped(),
            "7EC8 C5E1 A37C C538 AFCB A0B0 BD28 C9E1 5A4B 1A3E 53DC 0F18 BE09 6DF9 4742 8EB4"
        );
        // Display 与 grouped 必须完全一致。
        assert_eq!(keys.fingerprint().to_string(), keys.fingerprint().grouped());
        // 设备 ID 冻结向量：账户种子 0x01 + 设备种子 0x11。
        let device = DeviceKeys::from_seed(&[0x11; KEY_SEED_BYTES]);
        assert_eq!(
            hex(&device.public_key()),
            "d04ab232742bb4ab3a1368bd4615e4e6d0224ab71a016baf8520a332c9778737"
        );
        assert_eq!(
            device.device_id(keys.account_id()).encode(),
            "dev:1:a4ec09edbb8ceb00f41b77fed48dbda0796e4456e2a99e813dfcad390bc9e1f5"
        );
    }

    #[test]
    fn derivations_are_domain_separated_and_key_bound() {
        let keys = vector_account();
        let public_key = keys.public_key();
        let expected_id = Digest::sha256(&[ACCOUNT_ID_DOMAIN, public_key.as_slice()].concat());
        assert_eq!(keys.account_id().as_bytes(), expected_id.as_bytes());
        let expected_fingerprint =
            Digest::sha256(&[FINGERPRINT_DOMAIN, public_key.as_slice()].concat());
        assert_eq!(keys.fingerprint().as_bytes(), expected_fingerprint.as_bytes());
        assert_ne!(keys.account_id().as_bytes(), keys.fingerprint().as_bytes());

        let other = AccountKeys::from_seed(&[0x02; KEY_SEED_BYTES]);
        assert_ne!(other.account_id(), keys.account_id());
        assert_ne!(other.fingerprint(), keys.fingerprint());

        // 同一设备公钥在不同账户下派生出不同设备 ID。
        let device = DeviceKeys::from_seed(&[0x11; KEY_SEED_BYTES]);
        let elsewhere = device_id_for(other.account_id(), &device.public_key());
        assert_eq!(device.device_id(other.account_id()), elsewhere);
        assert_ne!(device.device_id(keys.account_id()), elsewhere);
        // 同一输入恒得同一 ID。
        assert_eq!(
            device_id_for(keys.account_id(), &device.public_key()),
            device.device_id(keys.account_id())
        );
    }

    #[test]
    fn grouped_fingerprint_shape_and_parse_round_trip() {
        let grouped = vector_account().fingerprint().grouped();
        assert_eq!(grouped.len(), 79);
        let groups: Vec<&str> = grouped.split(' ').collect();
        assert_eq!(groups.len(), 16);
        for group in &groups {
            assert_eq!(group.len(), FINGERPRINT_GROUP_HEX_CHARS);
            assert!(group.chars().all(|character| character.is_ascii_hexdigit()));
            assert_eq!(*group, group.to_uppercase());
        }

        let fingerprint = vector_account().fingerprint();
        assert_eq!(Fingerprint::parse(&grouped).expect("parse grouped"), fingerprint);
        assert_eq!(
            Fingerprint::parse(&fingerprint.compact_hex()).expect("parse compact"),
            fingerprint
        );
        assert_eq!(
            Fingerprint::parse(&grouped.to_lowercase()).expect("parse lowercase"),
            fingerprint
        );
        assert_eq!(
            Fingerprint::parse(&format!("  {grouped}  ")).expect("parse padded"),
            fingerprint
        );
    }

    #[test]
    fn malformed_fingerprint_text_is_rejected() {
        let long = "A".repeat(MAX_FINGERPRINT_TEXT_BYTES + 1);
        assert_eq!(
            Fingerprint::parse(&long).unwrap_err(),
            IdentityError::TokenTooLong {
                limit: MAX_FINGERPRINT_TEXT_BYTES
            }
        );
        assert_eq!(
            Fingerprint::parse("zzzz").unwrap_err(),
            IdentityError::MalformedToken
        );
        assert_eq!(
            Fingerprint::parse(&"a".repeat(63)).unwrap_err(),
            IdentityError::MalformedToken
        );
        assert_eq!(
            Fingerprint::parse("ＦＦ").unwrap_err(),
            IdentityError::MalformedToken
        );
        assert!(Fingerprint::parse("").is_err());
    }

    #[test]
    fn trust_anchor_requires_self_consistent_keys() {
        let keys = vector_account();
        let anchor = TrustAnchor::from_public_key(keys.public_key()).expect("anchor");
        assert_eq!(anchor.account_id(), keys.account_id());
        assert_eq!(anchor.account_public_key(), &keys.public_key());
        assert_eq!(anchor.fingerprint(), keys.fingerprint());

        let mismatched = AccountKeys::from_seed(&[0x02; KEY_SEED_BYTES]).account_id();
        assert_eq!(
            TrustAnchor::pinned(mismatched, keys.public_key()).unwrap_err(),
            IdentityError::AccountKeyMismatch
        );
        // y = 2 的压缩点不可解压（x 不是二次剩余）。
        let mut non_point = [0u8; PUBLIC_KEY_BYTES];
        non_point[0] = 2;
        assert_eq!(
            TrustAnchor::from_public_key(non_point).unwrap_err(),
            IdentityError::InvalidAccountPublicKey
        );
        // 低阶（weak）公钥必须拒绝：它能让任意消息的签名都通过。
        assert_eq!(
            TrustAnchor::from_public_key([0u8; PUBLIC_KEY_BYTES]).unwrap_err(),
            IdentityError::InvalidAccountPublicKey
        );
        assert!(TrustAnchor::pinned(keys.account_id(), keys.public_key()).is_ok());
    }

    #[test]
    fn private_keys_are_redacted_in_debug_output() {
        let account = AccountKeys::from_seed(&[0xab; KEY_SEED_BYTES]);
        let device = DeviceKeys::from_seed(&[0xcd; KEY_SEED_BYTES]);
        assert_eq!(format!("{account:?}"), "AccountKeys(<redacted>)");
        assert_eq!(format!("{device:?}"), "DeviceKeys(<redacted>)");
        for rendered in [format!("{account:?}"), format!("{device:?}")] {
            assert!(!rendered.contains("abab"));
            assert!(!rendered.contains("cdcd"));
            assert!(!rendered.to_ascii_lowercase().contains("ababab"));
        }
    }

    #[test]
    fn generated_keys_differ_and_are_well_formed() {
        let first = AccountKeys::generate();
        let second = AccountKeys::generate();
        assert_ne!(first.account_id(), second.account_id());
        // 生成路径产出的公钥必须是合法曲线点。
        assert!(TrustAnchor::from_public_key(first.public_key()).is_ok());
        assert_eq!(
            first.fingerprint(),
            Fingerprint::of_public_key(&first.public_key())
        );
    }
}
