//! E2EE 附件 SecretDescriptor 的编解码（版本字节 + 密文 + 摘要）。
//!
//! 用途：把附件的 FileKey（32 字节）封装成一个可以放进 E2EE 内层事件、或作为
//! 受保护本地记录传给他人的不透明描述符。格式（v2，当前写入格式）：
//!
//! ```text
//! version(2) || derivation(1) || nonce(12) || ciphertext(32) || tag(16) || digest(32)
//! ```
//!
//! legacy 格式（v1，P0 占位派生时代写入的记录）仍然可读：
//!
//! ```text
//! version(1) || nonce(12) || ciphertext(32) || tag(16) || digest(32)
//! ```
//!
//! - 密文用调用方给的包装密钥（wrapping key）做 ChaCha20-Poly1305。**v2 的包装密钥
//!   必须来自 MLS exporter**（见 [crate::exporter]：label = "chat attachment file key"，
//!   context = SHA-256(attachment_id ‖ 会话 ‖ epoch)）；v1 记录保留 legacy 标记，
//!   用旧的 P0 占位派生打开（老数据可读，但不再产生新的 v1 记录）；
//! - AAD 绑定 attachment_id，把描述符换到另一个附件上会认证失败；
//! - 末尾摘要（SHA-256，域分隔）覆盖整段正文（含版本/派生标记），在 AEAD 之前校验；
//!   它不是替代认证，真正的机密性/完整性仍由 AEAD 提供；
//! - 打开失败一律返回 ATTACHMENT_CORRUPTED，不区分"密钥错"和"被篡改"，也不返回
//!   任何明文；成功时返回的 FileKey 在 Drop 时清零。
//!
//! 本模块是纯密码函数：不读写磁盘、不接触存储事务。

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::RngExt;
use sha2::{Digest, Sha256};
use std::fmt;

use crate::attachment::AttachmentCryptoError;
use crate::attachment::{FILE_KEY_BYTES, FileKey, NONCE_BYTES, TAG_BYTES};

/// legacy（v1）描述符版本号：P0 占位派生时代写入的记录。
pub const SECRET_DESCRIPTOR_VERSION_V1_LEGACY: u8 = 1;
/// 当前（v2）描述符版本号：包装密钥来自 MLS exporter。
pub const SECRET_DESCRIPTOR_VERSION: u8 = 2;
/// 包装密钥长度（字节）。
pub const WRAPPING_KEY_BYTES: usize = 32;
/// v2 正文长度：version + derivation + nonce + ciphertext + tag。
pub const SECRET_DESCRIPTOR_BODY_BYTES: usize = 2 + NONCE_BYTES + FILE_KEY_BYTES + TAG_BYTES;
/// v2 描述符总长度：正文 + 摘要。
pub const SECRET_DESCRIPTOR_BYTES: usize = SECRET_DESCRIPTOR_BODY_BYTES + 32;
/// legacy（v1）正文长度：version + nonce + ciphertext + tag。
pub const LEGACY_SECRET_DESCRIPTOR_BODY_BYTES: usize = 1 + NONCE_BYTES + FILE_KEY_BYTES + TAG_BYTES;
/// legacy（v1）描述符总长度：正文 + 摘要。
pub const LEGACY_SECRET_DESCRIPTOR_BYTES: usize = LEGACY_SECRET_DESCRIPTOR_BODY_BYTES + 32;
/// 允许解析的描述符上限（有界拒绝）。
pub const MAX_SECRET_DESCRIPTOR_BYTES: usize = 256;

/// v2 里 derivation 字段的取值：包装密钥来自 MLS exporter。
const DERIVATION_EXPORTER: u8 = 1;

const AAD_DOMAIN: &[u8] = b"chat/crypto/secret-descriptor/v1";
const DIGEST_DOMAIN: &[u8] = b"chat/crypto/secret-descriptor/digest/v1";

/// 包装密钥的派生来源（写进 v2 描述符；v1 记录隐含 legacy）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescriptorKeyDerivation {
    /// 来自 MLS exporter（当前写入格式）。
    Exporter,
    /// legacy P0 占位派生（只用于读取老记录，不再产生）。
    LegacyP0,
}

impl DescriptorKeyDerivation {
    /// 稳定编码文本。
    pub const fn code(self) -> &'static str {
        match self {
            Self::Exporter => "exporter",
            Self::LegacyP0 => "legacy_p0",
        }
    }
}

