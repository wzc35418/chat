//! CRYPTO-011/012：附件分块 AEAD（ChaCha20-Poly1305）。
//!
//! 每个附件使用独立随机 FileKey（32 字节）；明文按固定 256 KiB 分块，每块单独
//! 认证加密，输出格式为 version(1) || nonce(12) || ciphertext || tag(16)。
//!
//! AAD 绑定“域分隔 + file_id + index + total_chunks + plain_length + is_final +
//! format_version”的规范编码，因此交换序号、换文件、伪造最后一块、改声明长度或
//! 截断分块都会认证失败；nonce 每块随机生成（同一 key 下 100 块量级，碰撞概率可忽略）。
//!
//! 说明（P0-03 决策）：不使用流式 AEAD 或计数器 nonce，是为了让每块密文可独立校验、
//! 可乱序重传且不需要保存额外状态；完整的“块序号+总数”一致性仍由上层导出流程复核。

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::RngExt;
use std::fmt;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// 单个分块的明文上限（字节）。
pub const CHUNK_BYTES: usize = 256 * 1024;
/// 单附件明文上限（字节，25 MiB）。
pub const MAX_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;
/// 单附件允许的最大分块数（有界拒绝，防止声明超大块数）。
pub const MAX_TOTAL_CHUNKS: u32 = 4096;
/// 分块密文格式版本。
pub const CHUNK_FORMAT_VERSION: u8 = 1;
/// FileKey 长度（字节）。
pub const FILE_KEY_BYTES: usize = 32;
/// FileId 长度（字节）。
pub const FILE_ID_BYTES: usize = 16;
/// nonce 长度（字节）。
pub const NONCE_BYTES: usize = 12;
/// 认证标签长度（字节）。
pub const TAG_BYTES: usize = 16;

const AAD_DOMAIN: &[u8] = b"chat/crypto/attachment-chunk/aad/v1";
const CHUNK_OVERHEAD: usize = 1 + NONCE_BYTES + TAG_BYTES;

/// 附件分块密码学错误。每个变体对应稳定错误码。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentCryptoError {
    /// 明文超过附件上限。
    ContentTooLarge,
    /// 分块上下文不合法（序号、总数、声明长度、版本等）。
    InvalidChunkContext(&'static str),
    /// 密文被篡改、截断或与上下文不匹配。
    CipherCorrupted(&'static str),
    /// 加密原语失败（正常路径不应出现）。
    CryptoFailure(&'static str),
}

impl AttachmentCryptoError {
    /// 稳定错误码字符串。
    pub const fn code(self) -> &'static str {
        match self {
            Self::ContentTooLarge => "CONTENT_TOO_LARGE",
            Self::InvalidChunkContext(_) => "INVALID_CHUNK_CONTEXT",
            Self::CipherCorrupted(_) => "ATTACHMENT_CORRUPTED",
            Self::CryptoFailure(_) => "CRYPTO_FAILURE",
        }
    }
}

impl fmt::Display for AttachmentCryptoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContentTooLarge => formatter.write_str("attachment exceeds the 25 MiB plaintext limit"),
            Self::InvalidChunkContext(detail) => write!(formatter, "invalid chunk context: {detail}"),
            Self::CipherCorrupted(detail) => write!(formatter, "attachment chunk is not authentic: {detail}"),
            Self::CryptoFailure(detail) => write!(formatter, "attachment chunk crypto failure: {detail}"),
        }
    }
}

impl std::error::Error for AttachmentCryptoError {}

/// 结果类型。
pub type Result<T> = std::result::Result<T, AttachmentCryptoError>;

/// 单个附件的内容密钥。只在可信核心路径内存在，Drop 时清零，Debug 不输出字节。
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct FileKey([u8; FILE_KEY_BYTES]);

impl FileKey {
    /// 用系统 CSPRNG 生成新的附件密钥。
    pub fn generate() -> Self {
        let mut bytes = [0u8; FILE_KEY_BYTES];
        rand::rng().fill(&mut bytes);
        Self(bytes)
    }

    /// 从固定长度切片导入（长度不符立即拒绝）。
    pub fn try_from_slice(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != FILE_KEY_BYTES {
            return Err(AttachmentCryptoError::InvalidChunkContext(
                "file key must be exactly 32 bytes",
            ));
        }
        let mut key = [0u8; FILE_KEY_BYTES];
        key.copy_from_slice(bytes);
        Ok(Self(key))
    }

    /// 常量时间比较（只用于测试与一致性校验）。
    pub fn equals(&self, other: &Self) -> bool {
        let mut diff = 0u8;
        for index in 0..FILE_KEY_BYTES {
            diff |= self.0[index] ^ other.0[index];
        }
        diff == 0
    }

    fn cipher(&self) -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new(&Key::from(self.0))
    }
}

