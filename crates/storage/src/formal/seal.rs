//! 本地存储的对称加密封装（P0-04 基础）。
//!
//! 设计约束：
//! - 主密钥固定 32 字节，只以 [`ProtectedStorageKey`] 形式在可信核心路径内存在；
//!   `Drop` 时 zeroize，`Debug` 永不输出密钥字节，也不实现 `Clone`。
//! - 每个用途从主密钥派生独立子密钥（域分隔），不复用主密钥直接加密。
//! - 每个封装值带版本字节 + 12 字节随机 nonce；AAD 由“用途 + 行身份字段”的
//!   长度前缀规范化编码构成，因此把密文挪到别的表/行/版本都会认证失败。
//! - 打开失败一律返回 `STATE_CORRUPTED`，不区分“密钥错”与“密文被改”，避免给
//!   攻击者提供区分信息。

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::RngExt;
use sha2::{Digest, Sha256};
use std::fmt;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use super::error::StorageError;

/// 主密钥长度（字节）。
pub const MASTER_KEY_BYTES: usize = 32;
/// ChaCha20-Poly1305 nonce 长度（字节）。
pub const NONCE_BYTES: usize = 12;
/// Poly1305 认证标签长度（字节）。
pub const TAG_BYTES: usize = 16;
/// 封装格式版本。
pub const SEAL_VERSION: u8 = 1;
/// 单个封装值的上限（含版本/nonce/tag）；超过即有界拒绝。
pub const MAX_SEALED_BYTES: usize = 1024 * 1024;
/// 明文上限（扣除封装开销）。
pub const MAX_SEAL_PLAINTEXT_BYTES: usize = MAX_SEALED_BYTES - 1 - NONCE_BYTES - TAG_BYTES;

const SUBKEY_DOMAIN: &[u8] = b"chat/storage/aead/subkey/v1";
const AAD_DOMAIN: &[u8] = b"chat/storage/aead/aad/v1";

/// 封装用途。每个用途独立子密钥与独立 AAD 前缀，互相不能解密。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SealPurpose {
    /// `drafts.protected_content`：用户消息正文。
    DraftContent,
    /// `mls_state_records.protected_value`：MLS 秘密（密钥材料、群状态记录）。
    MlsStateRecord,
    /// `store_key_check.verifier`：主密钥校验记录。
    StoreVerifier,
    /// `store_quarantine.protected_detail`：隔离诊断。
    QuarantineDetail,
    /// `messages.protected_content`：已接受消息的展示正文。
    MessageContent,
    /// 内部分页游标令牌（只在本机存储边界内使用，不是线协议 `cur1` 令牌）。
    PageCursor,
    /// `conversations.protected_metadata`：本地会话摘要（标题、预览、活动时间、本机作者）。
    ConversationMetadata,
    /// `identity_account.protected_account`：账户/设备种子、公钥与账户展示信息。
    IdentityAccount,
    /// `contacts.protected_record`：对端账户、显示名、观测指纹与联系人状态。
    ContactRecord,
    /// `mls_group_state.protected_state`：已提交 MLS 群级状态（含秘密）。
    MlsGroupState,
    /// `mls_record.protected_value`：MLS provider 记录（群状态、epoch 秘密、签名密钥）。
    MlsRecord,
    /// `attachments.protected_descriptor`：附件记录的敏感元数据（内容/密文摘要、展示名）。
    AttachmentRecord,
    /// `attachment_secrets.protected_secret`：附件的 file_id 与 FileKey（内容密钥）。
    AttachmentSecret,
    /// `attachment_transfer_progress.protected_progress`：断点续传进度（位图 + 会话/blob + 前缀摘要）。
    AttachmentTransferProgress,
    /// `plugin_kv.value`：插件私有键值（PLUGINS-004 的持久化形式）。
    PluginKv,
}

impl SealPurpose {
    /// 用途的稳定域名字节（进入子密钥派生与 AAD）。
    pub const fn domain(self) -> &'static [u8] {
        match self {
            Self::DraftContent => b"draft-content",
            Self::MlsStateRecord => b"mls-state-record",
            Self::StoreVerifier => b"store-verifier",
            Self::QuarantineDetail => b"quarantine-detail",
            Self::MessageContent => b"message-content",
            Self::PageCursor => b"page-cursor",
            Self::ConversationMetadata => b"conversation-metadata",
            Self::IdentityAccount => b"identity-account",
            Self::ContactRecord => b"contact-record",
            Self::MlsGroupState => b"mls-group-state",
            Self::MlsRecord => b"mls-record",
            Self::AttachmentRecord => b"attachment-record",
            Self::AttachmentSecret => b"attachment-secret",
            Self::AttachmentTransferProgress => b"attachment-transfer-progress",
            Self::PluginKv => b"plugin-kv",
        }
    }
}