/// 不透明 SecretDescriptor。字节原样可放进 E2EE 事件；不含明文 FileKey。
#[derive(Clone, PartialEq, Eq)]
pub struct SecretDescriptor {
    bytes: Vec<u8>,
}

impl SecretDescriptor {
    /// 从原始字节解析并做格式/长度校验（摘要与认证在 open 时校验）。
    ///
    /// 同时接受 v2（当前）与 v1（legacy）两种固定长度格式。
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, AttachmentCryptoError> {
        if bytes.is_empty() || bytes.len() > MAX_SECRET_DESCRIPTOR_BYTES {
            return Err(AttachmentCryptoError::CipherCorrupted(
                "secret descriptor length is outside the supported bounds",
            ));
        }
        match bytes[0] {
            SECRET_DESCRIPTOR_VERSION if bytes.len() == SECRET_DESCRIPTOR_BYTES => {
                if bytes[1] != DERIVATION_EXPORTER {
                    return Err(AttachmentCryptoError::CipherCorrupted(
                        "secret descriptor has an unsupported key derivation",
                    ));
                }
            }
            SECRET_DESCRIPTOR_VERSION_V1_LEGACY if bytes.len() == LEGACY_SECRET_DESCRIPTOR_BYTES => {}
            _ => {
                return Err(AttachmentCryptoError::CipherCorrupted(
                    "secret descriptor has an unsupported version or length",
                ));
            }
        }
        Ok(Self { bytes })
    }

    /// 原始字节。
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// 消耗并返回原始字节。
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// 描述符格式版本（1 = legacy，2 = exporter）。
    pub fn version(&self) -> u8 {
        self.bytes[0]
    }

    /// 包装密钥的派生来源（v1 恒为 [DescriptorKeyDerivation::LegacyP0]）。
    pub fn derivation(&self) -> DescriptorKeyDerivation {
        match self.version() {
            SECRET_DESCRIPTOR_VERSION_V1_LEGACY => DescriptorKeyDerivation::LegacyP0,
            _ => DescriptorKeyDerivation::Exporter,
        }
    }

    /// 是否为 legacy（P0 占位派生）记录。
    pub fn is_legacy(&self) -> bool {
        self.derivation() == DescriptorKeyDerivation::LegacyP0
    }

    /// 正文长度（不含末尾摘要）。
    fn body_bytes(&self) -> usize {
        self.bytes.len() - 32
    }

    /// 描述符正文摘要（末尾 32 字节）。
    pub fn digest(&self) -> [u8; 32] {
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&self.bytes[self.body_bytes()..]);
        digest
    }
}

impl fmt::Debug for SecretDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecretDescriptor")
            .field("version", &self.version())
            .field("derivation", &self.derivation().code())
            .field("digest", &hex(&self.digest()))
            .finish_non_exhaustive()
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

fn aad(attachment_id: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(AAD_DOMAIN.len() + 4 + attachment_id.len());
    aad.extend_from_slice(AAD_DOMAIN);
    aad.extend_from_slice(&(attachment_id.len() as u32).to_be_bytes());
    aad.extend_from_slice(attachment_id.as_bytes());
    aad
}

fn body_digest(body: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(DIGEST_DOMAIN);
    hasher.update((body.len() as u32).to_be_bytes());
    hasher.update(body);
    let digest = hasher.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(digest.as_slice());
    bytes
}

fn seal_with_derivation(
    wrapping_key: &[u8; WRAPPING_KEY_BYTES],
    attachment_id: &str,
    file_key: &FileKey,
    derivation: DescriptorKeyDerivation,
) -> Result<SecretDescriptor, AttachmentCryptoError> {
    if attachment_id.is_empty() || attachment_id.len() > 128 {
        return Err(AttachmentCryptoError::InvalidChunkContext(
            "secret descriptor attachment ID must be 1..=128 bytes",
        ));
    }
    let mut nonce_bytes = [0u8; NONCE_BYTES];
    rand::rng().fill(&mut nonce_bytes);
    let nonce = Nonce::from(nonce_bytes);
    let plaintext = file_key.copy_bytes();
    let cipher = ChaCha20Poly1305::new(&Key::from(*wrapping_key));
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext.as_slice(),
                aad: &aad(attachment_id),
            },
        )
        .map_err(|_| AttachmentCryptoError::CryptoFailure("secret descriptor encryption failed"))?;
    let body_bytes = match derivation {
        DescriptorKeyDerivation::Exporter => SECRET_DESCRIPTOR_BODY_BYTES,
        DescriptorKeyDerivation::LegacyP0 => LEGACY_SECRET_DESCRIPTOR_BODY_BYTES,
    };
    let mut body = Vec::with_capacity(body_bytes);
    match derivation {
        DescriptorKeyDerivation::Exporter => {
            body.push(SECRET_DESCRIPTOR_VERSION);
            body.push(DERIVATION_EXPORTER);
        }
        DescriptorKeyDerivation::LegacyP0 => body.push(SECRET_DESCRIPTOR_VERSION_V1_LEGACY),
    }
    body.extend_from_slice(&nonce_bytes);
    body.extend_from_slice(&ciphertext);
    if body.len() != body_bytes {
        return Err(AttachmentCryptoError::CryptoFailure(
            "secret descriptor body has an unexpected length",
        ));
    }
    let mut bytes = Vec::with_capacity(body_bytes + 32);
    bytes.extend_from_slice(&body);
    bytes.extend_from_slice(&body_digest(&body));
    Ok(SecretDescriptor { bytes })
}