impl fmt::Debug for FileKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FileKey(<redacted>)")
    }
}

/// 附件文件标识：随机 16 字节，进入每块的 AAD，跨文件不可互换。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileId([u8; FILE_ID_BYTES]);

impl FileId {
    /// 用系统 CSPRNG 生成新的文件标识。
    pub fn generate() -> Self {
        let mut bytes = [0u8; FILE_ID_BYTES];
        rand::rng().fill(&mut bytes);
        Self(bytes)
    }

    /// 从固定长度切片导入。
    pub fn try_from_slice(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != FILE_ID_BYTES {
            return Err(AttachmentCryptoError::InvalidChunkContext(
                "file id must be exactly 16 bytes",
            ));
        }
        let mut id = [0u8; FILE_ID_BYTES];
        id.copy_from_slice(bytes);
        Ok(Self(id))
    }

    /// 原始字节。
    pub fn as_bytes(&self) -> &[u8; FILE_ID_BYTES] {
        &self.0
    }

    /// 小写十六进制文本（用于本地附件 ID 与缓存目录名）。
    pub fn to_hex(&self) -> String {
        let mut text = String::with_capacity(FILE_ID_BYTES * 2);
        for byte in self.0 {
            text.push_str(&format!("{byte:02x}"));
        }
        text
    }
}

/// 单块加密上下文。字段私有，is_final 由 index 与 total_chunks 推导，无法伪造。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkContext {
    file_id: FileId,
    index: u32,
    total_chunks: u32,
    plain_length: u32,
    format_version: u8,
}

impl ChunkContext {
    /// 构造并校验上下文：
    /// - total_chunks 必须在 1..=MAX_TOTAL_CHUNKS；
    /// - index 必须小于 total_chunks；
    /// - is_final 由 index + 1 == total_chunks 推导；
    /// - 非最后一块必须是满块（plain_length == CHUNK_BYTES），最后一块可 0..=CHUNK_BYTES。
    pub fn new(file_id: FileId, index: u32, total_chunks: u32, plain_length: u32) -> Result<Self> {
        let context = Self {
            file_id,
            index,
            total_chunks,
            plain_length,
            format_version: CHUNK_FORMAT_VERSION,
        };
        context.validate()?;
        Ok(context)
    }

    /// 附件文件标识。
    pub fn file_id(&self) -> FileId {
        self.file_id
    }

    /// 块序号（从 0 开始）。
    pub fn index(&self) -> u32 {
        self.index
    }

    /// 总块数。
    pub fn total_chunks(&self) -> u32 {
        self.total_chunks
    }

    /// 本块明文长度。
    pub fn plain_length(&self) -> u32 {
        self.plain_length
    }

    /// 是否最后一块。
    pub fn is_final(&self) -> bool {
        self.index + 1 == self.total_chunks
    }

    /// 分块格式版本。
    pub fn format_version(&self) -> u8 {
        self.format_version
    }

    /// 当前上下文的规范 AAD（长度前缀不需要：file_id 定长，其余定宽大端）。
    fn aad(&self) -> Vec<u8> {
        let mut aad = Vec::with_capacity(AAD_DOMAIN.len() + FILE_ID_BYTES + 13);
        aad.extend_from_slice(AAD_DOMAIN);
        aad.extend_from_slice(&self.file_id.0);
        aad.extend_from_slice(&self.index.to_be_bytes());
        aad.extend_from_slice(&self.total_chunks.to_be_bytes());
        aad.extend_from_slice(&self.plain_length.to_be_bytes());
        aad.push(u8::from(self.is_final()));
        aad.push(self.format_version);
        aad
    }

    fn validate(&self) -> Result<()> {
        if self.format_version != CHUNK_FORMAT_VERSION {
            return Err(AttachmentCryptoError::InvalidChunkContext(
                "unsupported chunk format version",
            ));
        }
        if self.total_chunks == 0 || self.total_chunks > MAX_TOTAL_CHUNKS {
            return Err(AttachmentCryptoError::InvalidChunkContext(
                "total chunk count is outside the supported bounds",
            ));
        }
        if self.index >= self.total_chunks {
            return Err(AttachmentCryptoError::InvalidChunkContext(
                "chunk index is out of range",
            ));
        }
        if self.is_final() {
            if self.plain_length as usize > CHUNK_BYTES {
                return Err(AttachmentCryptoError::InvalidChunkContext(
                    "final chunk exceeds the chunk size limit",
                ));
            }
        } else if self.plain_length as usize != CHUNK_BYTES {
            return Err(AttachmentCryptoError::InvalidChunkContext(
                "non-final chunk must be a full chunk",
            ));
        }
        Ok(())
    }
}

/// 一块认证密文：version(1) || nonce(12) || ciphertext || tag(16)。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CipherChunk {
    bytes: Vec<u8>,
}