/// 32 字节存储主密钥。禁止日志输出、禁止拷贝到普通 DTO。
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct ProtectedStorageKey([u8; MASTER_KEY_BYTES]);

impl ProtectedStorageKey {
    /// 从固定长度字节构造。
    pub fn from_bytes(bytes: [u8; MASTER_KEY_BYTES]) -> Self {
        Self(bytes)
    }

    /// 从切片构造；长度不等于 32 字节立即拒绝。
    pub fn try_from_slice(bytes: &[u8]) -> Result<Self, StorageError> {
        if bytes.len() != MASTER_KEY_BYTES {
            return Err(StorageError::InvalidInput(format!(
                "master key must be exactly {MASTER_KEY_BYTES} bytes"
            )));
        }
        let mut key = [0u8; MASTER_KEY_BYTES];
        key.copy_from_slice(bytes);
        Ok(Self(key))
    }

    /// 用系统 CSPRNG 生成新的主密钥。
    pub fn generate() -> Self {
        let mut bytes = [0u8; MASTER_KEY_BYTES];
        rand::rng().fill(&mut bytes);
        Self(bytes)
    }

    /// 生成随机盐（长度 16..=64 字节）。
    pub fn random_salt(length: usize) -> Result<Vec<u8>, StorageError> {
        if !(16..=64).contains(&length) {
            return Err(StorageError::InvalidInput(
                "salt length must be within 16..=64 bytes".to_owned(),
            ));
        }
        let mut salt = vec![0u8; length];
        rand::rng().fill(salt.as_mut_slice());
        Ok(salt)
    }

    /// 常量时间比较两个密钥（只用于测试与校验记录，业务路径不应比较密钥）。
    pub fn equals(&self, other: &Self) -> bool {
        let mut diff = 0u8;
        for index in 0..MASTER_KEY_BYTES {
            diff |= self.0[index] ^ other.0[index];
        }
        diff == 0
    }

    /// 只在子密钥派生内部使用；调用方不得复制返回值。
    pub(crate) fn expose(&self) -> &[u8; MASTER_KEY_BYTES] {
        &self.0
    }
}

impl fmt::Debug for ProtectedStorageKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProtectedStorageKey(<redacted>)")
    }
}

/// 单一用途的 AEAD 封装器。只保存派生出的子密钥，不保存主密钥。
pub struct RecordSealer {
    cipher: ChaCha20Poly1305,
    purpose: SealPurpose,
}