/// 用 **exporter 派生**的包装密钥把 FileKey 封装成 v2 SecretDescriptor（当前写入路径）。
pub fn seal_secret_descriptor(
    wrapping_key: &[u8; WRAPPING_KEY_BYTES],
    attachment_id: &str,
    file_key: &FileKey,
) -> Result<SecretDescriptor, AttachmentCryptoError> {
    seal_with_derivation(
        wrapping_key,
        attachment_id,
        file_key,
        DescriptorKeyDerivation::Exporter,
    )
}

/// 用 legacy P0 占位派生的包装密钥封装成 v1 描述符。
///
/// 只用于迁移/兼容测试：新记录一律用 [seal_secret_descriptor]（exporter）。
pub fn seal_secret_descriptor_legacy(
    wrapping_key: &[u8; WRAPPING_KEY_BYTES],
    attachment_id: &str,
    file_key: &FileKey,
) -> Result<SecretDescriptor, AttachmentCryptoError> {
    seal_with_derivation(
        wrapping_key,
        attachment_id,
        file_key,
        DescriptorKeyDerivation::LegacyP0,
    )
}

/// 校验摘要并用包装密钥解出 FileKey；v1（legacy）与 v2（exporter）都可读。
///
/// 任何失败都返回 ATTACHMENT_CORRUPTED；调用方可用 [SecretDescriptor::derivation]
/// 判断这条记录是 legacy 还是 exporter 派生。
pub fn open_secret_descriptor(
    wrapping_key: &[u8; WRAPPING_KEY_BYTES],
    attachment_id: &str,
    descriptor: &SecretDescriptor,
) -> Result<FileKey, AttachmentCryptoError> {
    if attachment_id.is_empty() || attachment_id.len() > 128 {
        return Err(AttachmentCryptoError::InvalidChunkContext(
            "secret descriptor attachment ID must be 1..=128 bytes",
        ));
    }
    let body_len = descriptor.body_bytes();
    let body = &descriptor.bytes[..body_len];
    if body_digest(body) != descriptor.digest() {
        return Err(AttachmentCryptoError::CipherCorrupted(
            "secret descriptor digest does not match its body",
        ));
    }
    let nonce_offset = match descriptor.derivation() {
        DescriptorKeyDerivation::Exporter => {
            if body[0] != SECRET_DESCRIPTOR_VERSION || body[1] != DERIVATION_EXPORTER {
                return Err(AttachmentCryptoError::CipherCorrupted(
                    "secret descriptor has an unsupported derivation",
                ));
            }
            2
        }
        DescriptorKeyDerivation::LegacyP0 => {
            if body[0] != SECRET_DESCRIPTOR_VERSION_V1_LEGACY {
                return Err(AttachmentCryptoError::CipherCorrupted(
                    "secret descriptor has an unsupported version",
                ));
            }
            1
        }
    };
    let mut nonce_bytes = [0u8; NONCE_BYTES];
    nonce_bytes.copy_from_slice(&body[nonce_offset..nonce_offset + NONCE_BYTES]);
    let nonce = Nonce::from(nonce_bytes);
    let cipher = ChaCha20Poly1305::new(&Key::from(*wrapping_key));
    let plaintext = cipher
        .decrypt(
            &nonce,
            Payload {
                msg: &body[nonce_offset + NONCE_BYTES..],
                aad: &aad(attachment_id),
            },
        )
        .map_err(|_| {
            AttachmentCryptoError::CipherCorrupted("secret descriptor authentication failed")
        })?;
    FileKey::try_from_slice(plaintext.as_slice())
}