impl CipherChunk {
    /// 从磁盘/网络读到的原始字节构造，并做有界格式校验。
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() < CHUNK_OVERHEAD || bytes.len() > CHUNK_OVERHEAD + CHUNK_BYTES {
            return Err(AttachmentCryptoError::CipherCorrupted(
                "cipher chunk length is outside the supported bounds",
            ));
        }
        if bytes[0] != CHUNK_FORMAT_VERSION {
            return Err(AttachmentCryptoError::CipherCorrupted(
                "unsupported cipher chunk version",
            ));
        }
        Ok(Self { bytes })
    }

    /// 原始字节（含版本、nonce 与认证标签）。
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// 消耗并返回原始字节。
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// 加密一块附件明文。
pub fn encrypt_attachment_chunk(
    key: &FileKey,
    context: &ChunkContext,
    plaintext: &[u8],
) -> Result<CipherChunk> {
    context.validate()?;
    if plaintext.len() != context.plain_length as usize {
        return Err(AttachmentCryptoError::InvalidChunkContext(
            "plaintext length does not match the declared chunk length",
        ));
    }
    let mut nonce_bytes = [0u8; NONCE_BYTES];
    rand::rng().fill(&mut nonce_bytes);
    let nonce = Nonce::from(nonce_bytes);
    let aad = context.aad();
    let ciphertext = key
        .cipher()
        .encrypt(&nonce, Payload { msg: plaintext, aad: &aad })
        .map_err(|_| AttachmentCryptoError::CryptoFailure("chunk encryption failed"))?;
    let mut bytes = Vec::with_capacity(CHUNK_OVERHEAD + ciphertext.len());
    bytes.push(CHUNK_FORMAT_VERSION);
    bytes.extend_from_slice(&nonce_bytes);
    bytes.extend_from_slice(&ciphertext);
    Ok(CipherChunk { bytes })
}

/// 认证并解密一块附件密文；任何失败都显式返回错误，绝不返回未认证字节。
pub fn decrypt_attachment_chunk(
    key: &FileKey,
    context: &ChunkContext,
    chunk: &CipherChunk,
) -> Result<Zeroizing<Vec<u8>>> {
    context.validate()?;
    let bytes = chunk.as_bytes();
    let expected_length = CHUNK_OVERHEAD + context.plain_length as usize;
    if bytes.len() != expected_length {
        return Err(AttachmentCryptoError::CipherCorrupted(
            "chunk length does not match the declared context",
        ));
    }
    let mut nonce_bytes = [0u8; NONCE_BYTES];
    nonce_bytes.copy_from_slice(&bytes[1..1 + NONCE_BYTES]);
    let nonce = Nonce::from(nonce_bytes);
    let aad = context.aad();
    let plaintext = key
        .cipher()
        .decrypt(
            &nonce,
            Payload {
                msg: &bytes[1 + NONCE_BYTES..],
                aad: &aad,
            },
        )
        .map_err(|_| {
            AttachmentCryptoError::CipherCorrupted("chunk authentication failed")
        })?;
    Ok(Zeroizing::new(plaintext))
}

/// 给定明文大小计算分块数（0 字节文件按 1 个空块处理）。
pub fn chunk_count(plain_size: u64) -> Result<u32> {
    if plain_size > MAX_ATTACHMENT_BYTES {
        return Err(AttachmentCryptoError::ContentTooLarge);
    }
    if plain_size == 0 {
        return Ok(1);
    }
    let chunks = plain_size.div_ceil(CHUNK_BYTES as u64);
    let chunks = u32::try_from(chunks).map_err(|_| AttachmentCryptoError::ContentTooLarge)?;
    if chunks > MAX_TOTAL_CHUNKS {
        return Err(AttachmentCryptoError::ContentTooLarge);
    }
    Ok(chunks)
}

/// 给定明文大小与块序号计算该块明文长度。
pub fn chunk_plain_length(plain_size: u64, index: u32) -> Result<u32> {
    let total = chunk_count(plain_size)?;
    if index >= total {
        return Err(AttachmentCryptoError::InvalidChunkContext(
            "chunk index is out of range",
        ));
    }
    let start = u64::from(index) * CHUNK_BYTES as u64;
    let remaining = plain_size.saturating_sub(start);
    let length = remaining.min(CHUNK_BYTES as u64);
    u32::try_from(length).map_err(|_| AttachmentCryptoError::ContentTooLarge)
}

/// 按明文大小与块序号构造上下文（is_final 自动推导）。
pub fn chunk_context_for(
    file_id: FileId,
    plain_size: u64,
    index: u32,
) -> Result<ChunkContext> {
    let total = chunk_count(plain_size)?;
    let plain_length = chunk_plain_length(plain_size, index)?;
    ChunkContext::new(file_id, index, total, plain_length)
}