impl RecordSealer {
    /// 由主密钥与用途派生独立子密钥。
    pub fn new(key: &ProtectedStorageKey, purpose: SealPurpose) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(SUBKEY_DOMAIN);
        hasher.update(purpose.domain());
        hasher.update(key.expose());
        let digest = hasher.finalize();
        let mut subkey = [0u8; 32];
        subkey.copy_from_slice(digest.as_slice());
        let cipher = ChaCha20Poly1305::new(&Key::from(subkey));
        subkey.zeroize();
        Self { cipher, purpose }
    }

    /// 需要同时认证的字段（行身份）。顺序与内容都必须稳定。
    fn aad(&self, fields: &[&[u8]]) -> Vec<u8> {
        let mut aad = Vec::with_capacity(64);
        aad.extend_from_slice(AAD_DOMAIN);
        aad.extend_from_slice(&(self.purpose.domain().len() as u32).to_be_bytes());
        aad.extend_from_slice(self.purpose.domain());
        for field in fields {
            aad.extend_from_slice(&(field.len() as u32).to_be_bytes());
            aad.extend_from_slice(field);
        }
        aad
    }

    /// 加密明文并返回 `version || nonce || ciphertext||tag`。
    pub fn seal(&self, fields: &[&[u8]], plaintext: &[u8]) -> Result<Vec<u8>, StorageError> {
        if plaintext.len() > MAX_SEAL_PLAINTEXT_BYTES {
            return Err(StorageError::InvalidInput(
                "plaintext exceeds the sealed value limit".to_owned(),
            ));
        }
        let mut nonce_bytes = [0u8; NONCE_BYTES];
        rand::rng().fill(&mut nonce_bytes);
        let nonce = Nonce::from(nonce_bytes);
        let aad = self.aad(fields);
        let ciphertext = self
            .cipher
            .encrypt(&nonce, Payload { msg: plaintext, aad: &aad })
            .map_err(|_| StorageError::Failure("AEAD encryption failed".to_owned()))?;
        let mut sealed = Vec::with_capacity(1 + NONCE_BYTES + ciphertext.len());
        sealed.push(SEAL_VERSION);
        sealed.extend_from_slice(&nonce_bytes);
        sealed.extend_from_slice(&ciphertext);
        Ok(sealed)
    }

    /// 认证并解密；任何失败都返回 `STATE_CORRUPTED`。
    pub fn open(
        &self,
        fields: &[&[u8]],
        sealed: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, StorageError> {
        let minimum = 1 + NONCE_BYTES + TAG_BYTES;
        if sealed.len() < minimum || sealed.len() > MAX_SEALED_BYTES {
            return Err(StorageError::StateCorrupted(
                "sealed value has an invalid length".to_owned(),
            ));
        }
        if sealed[0] != SEAL_VERSION {
            return Err(StorageError::StateCorrupted(
                "unsupported sealed value version".to_owned(),
            ));
        }
        let mut nonce_bytes = [0u8; NONCE_BYTES];
        nonce_bytes.copy_from_slice(&sealed[1..1 + NONCE_BYTES]);
        let nonce = Nonce::from(nonce_bytes);
        let aad = self.aad(fields);
        let plaintext = self
            .cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &sealed[1 + NONCE_BYTES..],
                    aad: &aad,
                },
            )
            .map_err(|_| {
                StorageError::StateCorrupted("sealed value authentication failed".to_owned())
            })?;
        Ok(Zeroizing::new(plaintext))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_key() -> ProtectedStorageKey {
        ProtectedStorageKey::generate()
    }

    #[test]
    fn roundtrip_and_nonce_uniqueness() {
        let key = fresh_key();
        let sealer = RecordSealer::new(&key, SealPurpose::DraftContent);
        let first = sealer.seal(&[b"row-1"], b"\xe4\xbd\xa0\xe5\xa5\xbd").unwrap();
        let second = sealer.seal(&[b"row-1"], b"\xe4\xbd\xa0\xe5\xa5\xbd").unwrap();
        assert_ne!(first, second, "each sealed value must use a fresh nonce");
        assert_eq!(
            sealer.open(&[b"row-1"], &first).unwrap().as_slice(),
            b"\xe4\xbd\xa0\xe5\xa5\xbd".as_slice()
        );
        assert!(sealer.open(&[b"row-2"], &first).is_err(), "AAD binds the row identity");
    }

    #[test]
    fn wrong_key_purpose_and_tamper_are_rejected() {
        let key = fresh_key();
        let other = fresh_key();
        let sealer = RecordSealer::new(&key, SealPurpose::DraftContent);
        let sealed = sealer.seal(&[b"row"], b"secret").unwrap();
        let other_key = RecordSealer::new(&other, SealPurpose::DraftContent);
        assert!(other_key.open(&[b"row"], &sealed).is_err());
        let other_purpose = RecordSealer::new(&key, SealPurpose::MlsStateRecord);
        assert!(other_purpose.open(&[b"row"], &sealed).is_err());
        for index in [0usize, 1, 5, sealed.len() - 1] {
            let mut tampered = sealed.clone();
            tampered[index] ^= 0x01;
            assert!(sealer.open(&[b"row"], &tampered).is_err(), "byte {index}");
        }
        assert!(sealer.open(&[b"row"], &sealed[..sealed.len() - 1]).is_err());
    }

    #[test]
    fn key_material_is_not_debug_printed_and_bounds_are_enforced() {
        let key = fresh_key();
        assert_eq!(format!("{key:?}"), "ProtectedStorageKey(<redacted>)");
        assert!(ProtectedStorageKey::try_from_slice(&[0u8; 31]).is_err());
        assert!(ProtectedStorageKey::try_from_slice(&[0u8; 33]).is_err());
        assert!(ProtectedStorageKey::random_salt(15).is_err());
        assert!(ProtectedStorageKey::random_salt(65).is_err());
        let sealer = RecordSealer::new(&key, SealPurpose::DraftContent);
        let oversized = vec![0u8; MAX_SEAL_PLAINTEXT_BYTES + 1];
        assert!(sealer.seal(&[b"row"], &oversized).is_err());
    }
}
