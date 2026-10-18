//! 附件本地导入/导出（ATTACHMENTS-001/002/004/006/007 的本地部分，不做网络）。
//!
//! 本轮交付的边界：
//! - 只做"本地加密缓存 + 本地导出"：不发起上传/下载，不写节点，不伪造传输成功；
//! - 每块明文用 CRYPTO-011 的 FileKey 独立认证加密，缓存目录里只有密文块；
//! - FileKey 与明文摘要只留在本进程内存与（未来的）受保护本地记录中，不进入公开描述符；
//! - 记录（附件 ID、状态、大小/块数/摘要/展示名）可落到正式加密存储的 attachments 表
//!   （[PersistedAttachmentStore]）：重启后仍可查询；但 FileKey 只在进程内，重启后没有
//!   密钥，导出必须显式失败（ATTACHMENT_KEY_UNAVAILABLE），绝不伪造“可导出”。
//!
//! 已知限制（详见 docs/implementation/attachments.md）：
//! - 未知大小流按 fail-closed 显式拒绝：分块 AAD 必须绑定 total_chunks，无法在不知道
//!   总数时安全导入，因此本实现要求来源声明大小（本地文件用 metadata 大小）；
//! - 上传/下载（ATTACHMENTS-003/005）、含 FileKey 的 E2EE SecretDescriptor、
//!   取消导出（EXPORT_CANCELLED）与 FileKey 的持久化保护（需要 PLATFORM/CRYPTO 设计）未实现。

use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use chat_crypto::attachment::{
    AttachmentCryptoError, CHUNK_BYTES, CipherChunk, FileId, FileKey, MAX_ATTACHMENT_BYTES,
    chunk_context_for, chunk_count, chunk_plain_length, decrypt_attachment_chunk,
    encrypt_attachment_chunk,
};
use chat_crypto::secret_descriptor::{SecretDescriptor, seal_secret_descriptor};
use chat_storage::formal::{
    AttachmentProgress as StoredTransferProgress,
    AttachmentProgressInput as PersistedProgressInput, AttachmentRecordInput as PersistedAttachmentInput,
    AttachmentSecretInput as PersistedAttachmentSecretInput, StorageError, Store as FormalStore,
};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

/// 正式存储里的附件记录投影（密封元数据已解密，**不含 FileKey**）。
pub use chat_storage::formal::AttachmentRecord as StoredAttachmentRecord;

/// 当前时间（Unix 毫秒，饱和到 i64）。
fn now_ms() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

/// 单附件明文上限（字节，25 MiB），与 CRYPTO-011 保持一致。
pub const MAX_ATTACHMENT_BYTES_LIMIT: u64 = MAX_ATTACHMENT_BYTES;
/// 展示文件名上限（字节）。
pub const MAX_DISPLAY_NAME_BYTES: usize = 200;
/// 附件 ID 前缀（本地标识，不是 PROTOCOL-001 的冻结 ID 类型）。
pub const ATTACHMENT_ID_PREFIX: &str = "att-";

/// 附件本地状态（与 attachments 表的 state 取值同名）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentState {
    /// 正在导入（本实现不在该状态对外可见时返回）。
    Staging,
    /// 本地导入完成、尚未上传。
    Imported,
    /// 正在上传（本地无网络实现，仅状态机占位）。
    Uploading,
    /// 已上传且可下载（本地无网络实现，仅状态机占位）。
    Available,
    /// 正在下载（本地无网络实现，仅状态机占位）。
    Downloading,
    /// 导入失败（失败记录不保留缓存）。
    Failed,
    /// 已请求取消，本地缓存与进度保留。
    Cancelled,
}

impl AttachmentState {
    /// 稳定状态码（与 schema 的 attachments.state 小写取值一致）。
    pub const fn code(self) -> &'static str {
        match self {
            Self::Staging => "staging",
            Self::Imported => "imported",
            Self::Uploading => "uploading",
            Self::Available => "available",
            Self::Downloading => "downloading",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// 由 schema 状态码还原（未知码返回 None，不做猜测映射）。
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "staging" => Some(Self::Staging),
            "imported" => Some(Self::Imported),
            "uploading" => Some(Self::Uploading),
            "available" => Some(Self::Available),
            "downloading" => Some(Self::Downloading),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// 附件层稳定错误码。安全校验失败必须显式失败，绝不降级继续。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttachmentError {
    /// 明文超过 25 MiB 上限。
    ContentTooLarge,
    /// 本地文件读写失败（不回显路径，避免把目录结构写进 UI/日志）。
    FileIoFailure,
    /// 平台文件令牌无效（不是普通文件、大小不可信等）。
    InvalidFileToken(&'static str),
    /// 展示文件名不合法（空、过长、含控制字符或路径分隔符）。
    InvalidDisplayName,
    /// 附件不存在（含非法 ID 形式）。
    AttachmentNotFound,
    /// 附件当前状态不允许该操作。
    AttachmentNotReady(&'static str),
    /// 已持久化的密文/摘要无法证明一致。
    AttachmentCorrupted(&'static str),
    /// 传输已经完成，拒绝取消。
    TransferAlreadyComplete,
    /// 分块上下文不合法。
    InvalidChunkContext(&'static str),
    /// 加密原语失败（正常路径不应出现）。
    CryptoFailure(&'static str),
    /// 进程内状态被 poison 等内部故障。
    InternalFailure(&'static str),
    /// 正式存储层错误：保留稳定码与脱敏消息（不把 storage 类型泄漏出 core）。
    Storage {
        /// 稳定错误码（来自 `StorageError::code`）。
        code: &'static str,
        /// 脱敏错误消息（不含正文/密钥）。
        detail: String,
    },
    /// 文件内容密钥只在进程内：该附件当前无法解密/导出。
    KeyUnavailable(&'static str),
    /// 块传输端口失败（节点不可用/配额/会话过期/块缺失等）：保留冻结码，调用方可重试。
    Transport {
        /// 稳定错误码（实现方给冻结码，例如 NODE_UNAVAILABLE）。
        code: &'static str,
        /// 脱敏错误消息（不含正文/密钥）。
        detail: String,
    },
}

impl AttachmentError {
    /// 构造传输端口错误（实现 [BlobTransport] 的适配层使用）。
    pub fn transport(code: &'static str, detail: impl Into<String>) -> Self {
        Self::Transport {
            code,
            detail: detail.into(),
        }
    }
}

impl From<StorageError> for AttachmentError {
    fn from(error: StorageError) -> Self {
        Self::Storage {
            code: error.code(),
            detail: error.to_string(),
        }
    }
}

impl AttachmentError {
    /// 稳定错误码字符串。
    pub fn code(&self) -> &'static str {
        match self {
            Self::ContentTooLarge => "CONTENT_TOO_LARGE",
            Self::FileIoFailure => "FILE_IO_FAILURE",
            Self::InvalidFileToken(_) => "INVALID_FILE_TOKEN",
            Self::InvalidDisplayName => "INVALID_DISPLAY_NAME",
            Self::AttachmentNotFound => "ATTACHMENT_NOT_FOUND",
            Self::AttachmentNotReady(_) => "ATTACHMENT_NOT_READY",
            Self::AttachmentCorrupted(_) => "ATTACHMENT_CORRUPTED",
            Self::TransferAlreadyComplete => "TRANSFER_ALREADY_COMPLETE",
            Self::InvalidChunkContext(_) => "INVALID_CHUNK_CONTEXT",
            Self::CryptoFailure(_) => "CRYPTO_FAILURE",
            Self::InternalFailure(_) => "INTERNAL_FAILURE",
            Self::Storage { code, .. } => code,
            Self::KeyUnavailable(_) => "ATTACHMENT_KEY_UNAVAILABLE",
            Self::Transport { code, .. } => code,
        }
    }
}

impl fmt::Display for AttachmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContentTooLarge => formatter.write_str("attachment exceeds the 25 MiB plaintext limit"),
            Self::FileIoFailure => formatter.write_str("attachment file operation failed"),
            Self::InvalidFileToken(detail) => write!(formatter, "invalid file token: {detail}"),
            Self::InvalidDisplayName => formatter.write_str("invalid display name"),
            Self::AttachmentNotFound => formatter.write_str("attachment not found"),
            Self::AttachmentNotReady(detail) => write!(formatter, "attachment is not ready: {detail}"),
            Self::AttachmentCorrupted(detail) => write!(formatter, "attachment content is corrupted: {detail}"),
            Self::TransferAlreadyComplete => formatter.write_str("transfer already completed"),
            Self::InvalidChunkContext(detail) => write!(formatter, "invalid chunk context: {detail}"),
            Self::CryptoFailure(detail) => write!(formatter, "attachment crypto failure: {detail}"),
            Self::InternalFailure(detail) => write!(formatter, "internal failure: {detail}"),
            Self::Storage { code, detail } => {
                write!(formatter, "attachment storage failure {code}: {detail}")
            }
            Self::KeyUnavailable(detail) => {
                write!(formatter, "attachment key is unavailable: {detail}")
            }
            Self::Transport { code, detail } => {
                write!(formatter, "attachment transport failure {code}: {detail}")
            }
        }
    }
}

impl std::error::Error for AttachmentError {}

impl From<AttachmentCryptoError> for AttachmentError {
    fn from(error: AttachmentCryptoError) -> Self {
        match error {
            AttachmentCryptoError::ContentTooLarge => Self::ContentTooLarge,
            AttachmentCryptoError::InvalidChunkContext(detail) => Self::InvalidChunkContext(detail),
            AttachmentCryptoError::CipherCorrupted(detail) => Self::AttachmentCorrupted(detail),
            AttachmentCryptoError::CryptoFailure(detail) => Self::CryptoFailure(detail),
        }
    }
}

/// 结果类型。
pub type AttachmentResult<T> = std::result::Result<T, AttachmentError>;

/// 平台文件令牌：代表用户授权的一次受控访问。P0 本地形态只携带路径，
/// 调用方不得把任意路径直接传进内核，也不得把它写进日志或普通 DTO。
pub struct FileToken {
    path: PathBuf,
}

impl FileToken {
    /// 由平台选择器给出的本地路径构造（P0 本地形态）。
    pub fn from_local_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// 仅供内核内部读取路径。
    fn path(&self) -> &Path {
        &self.path
    }
}

/// 已完成本地导入的附件公开描述符。
///
/// 只含公开字段（ID、大小、块参数、明文摘要、展示文件名），不含 FileKey 与明文。
/// 注意：明文摘要按 docs/implementation/TYPES.md 只允许进入 E2EE 内层事件与本地记录，
/// 不得写进节点公开清单或普通日志；需要对外最小化展示时用 [AttachmentManifest]。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentDescriptor {
    /// 本地附件 ID（att- 前缀 + 32 位小写 hex，非 PROTOCOL-001 冻结类型）。
    pub attachment_id: String,
    /// 明文总字节数。
    pub plain_size: u64,
    /// 分块大小（字节）。
    pub chunk_size: u32,
    /// 分块总数。
    pub chunk_count: u32,
    /// 明文 SHA-256（小写 hex），只用于本地导出校验与 E2EE 内层。
    pub content_digest: String,
    /// 文件名展示值（只作展示，绝不参与路径拼接）。
    pub display_name: String,
}

/// 公开清单（TYPES.md 的 PublicBlobDescriptor/CipherManifest）：只有密文块数/大小/摘要。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentManifest {
    /// 本地附件 ID。
    pub attachment_id: String,
    /// 明文总字节数。
    pub plain_size: u64,
    /// 分块大小（字节）。
    pub chunk_size: u32,
    /// 分块总数。
    pub chunk_count: u32,
    /// 密文总字节数。
    pub ciphertext_bytes: u64,
    /// 密文 SHA-256（小写 hex）；不含明文摘要、不含 FileKey。
    pub ciphertext_digest: String,
}

/// 附件状态快照（ATTACHMENTS-002）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentStatus {
    /// 本地附件 ID。
    pub attachment_id: String,
    /// 当前状态。
    pub state: AttachmentState,
    /// 明文总字节数。
    pub plain_size: u64,
    /// 分块总数。
    pub chunk_count: u32,
    /// 本地缓存中实际存在的块数（0..=chunk_count）。
    pub available_chunks: u32,
    /// 已完成上传/下载的块数；本地无网络实现时恒为 0。
    pub transferred_chunks: u32,
    /// 是否已请求取消。
    pub cancel_requested: bool,
    /// 失败原因码（成功路径为 None）。
    pub failure: Option<&'static str>,
}

/// 导出回执（ATTACHMENTS-006）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportReceipt {
    /// 本地附件 ID。
    pub attachment_id: String,
    /// 实际写出的明文字节数。
    pub written_bytes: u64,
    /// 导出的明文 SHA-256（小写 hex）。
    pub content_digest: String,
}

/// 缓存的机密材料（file_id + FileKey 副本）。只在“写入受保护存储”的边界使用：
/// 调用方必须立即交给加密存储并显式清零，不得写日志/DTO。
pub struct AttachmentSecretMaterial {
    /// 附件文件标识（进入每块 AAD）。
    pub file_id: [u8; 16],
    /// 文件内容密钥副本（用完显式清零）。
    pub file_key: [u8; 32],
}

impl fmt::Debug for AttachmentSecretMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttachmentSecretMaterial")
            .field("file_id", &hex(&self.file_id))
            .field("file_key", &"<redacted>")
            .finish()
    }
}

/// 可重试的传输类错误：保留断点续传进度；其余错误按终态清理进度。
fn is_retryable_transfer_error(error: &AttachmentError) -> bool {
    matches!(
        error,
        AttachmentError::Transport { code, .. }
            if matches!(*code, "NODE_UNAVAILABLE" | "NETWORK_UNAVAILABLE" | "UPLOAD_EXPIRED")
    )
}

/// 64 位小写 hex → 32 字节摘要（失败按损坏处理）。
fn digest_from_hex(value: &str) -> AttachmentResult<[u8; 32]> {
    if value.len() != 64 {
        return Err(AttachmentError::AttachmentCorrupted(
            "stored digest is not 64 hex characters",
        ));
    }
    let mut digest = [0u8; 32];
    for (index, chunk) in value.as_bytes().chunks(2).enumerate() {
        let text = core::str::from_utf8(chunk).map_err(|_| {
            AttachmentError::AttachmentCorrupted("stored digest is not valid hex")
        })?;
        digest[index] = u8::from_str_radix(text, 16).map_err(|_| {
            AttachmentError::AttachmentCorrupted("stored digest is not valid hex")
        })?;
    }
    Ok(digest)
}

struct AttachmentRecord {
    descriptor: AttachmentDescriptor,
    file_id: FileId,
    file_key: FileKey,
    state: AttachmentState,
    transferred_chunks: u32,
    cancel_requested: bool,
    failure: Option<&'static str>,
    ciphertext_bytes: u64,
    ciphertext_digest: [u8; 32],
}

/// 本地附件缓存与状态机。
///
/// 所有方法按单个 Mutex 串行执行（本机导入/导出不是并发热点）；缓存目录布局为
/// root/att-<hex>/chunk-<index>.bin，只含分块密文。
pub struct AttachmentStore {
    root: PathBuf,
    records: Mutex<HashMap<String, AttachmentRecord>>,
}

impl fmt::Debug for AttachmentStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttachmentStore")
            .field("root", &self.root)
            .field("records", &self.records.lock().map(|records| records.len()).unwrap_or(0))
            .finish()
    }
}

impl AttachmentStore {
    /// 打开（必要时创建）本地缓存根目录。
    pub fn new(root: impl Into<PathBuf>) -> AttachmentResult<Self> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|_| AttachmentError::FileIoFailure)?;
        if !root.is_dir() {
            return Err(AttachmentError::FileIoFailure);
        }
        Ok(Self {
            root,
            records: Mutex::new(HashMap::new()),
        })
    }

    /// 缓存根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 本进程内缓存的描述符（含明文摘要，仅供可信核心路径；重启后为 None）。
    pub fn cached_descriptor(&self, attachment_id: &str) -> Option<AttachmentDescriptor> {
        let records = self.records.lock().ok()?;
        records
            .get(attachment_id)
            .map(|record| record.descriptor.clone())
    }

    /// 删除本地缓存目录与进程内记录；返回是否删掉了其中任意一项。
    ///
    /// 只清理本地缓存，不涉及已发送消息引用（引用清理由 ATTACHMENTS-008 负责）。
    pub fn delete(&self, attachment_id: &str) -> AttachmentResult<bool> {
        let directory = self.attachment_dir(attachment_id)?;
        let removed_record = {
            let mut records = self.lock_records()?;
            records.remove(attachment_id).is_some()
        };
        let removed_directory = if directory.is_dir() {
            fs::remove_dir_all(&directory).map_err(|_| AttachmentError::FileIoFailure)?;
            true
        } else {
            false
        };
        Ok(removed_record || removed_directory)
    }

    /// ATTACHMENTS-001：流式导入本地文件，逐块加密写入缓存，返回公开描述符。
    ///
    /// 注意：只接受声明了大小的普通文件；未知大小流按 fail-closed 显式拒绝
    /// （INVALID_FILE_TOKEN），因为分块 AAD 必须绑定 total_chunks。
    pub fn import_file(
        &self,
        source: &FileToken,
        display_name: &str,
    ) -> AttachmentResult<AttachmentDescriptor> {
        validate_display_name(display_name)?;
        let metadata =
            fs::metadata(source.path()).map_err(|_| AttachmentError::FileIoFailure)?;
        if !metadata.is_file() {
            return Err(AttachmentError::InvalidFileToken(
                "source is not a regular file",
            ));
        }
        if metadata.len() > MAX_ATTACHMENT_BYTES_LIMIT {
            return Err(AttachmentError::ContentTooLarge);
        }
        let file = File::open(source.path()).map_err(|_| AttachmentError::FileIoFailure)?;
        self.import_reader(file, metadata.len(), display_name)
    }

    /// 流式导入核心：读取恰好 declared_size 字节并拒绝任何大小漂移。
    fn import_reader<R: Read>(
        &self,
        mut reader: R,
        declared_size: u64,
        display_name: &str,
    ) -> AttachmentResult<AttachmentDescriptor> {
        if declared_size > MAX_ATTACHMENT_BYTES_LIMIT {
            return Err(AttachmentError::ContentTooLarge);
        }
        let total_chunks = chunk_count(declared_size)?;
        let file_id = FileId::generate();
        let file_key = FileKey::generate();
        let attachment_id = format!("{ATTACHMENT_ID_PREFIX}{}", file_id.to_hex());
        let directory = self.attachment_dir(&attachment_id)?;
        fs::create_dir_all(&directory).map_err(|_| AttachmentError::FileIoFailure)?;

        let outcome = self.write_chunks(
            &mut reader,
            declared_size,
            total_chunks,
            file_id,
            &file_key,
            &directory,
        );
        let (content_digest, ciphertext_bytes, ciphertext_digest) = match outcome {
            Ok(result) => result,
            Err(error) => {
                // 导入失败清理未引用缓存，不留半成品目录。
                let _ = fs::remove_dir_all(&directory);
                return Err(error);
            }
        };

        let descriptor = AttachmentDescriptor {
            attachment_id: attachment_id.clone(),
            plain_size: declared_size,
            chunk_size: CHUNK_BYTES as u32,
            chunk_count: total_chunks,
            content_digest: hex(&content_digest),
            display_name: display_name.to_owned(),
        };
        let record = AttachmentRecord {
            descriptor: descriptor.clone(),
            file_id,
            file_key,
            state: AttachmentState::Imported,
            transferred_chunks: 0,
            cancel_requested: false,
            failure: None,
            ciphertext_bytes,
            ciphertext_digest,
        };
        self.records
            .lock()
            .map_err(|_| AttachmentError::InternalFailure("attachment state is poisoned"))?
            .insert(attachment_id, record);
        Ok(descriptor)
    }

    fn write_chunks<R: Read>(
        &self,
        reader: &mut R,
        declared_size: u64,
        total_chunks: u32,
        file_id: FileId,
        file_key: &FileKey,
        directory: &Path,
    ) -> AttachmentResult<([u8; 32], u64, [u8; 32])> {
        let mut content_hasher = Sha256::new();
        let mut ciphertext_hasher = Sha256::new();
        let mut ciphertext_bytes = 0u64;
        let mut buffer = vec![0u8; CHUNK_BYTES];
        for index in 0..total_chunks {
            let length = chunk_plain_length(declared_size, index)? as usize;
            read_exact(reader, &mut buffer[..length])?;
            content_hasher.update(&buffer[..length]);
            let context = chunk_context_for(file_id, declared_size, index)?;
            let chunk = encrypt_attachment_chunk(file_key, &context, &buffer[..length])?;
            ciphertext_bytes += chunk.as_bytes().len() as u64;
            ciphertext_hasher.update(chunk.as_bytes());
            self.write_chunk_file(directory, index, chunk.as_bytes())?;
            buffer[..length].fill(0);
        }
        buffer.fill(0);
        // 大小漂移必须显式失败，不能静默截断或继续读。
        let mut probe = [0u8; 1];
        match reader.read(&mut probe) {
            Ok(0) => {}
            Ok(_) => {
                return Err(AttachmentError::FileIoFailure);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                return Err(AttachmentError::FileIoFailure);
            }
            Err(_) => return Err(AttachmentError::FileIoFailure),
        }
        let mut content_digest = [0u8; 32];
        content_digest.copy_from_slice(content_hasher.finalize().as_slice());
        let mut ciphertext_digest = [0u8; 32];
        ciphertext_digest.copy_from_slice(ciphertext_hasher.finalize().as_slice());
        Ok((content_digest, ciphertext_bytes, ciphertext_digest))
    }

    /// 把下载回来的分块密文原样写进本地缓存。
    ///
    /// 不做二次加密：字节与节点清单一致，因此 ciphertext_bytes/ciphertext_digest 保持有效；
    /// 只接受合法分块格式（版本/长度由 CipherChunk 校验），不把任意字节写进缓存。
    pub fn cache_downloaded_chunk(
        &self,
        attachment_id: &str,
        index: u32,
        ciphertext: &[u8],
    ) -> AttachmentResult<()> {
        let directory = self.attachment_dir(attachment_id)?;
        let chunk = CipherChunk::from_bytes(ciphertext.to_vec())?;
        fs::create_dir_all(&directory).map_err(|_| AttachmentError::FileIoFailure)?;
        self.write_chunk_file(&directory, index, chunk.as_bytes())
    }

    /// 下载成功后按缓存块重算密文摘要并把记录标成 available（缓存必须完整）。
    pub fn finish_cached_download(&self, attachment_id: &str) -> AttachmentResult<()> {
        let mut records = self.lock_records()?;
        let record = records
            .get_mut(attachment_id)
            .ok_or(AttachmentError::AttachmentNotFound)?;
        let mut hasher = Sha256::new();
        let mut bytes = 0u64;
        for index in 0..record.descriptor.chunk_count {
            let path = self.chunk_path(attachment_id, index)?;
            let data = fs::read(&path)
                .map_err(|_| AttachmentError::AttachmentCorrupted("cached chunk is missing"))?;
            bytes = bytes.saturating_add(data.len() as u64);
            hasher.update(&data);
        }
        let mut digest = [0u8; 32];
        digest.copy_from_slice(hasher.finalize().as_slice());
        if digest != record.ciphertext_digest {
            return Err(AttachmentError::AttachmentCorrupted(
                "cached ciphertext digest does not match the manifest",
            ));
        }
        record.ciphertext_bytes = bytes;
        record.state = AttachmentState::Available;
        record.transferred_chunks = record.descriptor.chunk_count;
        record.failure = None;
        Ok(())
    }

    /// 该块是否已在本地缓存里（下载时不重复写已有块）。
    pub fn has_cached_chunk(&self, attachment_id: &str, index: u32) -> bool {
        self.chunk_path(attachment_id, index)
            .map(|path| path.is_file())
            .unwrap_or(false)
    }

    /// 删除本次下载新写入的缓存块（失败时只回滚本次写入，不动既有缓存）。
    pub fn remove_cached_chunk(&self, attachment_id: &str, index: u32) -> AttachmentResult<()> {
        let path = self.chunk_path(attachment_id, index)?;
        if path.is_file() {
            fs::remove_file(&path).map_err(|_| AttachmentError::FileIoFailure)?;
        }
        Ok(())
    }

    fn write_chunk_file(
        &self,
        directory: &Path,
        index: u32,
        bytes: &[u8],
    ) -> AttachmentResult<()> {
        let path = directory.join(chunk_file_name(index));
        let mut file = File::create(&path).map_err(|_| AttachmentError::FileIoFailure)?;
        file.write_all(bytes).map_err(|_| AttachmentError::FileIoFailure)?;
        file.sync_all().map_err(|_| AttachmentError::FileIoFailure)?;
        Ok(())
    }

    /// ATTACHMENTS-004（本地部分）：读取公开描述符。不含 FileKey 与明文。
    pub fn build_attachment_descriptor(
        &self,
        attachment_id: &str,
    ) -> AttachmentResult<AttachmentDescriptor> {
        let records = self.lock_records()?;
        let record = records
            .get(attachment_id)
            .ok_or(AttachmentError::AttachmentNotFound)?;
        match record.state {
            AttachmentState::Staging | AttachmentState::Failed => Err(
                AttachmentError::AttachmentNotReady("attachment was not imported successfully"),
            ),
            _ => Ok(record.descriptor.clone()),
        }
    }

    /// 公开清单：只有密文块数/大小/摘要，可安全进入节点清单与普通日志。
    pub fn attachment_manifest(
        &self,
        attachment_id: &str,
    ) -> AttachmentResult<AttachmentManifest> {
        let records = self.lock_records()?;
        let record = records
            .get(attachment_id)
            .ok_or(AttachmentError::AttachmentNotFound)?;
        if record.state == AttachmentState::Failed {
            return Err(AttachmentError::AttachmentNotReady(
                "attachment was not imported successfully",
            ));
        }
        Ok(AttachmentManifest {
            attachment_id: record.descriptor.attachment_id.clone(),
            plain_size: record.descriptor.plain_size,
            chunk_size: record.descriptor.chunk_size,
            chunk_count: record.descriptor.chunk_count,
            ciphertext_bytes: record.ciphertext_bytes,
            ciphertext_digest: hex(&record.ciphertext_digest),
        })
    }

    /// ATTACHMENTS-002：查询本地状态与进度。
    pub fn attachment_status(&self, attachment_id: &str) -> AttachmentResult<AttachmentStatus> {
        let records = self.lock_records()?;
        let record = records
            .get(attachment_id)
            .ok_or(AttachmentError::AttachmentNotFound)?;
        let available = self.available_chunks(attachment_id, record.descriptor.chunk_count);
        Ok(status_of(record, available))
    }

    /// ATTACHMENTS-007（本地部分）：请求取消当前传输，幂等，且保留本地缓存与进度。
    pub fn cancel_transfer(&self, attachment_id: &str) -> AttachmentResult<AttachmentStatus> {
        let mut records = self.lock_records()?;
        let record = records
            .get_mut(attachment_id)
            .ok_or(AttachmentError::AttachmentNotFound)?;
        match record.state {
            AttachmentState::Available => return Err(AttachmentError::TransferAlreadyComplete),
            AttachmentState::Cancelled | AttachmentState::Failed => {
                // 已终态：取消是幂等的，不改变状态。
            }
            AttachmentState::Staging
            | AttachmentState::Imported
            | AttachmentState::Uploading
            | AttachmentState::Downloading => {
                record.state = AttachmentState::Cancelled;
                record.cancel_requested = true;
            }
        }
        let available = self.available_chunks(attachment_id, record.descriptor.chunk_count);
        Ok(status_of(record, available))
    }

    /// ATTACHMENTS-006（本地部分）：按块认证解密并校验完整摘要后导出到目标。
    ///
    /// 先写同目录临时文件，只有全部分块认证通过、总长与明文摘要都匹配才 rename 成
    /// 目标文件；任何失败都会删除临时文件，绝不暴露未验证成品。
    pub fn export_attachment(
        &self,
        attachment_id: &str,
        destination: &FileToken,
    ) -> AttachmentResult<ExportReceipt> {
        let records = self.lock_records()?;
        let record = records
            .get(attachment_id)
            .ok_or(AttachmentError::AttachmentNotFound)?;
        match record.state {
            AttachmentState::Staging | AttachmentState::Failed => {
                return Err(AttachmentError::AttachmentNotReady(
                    "attachment was not imported successfully",
                ));
            }
            _ => {}
        }
        let destination_path = destination.path();
        if destination_path.is_dir() {
            return Err(AttachmentError::InvalidFileToken(
                "destination must name a file",
            ));
        }
        let file_name = destination_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(AttachmentError::InvalidFileToken(
                "destination must name a file",
            ))?;
        if let Some(parent) = destination_path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.is_dir()
        {
            return Err(AttachmentError::FileIoFailure);
        }
        let partial_name = format!("{file_name}.{}.export-partial", record.file_id.to_hex());
        let partial_path = destination_path.with_file_name(partial_name);

        let outcome = self.write_plaintext(attachment_id, record, &partial_path);
        match outcome {
            Ok((written, digest)) => {
                if destination_path.exists()
                    && fs::remove_file(destination_path).is_err()
                {
                    let _ = fs::remove_file(&partial_path);
                    return Err(AttachmentError::FileIoFailure);
                }
                if fs::rename(&partial_path, destination_path).is_err() {
                    let _ = fs::remove_file(&partial_path);
                    return Err(AttachmentError::FileIoFailure);
                }
                Ok(ExportReceipt {
                    attachment_id: record.descriptor.attachment_id.clone(),
                    written_bytes: written,
                    content_digest: digest,
                })
            }
            Err(error) => {
                let _ = fs::remove_file(&partial_path);
                Err(error)
            }
        }
    }


    /// 导出本进程内缓存的机密材料（file_id + FileKey），用于写入受保护存储。
    ///
    /// 返回值里的密钥是普通副本：调用方必须立即交给加密存储并显式清零，
    /// 不得写日志/DTO；本类型不持有 Zeroizing（core 不直接依赖 zeroize）。
    pub fn secret_material(
        &self,
        attachment_id: &str,
    ) -> AttachmentResult<AttachmentSecretMaterial> {
        let records = self.lock_records()?;
        let record = records
            .get(attachment_id)
            .ok_or(AttachmentError::AttachmentNotFound)?;
        let key = record.file_key.copy_bytes();
        Ok(AttachmentSecretMaterial {
            file_id: *record.file_id.as_bytes(),
            file_key: *key,
        })
    }

    /// 把“从正式存储恢复出来的”记录重新注入进程内缓存（重启后仍可导出/下载）。
    ///
    /// 只接受与 attachment_id 一致的描述符；已存在同 ID 记录时直接返回（幂等）。
    /// ciphertext_bytes 由缓存里现有块文件长度求和，缺块不在这里报错（导出/下载会显式失败）。
    pub fn restore_record(
        &self,
        descriptor: AttachmentDescriptor,
        file_id: [u8; 16],
        file_key: [u8; 32],
        ciphertext_digest: [u8; 32],
        state: AttachmentState,
    ) -> AttachmentResult<()> {
        let file_id = FileId::try_from_slice(&file_id)?;
        if descriptor.attachment_id != format!("{ATTACHMENT_ID_PREFIX}{}", file_id.to_hex()) {
            return Err(AttachmentError::AttachmentCorrupted(
                "restored file id does not match the attachment id",
            ));
        }
        let mut ciphertext_bytes = 0u64;
        for index in 0..descriptor.chunk_count {
            if let Ok(path) = self.chunk_path(&descriptor.attachment_id, index)
                && let Ok(metadata) = fs::metadata(&path)
            {
                ciphertext_bytes = ciphertext_bytes.saturating_add(metadata.len());
            }
        }
        let mut records = self.lock_records()?;
        if records.contains_key(&descriptor.attachment_id) {
            return Ok(());
        }
        let key = FileKey::try_from_slice(&file_key)?;
        records.insert(
            descriptor.attachment_id.clone(),
            AttachmentRecord {
                descriptor,
                file_id,
                file_key: key,
                state,
                transferred_chunks: 0,
                cancel_requested: state == AttachmentState::Cancelled,
                failure: None,
                ciphertext_bytes,
                ciphertext_digest,
            },
        );
        Ok(())
    }

    fn write_plaintext(
        &self,
        attachment_id: &str,
        record: &AttachmentRecord,
        partial_path: &Path,
    ) -> AttachmentResult<(u64, String)> {
        let mut file = File::create(partial_path).map_err(|_| AttachmentError::FileIoFailure)?;
        let mut hasher = Sha256::new();
        let mut written = 0u64;
        for index in 0..record.descriptor.chunk_count {
            let context = chunk_context_for(
                record.file_id,
                record.descriptor.plain_size,
                index,
            )?;
            let path = self.chunk_path(attachment_id, index)?;
            let bytes = fs::read(&path)
                .map_err(|_| AttachmentError::AttachmentCorrupted("chunk is missing"))?;
            let chunk = CipherChunk::from_bytes(bytes)?;
            let plaintext = decrypt_attachment_chunk(&record.file_key, &context, &chunk)?;
            hasher.update(plaintext.as_slice());
            file.write_all(plaintext.as_slice())
                .map_err(|_| AttachmentError::FileIoFailure)?;
            written += plaintext.len() as u64;
        }
        file.sync_all().map_err(|_| AttachmentError::FileIoFailure)?;
        drop(file);
        if written != record.descriptor.plain_size {
            return Err(AttachmentError::AttachmentCorrupted(
                "exported size does not match the descriptor",
            ));
        }
        let digest = hex(&hasher.finalize());
        if digest != record.descriptor.content_digest {
            return Err(AttachmentError::AttachmentCorrupted(
                "exported content digest does not match the descriptor",
            ));
        }
        Ok((written, digest))
    }

    fn available_chunks(&self, attachment_id: &str, total: u32) -> u32 {
        let mut available = 0u32;
        for index in 0..total {
            if let Ok(path) = self.chunk_path(attachment_id, index)
                && path.is_file()
            {
                available += 1;
            }
        }
        available
    }

    fn chunk_path(&self, attachment_id: &str, index: u32) -> AttachmentResult<PathBuf> {
        Ok(self.attachment_dir(attachment_id)?.join(chunk_file_name(index)))
    }

    fn attachment_dir(&self, attachment_id: &str) -> AttachmentResult<PathBuf> {
        // 严格校验 ID 形式，避免把任意字符串拼进路径（路径穿越防线）。
        let payload = attachment_id
            .strip_prefix(ATTACHMENT_ID_PREFIX)
            .ok_or(AttachmentError::AttachmentNotFound)?;
        if payload.len() != 32
            || !payload
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(AttachmentError::AttachmentNotFound);
        }
        Ok(self.root.join(attachment_id))
    }

    fn lock_records(&self) -> AttachmentResult<MutexGuard<'_, HashMap<String, AttachmentRecord>>> {
        self.records
            .lock()
            .map_err(|_| AttachmentError::InternalFailure("attachment state is poisoned"))
    }
}

fn status_of(record: &AttachmentRecord, available_chunks: u32) -> AttachmentStatus {
    AttachmentStatus {
        attachment_id: record.descriptor.attachment_id.clone(),
        state: record.state,
        plain_size: record.descriptor.plain_size,
        chunk_count: record.descriptor.chunk_count,
        available_chunks: available_chunks.min(record.descriptor.chunk_count),
        transferred_chunks: record.transferred_chunks.min(record.descriptor.chunk_count),
        cancel_requested: record.cancel_requested,
        failure: record.failure,
    }
}

fn chunk_file_name(index: u32) -> String {
    format!("chunk-{index:06}.bin")
}

/// read_exact 的显式包装：来源提前结束必须失败（不能当成合法短块）。
fn read_exact<R: Read>(reader: &mut R, buffer: &mut [u8]) -> AttachmentResult<()> {
    let mut filled = 0usize;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => return Err(AttachmentError::FileIoFailure),
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(AttachmentError::FileIoFailure),
        }
    }
    Ok(())
}

fn validate_display_name(name: &str) -> AttachmentResult<()> {
    if name.is_empty() || name.len() > MAX_DISPLAY_NAME_BYTES {
        return Err(AttachmentError::InvalidDisplayName);
    }
    if name.chars().any(|character| character.is_control()) {
        return Err(AttachmentError::InvalidDisplayName);
    }
    if name.chars().any(std::path::is_separator) || name == "." || name == ".." {
        return Err(AttachmentError::InvalidDisplayName);
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// 校验本地附件 ID 形式（att- + 32 位小写 hex）；不合法按“不存在”处理，避免路径拼接。
///
/// `pub(crate)`：块传输适配层（attachment_transport）用同一规则校验后派生节点 blob 标识。
pub(crate) fn validate_attachment_id(attachment_id: &str) -> AttachmentResult<()> {
    let payload = attachment_id
        .strip_prefix(ATTACHMENT_ID_PREFIX)
        .ok_or(AttachmentError::AttachmentNotFound)?;
    let valid = payload.len() == 32
        && payload
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !valid {
        return Err(AttachmentError::AttachmentNotFound);
    }
    Ok(())
}

fn status_from_stored(record: &StoredAttachmentRecord) -> AttachmentStatus {
    let state = AttachmentState::from_code(&record.state).unwrap_or(AttachmentState::Failed);
    AttachmentStatus {
        attachment_id: record.attachment_id.clone(),
        state,
        plain_size: record.plain_size,
        chunk_count: record.chunk_count,
        // 本进程没有该附件的缓存（例如重启后），可用块数只能是 0，不猜造进度。
        available_chunks: 0,
        transferred_chunks: 0,
        cancel_requested: state == AttachmentState::Cancelled,
        failure: (state == AttachmentState::Failed).then_some("attachment import failed"),
    }
}

/// 组合“本地加密缓存 + 正式加密存储记录”的附件入口。
///
/// - 记录只保存非秘密元数据（ID/状态/大小/块数/明文与密文摘要/展示名），敏感字段由
///   storage 用主密钥派生的附件用途子密钥密封；
/// - **FileKey 不持久化**：它只在 [AttachmentStore] 的进程内记录里，因此重启后
///   [Self::export_attachment] 会显式返回 ATTACHMENT_KEY_UNAVAILABLE，本类型不伪造
///   可导出状态；
/// - 记录写入失败时清理刚写入的缓存，不留“只有缓存没有记录”的半成品。
pub struct PersistedAttachmentStore {
    cache: AttachmentStore,
    store: Mutex<FormalStore>,
}

impl fmt::Debug for PersistedAttachmentStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersistedAttachmentStore")
            .field("cache_root", &self.cache.root())
            .finish_non_exhaustive()
    }
}

impl PersistedAttachmentStore {
    /// 打开附件入口：缓存根目录 + 已打开的正式加密库（调用方负责主密钥与独占锁）。
    pub fn open(cache_root: impl Into<PathBuf>, store: FormalStore) -> AttachmentResult<Self> {
        Ok(Self {
            cache: AttachmentStore::new(cache_root)?,
            store: Mutex::new(store),
        })
    }

    /// 本地缓存根目录。
    pub fn cache_root(&self) -> &Path {
        self.cache.root()
    }

    fn lock_store(&self) -> AttachmentResult<MutexGuard<'_, FormalStore>> {
        self.store.lock().map_err(|_| {
            AttachmentError::InternalFailure("attachment record store lock is poisoned")
        })
    }

    /// crate 内部：借出正式库句柄做只读查询（例如从持久身份派生节点设备凭据）。
    ///
    /// 不对外公开：库对象（含主密钥派生能力）不进入任何 DTO，也不跨越 FFI 边界。
    pub(crate) fn with_formal_store<T>(
        &self,
        query: impl FnOnce(&FormalStore) -> AttachmentResult<T>,
    ) -> AttachmentResult<T> {
        let store = self.lock_store()?;
        query(&store)
    }

    /// 导入文件：先写本地加密缓存，再把非秘密元数据写入正式存储记录。
    pub fn import_file(
        &self,
        source: &FileToken,
        display_name: &str,
    ) -> AttachmentResult<AttachmentDescriptor> {
        let descriptor = self.cache.import_file(source, display_name)?;
        let persisted = (|| -> AttachmentResult<()> {
            let manifest = self.cache.attachment_manifest(&descriptor.attachment_id)?;
            let input = PersistedAttachmentInput {
                attachment_id: descriptor.attachment_id.clone(),
                state: AttachmentState::Imported.code().to_owned(),
                plain_size: descriptor.plain_size,
                chunk_size: descriptor.chunk_size,
                chunk_count: descriptor.chunk_count,
                content_digest: descriptor.content_digest.clone(),
                ciphertext_digest: manifest.ciphertext_digest,
                display_name: descriptor.display_name.clone(),
                created_at_ms: now_ms(),
            };
            let mut material = self.cache.secret_material(&descriptor.attachment_id)?;
            let mut store = self.lock_store()?;
            store.save_attachment(input)?;
            // FileKey 与 file_id 一起封存：重启后没有进程内缓存也能导出/下载。
            let secret = PersistedAttachmentSecretInput {
                attachment_id: descriptor.attachment_id.clone(),
                file_id: material.file_id,
                file_key: material.file_key.to_vec(),
                created_at_ms: now_ms(),
            };
            let saved = store.save_attachment_secret(secret);
            material.file_key.fill(0);
            saved?;
            Ok(())
        })();
        if let Err(error) = persisted {
            // 记录写失败：清掉刚写入的缓存，避免半成品（记录才是持久真相）。
            let _ = self.cache.delete(&descriptor.attachment_id);
            return Err(error);
        }
        Ok(descriptor)
    }

    /// 读取一条持久化附件记录；不存在返回 None。
    pub fn attachment_record(
        &self,
        attachment_id: &str,
    ) -> AttachmentResult<Option<StoredAttachmentRecord>> {
        validate_attachment_id(attachment_id)?;
        let store = self.lock_store()?;
        store
            .load_attachment(attachment_id)
            .map_err(AttachmentError::from)
    }

    /// 按创建时间倒序列出持久化附件记录。
    pub fn attachment_records(
        &self,
        limit: u32,
    ) -> AttachmentResult<Vec<StoredAttachmentRecord>> {
        let store = self.lock_store()?;
        store
            .list_attachments(limit)
            .map_err(AttachmentError::from)
    }

    /// 查询附件状态：本进程有缓存时用缓存状态，否则退回持久化记录（可用块数为 0）。
    pub fn attachment_status(&self, attachment_id: &str) -> AttachmentResult<AttachmentStatus> {
        validate_attachment_id(attachment_id)?;
        match self.cache.attachment_status(attachment_id) {
            Ok(status) => Ok(status),
            Err(AttachmentError::AttachmentNotFound) => {
                let record = self
                    .attachment_record(attachment_id)?
                    .ok_or(AttachmentError::AttachmentNotFound)?;
                Ok(status_from_stored(&record))
            }
            Err(error) => Err(error),
        }
    }

    /// 取消传输：同步持久化记录状态（幂等；已完成/终态显式拒绝或保持）。
    pub fn cancel_transfer(&self, attachment_id: &str) -> AttachmentResult<AttachmentStatus> {
        validate_attachment_id(attachment_id)?;
        let cache_status = self.cache.attachment_status(attachment_id);
        if let Some(record) = self.attachment_record(attachment_id)? {
            let state = AttachmentState::from_code(&record.state).ok_or(
                AttachmentError::AttachmentCorrupted("stored attachment state is unknown"),
            )?;
            match state {
                AttachmentState::Cancelled | AttachmentState::Failed => {}
                AttachmentState::Available => return Err(AttachmentError::TransferAlreadyComplete),
                _ => {
                    let mut store = self.lock_store()?;
                    store.mark_attachment_state(attachment_id, AttachmentState::Cancelled.code())?;
                }
            }
        }
        match cache_status {
            Ok(_) => self.cache.cancel_transfer(attachment_id),
            Err(AttachmentError::AttachmentNotFound) => {
                let record = self
                    .attachment_record(attachment_id)?
                    .ok_or(AttachmentError::AttachmentNotFound)?;
                Ok(status_from_stored(&record))
            }
            Err(error) => Err(error),
        }
    }

    /// 从受保护存储取回 FileKey（必要时把缓存记录重新注入进进程内缓存）。
    ///
    /// 重启后进程内没有缓存记录，但附件记录与密钥记录都在正式库里；这里用
    /// storage 的 attachment_secrets 恢复 file_id + FileKey，使“关掉重开仍可导出”
    /// 成为真实行为。密钥记录缺失（例如旧数据）时仍显式 ATTACHMENT_KEY_UNAVAILABLE。
    fn ensure_cache_record(&self, record: &StoredAttachmentRecord) -> AttachmentResult<()> {
        if self.cache.cached_descriptor(&record.attachment_id).is_some() {
            return Ok(());
        }
        let secret = {
            let store = self.lock_store()?;
            store
                .load_attachment_secret(&record.attachment_id)
                .map_err(AttachmentError::from)?
        };
        let Some(secret) = secret else {
            return Err(AttachmentError::KeyUnavailable(
                "the file key is not persisted for this attachment; re-import the file before exporting",
            ));
        };
        if secret.file_key.len() != 32 {
            return Err(AttachmentError::AttachmentCorrupted(
                "stored file key has an invalid length",
            ));
        }
        let mut file_id = [0u8; 16];
        file_id.copy_from_slice(&secret.file_id);
        let mut file_key = [0u8; 32];
        file_key.copy_from_slice(secret.file_key.as_slice());
        let descriptor = AttachmentDescriptor {
            attachment_id: record.attachment_id.clone(),
            plain_size: record.plain_size,
            chunk_size: record.chunk_size,
            chunk_count: record.chunk_count,
            content_digest: record.content_digest.clone(),
            display_name: record.display_name.clone(),
        };
        let ciphertext_digest = digest_from_hex(&record.ciphertext_digest)?;
        let state = AttachmentState::from_code(&record.state).unwrap_or(AttachmentState::Imported);
        let result = self
            .cache
            .restore_record(descriptor, file_id, file_key, ciphertext_digest, state);
        file_key.fill(0);
        result
    }

    /// 导出附件：先核对持久化记录与本地缓存一致，再逐块认证解密并复核完整摘要。
    ///
    /// 重启后进程内没有缓存记录时，先用 attachment_secrets 里的 FileKey 恢复缓存记录，
    /// 因此“关掉重开仍可导出”成立；密钥记录缺失时显式 ATTACHMENT_KEY_UNAVAILABLE。
    pub fn export_attachment(
        &self,
        attachment_id: &str,
        destination: &FileToken,
    ) -> AttachmentResult<ExportReceipt> {
        validate_attachment_id(attachment_id)?;
        let record = self
            .attachment_record(attachment_id)?
            .ok_or(AttachmentError::AttachmentNotFound)?;
        self.ensure_cache_record(&record)?;
        let Some(cached) = self.cache.cached_descriptor(attachment_id) else {
            return Err(AttachmentError::AttachmentCorrupted(
                "restored attachment cache record is missing",
            ));
        };
        let manifest = self.cache.attachment_manifest(attachment_id)?;
        let matches = cached.plain_size == record.plain_size
            && cached.chunk_size == record.chunk_size
            && cached.chunk_count == record.chunk_count
            && cached.content_digest == record.content_digest
            && cached.display_name == record.display_name
            && manifest.ciphertext_digest == record.ciphertext_digest;
        if !matches {
            return Err(AttachmentError::AttachmentCorrupted(
                "persisted attachment record does not match the local encrypted cache",
            ));
        }
        let state = AttachmentState::from_code(&record.state).ok_or(
            AttachmentError::AttachmentCorrupted("stored attachment state is unknown"),
        )?;
        if matches!(state, AttachmentState::Staging | AttachmentState::Failed) {
            return Err(AttachmentError::AttachmentNotReady(
                "attachment record is not exportable yet",
            ));
        }
        self.cache.export_attachment(attachment_id, destination)
    }

    /// 把该附件的 FileKey 封装成 E2EE SecretDescriptor（用调用方给的包装密钥）。
    ///
    /// 包装密钥由上层从 MLS application secret 派生（本轮未接线），因此描述符可以被
    /// 放进 E2EE 内层事件发给同群设备；接收方用同一包装密钥打开后即可解密/导出。
    pub fn export_secret_descriptor(
        &self,
        attachment_id: &str,
        wrapping_key: &[u8; 32],
    ) -> AttachmentResult<SecretDescriptor> {
        validate_attachment_id(attachment_id)?;
        let record = self
            .attachment_record(attachment_id)?
            .ok_or(AttachmentError::AttachmentNotFound)?;
        self.ensure_cache_record(&record)?;
        let material = self.cache.secret_material(attachment_id)?;
        let mut file_key = material.file_key;
        let key = FileKey::try_from_slice(&file_key);
        file_key.fill(0);
        let key = key?;
        let descriptor = seal_secret_descriptor(wrapping_key, attachment_id, &key)?;
        Ok(descriptor)
    }

    /// 删除附件：先删持久化记录（权威，外键级联同时清掉 attachment_secrets），再清本地缓存。
    pub fn delete_attachment(&self, attachment_id: &str) -> AttachmentResult<bool> {
        validate_attachment_id(attachment_id)?;
        let record_existed = self.attachment_record(attachment_id)?.is_some();
        if record_existed {
            let mut store = self.lock_store()?;
            store.delete_attachment(attachment_id)?;
        }
        let cache_deleted = match self.cache.delete(attachment_id) {
            Ok(deleted) => deleted,
            Err(AttachmentError::AttachmentNotFound) => false,
            Err(error) => return Err(error),
        };
        Ok(record_existed || cache_deleted)
    }
}

// ============================================================================
// ATTACHMENTS-003 / 005：块传输端口（上传/下载），不依赖 node-client
// ============================================================================

/// 上传请求：只含公开清单字段（不含 FileKey、明文摘要与正文）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadRequest {
    /// 本地附件 ID（也是节点侧的 blob 标识）。
    pub attachment_id: String,
    /// 明文总字节数。
    pub plain_size: u64,
    /// 分块大小。
    pub chunk_size: u32,
    /// 分块总数。
    pub chunk_count: u32,
    /// 密文总字节数（各分块密文长度之和）。
    pub ciphertext_bytes: u64,
    /// 密文整文件 SHA-256（小写 hex）；节点据此核对清单。
    pub ciphertext_digest: String,
}

/// 上传会话句柄（不透明文本；实现方决定其有效期与幂等范围）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadSession {
    session_id: String,
}

impl UploadSession {
    /// 由端口实现方的会话标识构造。
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
        }
    }

    /// 会话标识。
    pub fn as_str(&self) -> &str {
        &self.session_id
    }

    /// 取出会话标识。
    pub fn into_id(self) -> String {
        self.session_id
    }
}

impl fmt::Display for UploadSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.session_id)
    }
}

/// 块传输端口：由适配层（node-client / 测试替身）实现，本 crate 不依赖具体客户端。
///
/// 实现契约（编排层依赖这些语义，缺一不可）：
/// - begin_upload 为一次上传尝试建立会话，返回稳定会话标识；
/// - put_chunk 对同一 (session, index) 重复写入同一字节必须幂等
///   （重复块不重复计费、不产生第二份数据）；
/// - complete 收到整文件密文摘要后由节点确认清单；
/// - read_chunk 按 blob + 序号返回分块密文；缺失/未就绪必须显式失败
///   （不得返回空字节冒充成功）。
pub trait BlobTransport {
    /// 建立上传会话。
    fn begin_upload(&mut self, request: UploadRequest) -> AttachmentResult<UploadSession>;

    /// 写入一块密文（同会话同序号幂等）。
    fn put_chunk(&mut self, session: &str, index: u32, bytes: &[u8]) -> AttachmentResult<()>;

    /// 完成上传：节点按整文件密文摘要确认清单。
    fn complete(&mut self, session: &str, total_ciphertext_digest: &str) -> AttachmentResult<()>;

    /// 读取一块密文（下载路径）。
    fn read_chunk(&mut self, blob_id: &str, index: u32) -> AttachmentResult<Vec<u8>>;
}

/// 下载回执（ATTACHMENTS-005）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadReceipt {
    /// 本地附件 ID。
    pub attachment_id: String,
    /// 实际写出的明文字节数。
    pub written_bytes: u64,
    /// 明文的整体 SHA-256（小写 hex），与持久化记录一致才会返回成功。
    pub content_digest: String,
}

impl AttachmentStore {
    /// 读取本地缓存的分块密文（只读；缺块按损坏处理，不返回空字节）。
    fn ciphertext_chunk(&self, attachment_id: &str, index: u32) -> AttachmentResult<Vec<u8>> {
        let path = self.chunk_path(attachment_id, index)?;
        fs::read(&path)
            .map_err(|_| AttachmentError::AttachmentCorrupted("cached ciphertext chunk is missing"))
    }

    /// 同步进程内的传输进度与状态（缓存状态与持久化状态保持一致）。
    ///
    /// 重启后没有进程内记录时静默跳过：进度不是持久真相（附件表没有进度列）。
    fn note_transfer(
        &self,
        attachment_id: &str,
        state: AttachmentState,
        transferred: u32,
        failure: Option<&'static str>,
    ) {
        let Ok(mut records) = self.records.lock() else {
            return;
        };
        if let Some(record) = records.get_mut(attachment_id) {
            record.state = state;
            record.transferred_chunks = transferred.min(record.descriptor.chunk_count);
            record.failure = failure;
        }
    }
}

impl PersistedAttachmentStore {
    fn mark_persisted_state(
        &self,
        attachment_id: &str,
        state: &str,
    ) -> AttachmentResult<StoredAttachmentRecord> {
        let mut store = self.lock_store()?;
        store
            .mark_attachment_state(attachment_id, state)
            .map_err(AttachmentError::from)
    }

    /// 由持久化记录 + 进程内进度组装的对外状态（进度取缓存，状态以持久记录为准）。
    fn persisted_status(&self, record: &StoredAttachmentRecord) -> AttachmentStatus {
        let state =
            AttachmentState::from_code(&record.state).unwrap_or(AttachmentState::Failed);
        let (available_chunks, transferred_chunks, failure) =
            match self.cache.attachment_status(&record.attachment_id) {
                Ok(status) => (
                    status.available_chunks,
                    status.transferred_chunks,
                    status.failure,
                ),
                Err(_) => (0, 0, None),
            };
        AttachmentStatus {
            attachment_id: record.attachment_id.clone(),
            state,
            plain_size: record.plain_size,
            chunk_count: record.chunk_count,
            available_chunks,
            transferred_chunks,
            cancel_requested: state == AttachmentState::Cancelled,
            failure,
        }
    }

    /// 读取某个方向的断点续传进度（不存在返回 None）。
    fn load_transfer_progress(
        &self,
        attachment_id: &str,
        direction: &str,
    ) -> AttachmentResult<Option<StoredTransferProgress>> {
        let store = self.lock_store()?;
        store
            .load_attachment_progress(attachment_id, direction)
            .map_err(AttachmentError::from)
    }

    /// 写入断点续传进度（单事务提交；每成功一块调用一次，崩溃最多丢正在传的一块）。
    #[allow(clippy::too_many_arguments)]
    fn save_transfer_progress(
        &self,
        attachment_id: &str,
        direction: &str,
        chunk_count: u32,
        confirmed: &[bool],
        session_id: Option<&str>,
        blob_id: Option<&str>,
        prefix_digest: Option<&str>,
    ) -> AttachmentResult<()> {
        let mut store = self.lock_store()?;
        store
            .save_attachment_progress(PersistedProgressInput {
                attachment_id: attachment_id.to_owned(),
                direction: direction.to_owned(),
                chunk_count,
                confirmed: confirmed.to_vec(),
                session_id: session_id.map(str::to_owned),
                blob_id: blob_id.map(str::to_owned),
                prefix_digest: prefix_digest.map(str::to_owned),
            })
            .map_err(AttachmentError::from)?;
        Ok(())
    }

    /// 清除某个方向的断点续传进度（完成/放弃时调用；不存在也算成功）。
    fn clear_transfer_progress(
        &self,
        attachment_id: &str,
        direction: &str,
    ) -> AttachmentResult<()> {
        let mut store = self.lock_store()?;
        store
            .clear_attachment_progress(attachment_id, direction)
            .map_err(AttachmentError::from)?;
        Ok(())
    }

    /// 按位图补发剩余分块：跳过已确认块，每成功一块原子写进度。
    ///
    /// 返回 UPLOAD_EXPIRED 表示节点会话失效（调用方重新 begin 并从零重发）。
    fn send_remaining_upload_chunks<T: BlobTransport + ?Sized>(
        &self,
        attachment_id: &str,
        record: &StoredAttachmentRecord,
        session: &str,
        confirmed: &mut [bool],
        transport: &mut T,
    ) -> AttachmentResult<()> {
        let mut sent = confirmed.iter().filter(|done| **done).count() as u32;
        for index in 0..record.chunk_count {
            if confirmed[index as usize] {
                continue;
            }
            let chunk = self.cache.ciphertext_chunk(attachment_id, index)?;
            transport.put_chunk(session, index, &chunk)?;
            confirmed[index as usize] = true;
            sent = sent.saturating_add(1);
            // 进度写入是独立短事务：成功后立刻可被重启复用。
            self.save_transfer_progress(
                attachment_id,
                "upload",
                record.chunk_count,
                confirmed,
                Some(session),
                Some(attachment_id),
                None,
            )?;
            self.cache
                .note_transfer(attachment_id, AttachmentState::Uploading, sent, None);
        }
        Ok(())
    }

    /// ATTACHMENTS-003：把本地加密缓存的分块密文按序上传，成功后标 available。
    ///
    /// 语义：
    /// - 状态机走 storage 白名单：imported → uploading → available（上传重试是
    ///   uploading → uploading 幂等；取消/失败/未导入显式拒绝）；
    /// - 已 available 时再次调用是**幂等无操作**（不建会话、不发块）；
    /// - **断点续传**（迁移 0008）：每成功一块就把“已确认位图 + 节点会话”写入正式库
    ///   （单事务提交）；重开进程后复用同一会话、跳过已确认块，只补发剩余块；
    /// - 节点会话失效（UPLOAD_EXPIRED）时重新 begin、位图清零后整份重发（一次机会），
    ///   保证节点侧与本地位图不会长期不一致；
    /// - 中途失败保留本地密文缓存与持久进度，状态停在 uploading 可原样重试；
    /// - 只有节点 complete 确认后才会把记录标成 available，绝不在网络失败时伪造成功。
    pub fn upload_attachment<T: BlobTransport + ?Sized>(
        &self,
        attachment_id: &str,
        transport: &mut T,
    ) -> AttachmentResult<AttachmentStatus> {
        validate_attachment_id(attachment_id)?;
        let record = self
            .attachment_record(attachment_id)?
            .ok_or(AttachmentError::AttachmentNotFound)?;
        let state = AttachmentState::from_code(&record.state).ok_or(
            AttachmentError::AttachmentCorrupted("stored attachment state is unknown"),
        )?;
        match state {
            AttachmentState::Available => return Ok(self.persisted_status(&record)),
            AttachmentState::Imported | AttachmentState::Uploading => {}
            AttachmentState::Cancelled => {
                return Err(AttachmentError::AttachmentNotReady(
                    "attachment transfer was cancelled",
                ));
            }
            AttachmentState::Downloading => {
                return Err(AttachmentError::AttachmentNotReady(
                    "attachment is being downloaded",
                ));
            }
            AttachmentState::Staging | AttachmentState::Failed => {
                return Err(AttachmentError::AttachmentNotReady(
                    "attachment was not imported successfully",
                ));
            }
        }
        if self.cache.available_chunks(attachment_id, record.chunk_count) != record.chunk_count {
            return Err(AttachmentError::AttachmentCorrupted(
                "cached ciphertext chunks are incomplete",
            ));
        }
        let mut ciphertext_bytes = 0u64;
        for index in 0..record.chunk_count {
            let chunk = self.cache.ciphertext_chunk(attachment_id, index)?;
            ciphertext_bytes = ciphertext_bytes.saturating_add(chunk.len() as u64);
        }
        let request = UploadRequest {
            attachment_id: record.attachment_id.clone(),
            plain_size: record.plain_size,
            chunk_size: record.chunk_size,
            chunk_count: record.chunk_count,
            ciphertext_bytes,
            ciphertext_digest: record.ciphertext_digest.clone(),
        };
        self.mark_persisted_state(attachment_id, AttachmentState::Uploading.code())?;

        // 断点续传：优先复用上次的会话与已确认位图。
        let progress = self.load_transfer_progress(attachment_id, "upload")?;
        let mut confirmed = match &progress {
            Some(progress) => progress.confirmed.clone(),
            None => vec![false; record.chunk_count as usize],
        };
        if confirmed.len() != record.chunk_count as usize {
            return Err(AttachmentError::AttachmentCorrupted(
                "stored upload progress does not match the attachment chunk count",
            ));
        }
        let mut session = progress.as_ref().and_then(|p| p.session_id.clone());
        self.cache.note_transfer(
            attachment_id,
            AttachmentState::Uploading,
            confirmed.iter().filter(|done| **done).count() as u32,
            None,
        );

        let mut expired_retried = false;
        loop {
            let current = match session.clone() {
                Some(session) => session,
                None => {
                    let started = transport.begin_upload(request.clone())?;
                    let started = started.into_id();
                    self.save_transfer_progress(
                        attachment_id,
                        "upload",
                        record.chunk_count,
                        &confirmed,
                        Some(&started),
                        Some(attachment_id),
                        None,
                    )?;
                    started
                }
            };
            match self.send_remaining_upload_chunks(
                attachment_id,
                &record,
                &current,
                &mut confirmed,
                transport,
            ) {
                Ok(()) => match transport.complete(&current, &record.ciphertext_digest) {
                    Ok(()) => break,
                    Err(error) if error.code() == "UPLOAD_EXPIRED" && !expired_retried => {
                        expired_retried = true;
                        session = None;
                        confirmed = vec![false; record.chunk_count as usize];
                        self.save_transfer_progress(
                            attachment_id,
                            "upload",
                            record.chunk_count,
                            &confirmed,
                            None,
                            Some(attachment_id),
                            None,
                        )?;
                        continue;
                    }
                    Err(error) => {
                        self.cache.note_transfer(
                            attachment_id,
                            AttachmentState::Uploading,
                            confirmed.iter().filter(|done| **done).count() as u32,
                            Some("upload failed"),
                        );
                        if !is_retryable_transfer_error(&error) {
                            let _ = self.clear_transfer_progress(attachment_id, "upload");
                        }
                        return Err(error);
                    }
                },
                Err(error) if error.code() == "UPLOAD_EXPIRED" && !expired_retried => {
                    expired_retried = true;
                    session = None;
                    confirmed = vec![false; record.chunk_count as usize];
                    self.save_transfer_progress(
                        attachment_id,
                        "upload",
                        record.chunk_count,
                        &confirmed,
                        None,
                        Some(attachment_id),
                        None,
                    )?;
                    continue;
                }
                Err(error) => {
                    self.cache.note_transfer(
                        attachment_id,
                        AttachmentState::Uploading,
                        confirmed.iter().filter(|done| **done).count() as u32,
                        Some("upload failed"),
                    );
                    if !is_retryable_transfer_error(&error) {
                        let _ = self.clear_transfer_progress(attachment_id, "upload");
                    }
                    return Err(error);
                }
            }
        }

        // 先清进度再标 available：进度残留不会影响幂等（available 早退），
        // 但清掉更干净；即使标记失败，重试看到 uploading 会按已确认位图继续。
        self.clear_transfer_progress(attachment_id, "upload")?;
        let available =
            self.mark_persisted_state(attachment_id, AttachmentState::Available.code())?;
        self.cache.note_transfer(
            attachment_id,
            AttachmentState::Available,
            record.chunk_count,
            None,
        );
        Ok(self.persisted_status(&available))
    }

    /// ATTACHMENTS-005：下载认证密文→逐块认证解密→整体摘要校验→写出明文。
    ///
    /// 语义：
    /// - 只接受已发布（available）或正在下载（downloading，可重试）的记录；
    /// - expected_manifest 必须与本地持久化记录一致（块数/大小/块大小/密文摘要），
    ///   否则在发起任何网络读取前显式拒绝（ATTACHMENT_CORRUPTED）；
    /// - 解密需要进程内的 FileKey：重启后（没有进程内记录）显式
    ///   ATTACHMENT_KEY_UNAVAILABLE，绝不写出未经验证的文件；
    /// - 先写同目录临时文件，全部块认证通过 + 密文摘要 + 明文大小 + 明文摘要都一致
    ///   才 rename 成目标文件；任何失败都会删除临时文件；
    /// - blob_id 本轮就是本地附件 ID（节点侧 blob 标识复用附件 ID，不新增第二种 ID）。
    pub fn download_attachment<T: BlobTransport + ?Sized>(
        &self,
        blob_id: &str,
        expected_manifest: &AttachmentManifest,
        destination: &FileToken,
        transport: &mut T,
    ) -> AttachmentResult<DownloadReceipt> {
        validate_attachment_id(blob_id)?;
        let record = self
            .attachment_record(blob_id)?
            .ok_or(AttachmentError::AttachmentNotFound)?;
        // 重启后进程内没有 FileKey：先从 attachment_secrets 恢复缓存记录。
        self.ensure_cache_record(&record)?;
        let state = AttachmentState::from_code(&record.state).ok_or(
            AttachmentError::AttachmentCorrupted("stored attachment state is unknown"),
        )?;
        match state {
            AttachmentState::Available | AttachmentState::Downloading => {}
            _ => {
                return Err(AttachmentError::AttachmentNotReady(
                    "attachment is not published yet",
                ));
            }
        }
        if expected_manifest.attachment_id != record.attachment_id {
            return Err(AttachmentError::AttachmentCorrupted(
                "expected manifest belongs to another attachment",
            ));
        }
        if expected_manifest.plain_size != record.plain_size
            || expected_manifest.chunk_size != record.chunk_size
            || expected_manifest.chunk_count != record.chunk_count
            || expected_manifest.ciphertext_digest != record.ciphertext_digest
        {
            return Err(AttachmentError::AttachmentCorrupted(
                "expected manifest does not match the stored attachment record",
            ));
        }
        let destination_path = destination.path();
        if destination_path.is_dir() {
            return Err(AttachmentError::InvalidFileToken(
                "destination must name a file",
            ));
        }
        let file_name = destination_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(AttachmentError::InvalidFileToken(
                "destination must name a file",
            ))?;
        if let Some(parent) = destination_path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.is_dir()
        {
            return Err(AttachmentError::FileIoFailure);
        }
        let partial_path =
            destination_path.with_file_name(format!("{file_name}.download-partial"));
        self.mark_persisted_state(blob_id, AttachmentState::Downloading.code())?;

        // 断点续传：读持久进度，校验本地半成品的前缀摘要，决定从哪一块继续。
        let progress = self.load_transfer_progress(blob_id, "download")?;
        let mut confirmed = match &progress {
            Some(progress) => progress.confirmed.clone(),
            None => vec![false; record.chunk_count as usize],
        };
        if confirmed.len() != record.chunk_count as usize {
            return Err(AttachmentError::AttachmentCorrupted(
                "stored download progress does not match the attachment chunk count",
            ));
        }
        let mut skip = confirmed.iter().take_while(|done| **done).count() as u32;
        let mut expected_prefix = 0u64;
        for index in 0..skip {
            expected_prefix = expected_prefix
                .saturating_add(u64::from(chunk_plain_length(record.plain_size, index)?));
        }
        let mut hasher = Sha256::new();
        let mut written = 0u64;
        let mut prefix_ok = skip == 0;
        if skip > 0 {
            let size = fs::metadata(&partial_path)
                .map(|meta| meta.len())
                .unwrap_or(0);
            if size == expected_prefix
                && let Ok(prefix) = fs::read(&partial_path)
            {
                hasher.update(&prefix);
                let digest = hex(hasher.clone().finalize().as_slice());
                if progress
                    .as_ref()
                    .and_then(|progress| progress.prefix_digest.clone())
                    .as_deref()
                    == Some(digest.as_str())
                {
                    prefix_ok = true;
                    written = size;
                }
            }
        }
        if skip > 0 && !prefix_ok {
            // 半成品与进度不符：丢弃进度与临时文件，从头下载（不静默接受坏前缀）。
            let _ = fs::remove_file(&partial_path);
            confirmed = vec![false; record.chunk_count as usize];
            skip = 0;
            hasher = Sha256::new();
            written = 0;
            self.save_transfer_progress(
                blob_id,
                "download",
                record.chunk_count,
                &confirmed,
                None,
                Some(blob_id),
                None,
            )?;
        }
        let mut file = if skip == 0 {
            let _ = fs::remove_file(&partial_path);
            self.save_transfer_progress(
                blob_id,
                "download",
                record.chunk_count,
                &confirmed,
                None,
                Some(blob_id),
                None,
            )?;
            File::create(&partial_path).map_err(|_| AttachmentError::FileIoFailure)?
        } else {
            OpenOptions::new()
                .append(true)
                .open(&partial_path)
                .map_err(|_| AttachmentError::FileIoFailure)?
        };
        // 只有“从头开始”的那一次能校验整段密文摘要；续传路径依赖每块 AEAD +
        // 明文整体摘要（前缀摘要已在上面按本地文件重算校验）。
        let mut ciphertext_hasher = (skip == 0).then(Sha256::new);
        // 只有从头开始的一轮会完整缓存密文块（续传前缀的密文已不在内存中）。
        let cache_chunks = skip == 0;
        let mut written_chunks: Vec<u32> = Vec::new();
        let outcome = (|| -> AttachmentResult<()> {
            for index in skip..record.chunk_count {
                let bytes = transport.read_chunk(blob_id, index)?;
                if let Some(hasher) = ciphertext_hasher.as_mut() {
                    hasher.update(&bytes);
                }
                let plaintext = {
                    let records = self.cache.lock_records()?;
                    let cached = records.get(blob_id).ok_or(AttachmentError::KeyUnavailable(
                        "the file key is not available for this attachment",
                    ))?;
                    let context = chunk_context_for(cached.file_id, record.plain_size, index)?;
                    let chunk = CipherChunk::from_bytes(bytes.clone())?;
                    decrypt_attachment_chunk(&cached.file_key, &context, &chunk)?.to_vec()
                };
                if cache_chunks && !self.cache.has_cached_chunk(blob_id, index) {
                    // 密文原样进缓存（字节与清单一致），供离线 export/status 使用；
                    // 已有块（同进程导入过）不重复写，失败时也只回滚本次新写的块。
                    self.cache.cache_downloaded_chunk(blob_id, index, &bytes)?;
                    written_chunks.push(index);
                }
                file.write_all(&plaintext)
                    .map_err(|_| AttachmentError::FileIoFailure)?;
                hasher.update(&plaintext);
                written = written.saturating_add(plaintext.len() as u64);
                file.sync_data().map_err(|_| AttachmentError::FileIoFailure)?;
                confirmed[index as usize] = true;
                let prefix_digest = hex(hasher.clone().finalize().as_slice());
                self.save_transfer_progress(
                    blob_id,
                    "download",
                    record.chunk_count,
                    &confirmed,
                    None,
                    Some(blob_id),
                    Some(&prefix_digest),
                )?;
            }
            file.sync_all().map_err(|_| AttachmentError::FileIoFailure)?;
            if written != record.plain_size {
                return Err(AttachmentError::AttachmentCorrupted(
                    "downloaded size does not match the descriptor",
                ));
            }
            if let Some(hasher) = ciphertext_hasher.take()
                && hex(hasher.finalize().as_slice()) != record.ciphertext_digest
            {
                return Err(AttachmentError::AttachmentCorrupted(
                    "downloaded ciphertext digest does not match the manifest",
                ));
            }
            let content_digest = hex(hasher.finalize().as_slice());
            if content_digest != record.content_digest {
                return Err(AttachmentError::AttachmentCorrupted(
                    "downloaded content digest does not match the descriptor",
                ));
            }
            Ok(())
        })();
        drop(file);
        match outcome {
            Ok(()) => {
                // 缓存完整性检查（密文摘要与清单一致）失败时不留本次新写的半份缓存。
                if cache_chunks
                    && let Err(error) = self.cache.finish_cached_download(blob_id)
                {
                    for index in written_chunks.drain(..) {
                        let _ = self.cache.remove_cached_chunk(blob_id, index);
                    }
                    let _ = fs::remove_file(&partial_path);
                    return Err(error);
                }
                if destination_path.exists() && fs::remove_file(destination_path).is_err() {
                    let _ = fs::remove_file(&partial_path);
                    return Err(AttachmentError::FileIoFailure);
                }
                if fs::rename(&partial_path, destination_path).is_err() {
                    let _ = fs::remove_file(&partial_path);
                    return Err(AttachmentError::FileIoFailure);
                }
                self.clear_transfer_progress(blob_id, "download")?;
                self.mark_persisted_state(blob_id, AttachmentState::Available.code())?;
                self.cache.note_transfer(
                    blob_id,
                    AttachmentState::Available,
                    record.chunk_count,
                    None,
                );
                Ok(DownloadReceipt {
                    attachment_id: record.attachment_id.clone(),
                    written_bytes: written,
                    content_digest: record.content_digest.clone(),
                })
            }
            Err(error) => {
                // 可重试的传输类错误：保留半成品与进度供续传；其余（损坏、缺密钥、IO 等）
                // 按终态清理半成品、缓存块与进度。
                let retryable = is_retryable_transfer_error(&error);
                // 只回滚本次新写的缓存块：既有缓存（导入时写入）不受失败影响。
                for index in written_chunks.drain(..) {
                    let _ = self.cache.remove_cached_chunk(blob_id, index);
                }
                if !retryable {
                    let _ = fs::remove_file(&partial_path);
                    let _ = self.clear_transfer_progress(blob_id, "download");
                }
                // 状态留在 downloading（可重试），不写出任何未验证文件。
                self.cache.note_transfer(
                    blob_id,
                    AttachmentState::Downloading,
                    confirmed.iter().filter(|done| **done).count() as u32,
                    Some("download failed"),
                );
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static UNIQUE: AtomicU64 = AtomicU64::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0);
            let unique = UNIQUE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "chat-att-{}-{nanos}-{unique}-{tag}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }

        fn source(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.path(name);
            fs::write(&path, bytes).expect("write source file");
            path
        }

        fn store(&self) -> AttachmentStore {
            AttachmentStore::new(self.path("cache")).expect("open attachment store")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn pattern(length: usize, seed: u8) -> Vec<u8> {
        (0..length)
            .map(|index| (index as u8).wrapping_mul(17).wrapping_add(seed))
            .collect()
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        !needle.is_empty() && haystack.windows(needle.len()).any(|window| window == needle)
    }

    /// 递归收集缓存目录里每个文件的字节，用于“缓存中无明文/摘要”检查。
    fn collect_files(directory: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(_) => return,
        };
        for entry in entries {
            let entry = entry.expect("read cache entry");
            let path = entry.path();
            if path.is_dir() {
                collect_files(&path, files);
            } else {
                let bytes = fs::read(&path).expect("read cache file");
                files.push((path, bytes));
            }
        }
    }

    fn export_to(
        store: &AttachmentStore,
        attachment_id: &str,
        destination: &Path,
    ) -> AttachmentResult<ExportReceipt> {
        store.export_attachment(attachment_id, &FileToken::from_local_path(destination))
    }

    #[test]
    fn import_then_export_roundtrip_and_cache_has_no_plaintext() {
        let dir = TempDir::new("roundtrip");
        let store = dir.store();
        let marker_head = b"ATTACHMENT-PLAINTEXT-MARKER-HEAD";
        let marker_tail = b"ATTACHMENT-PLAINTEXT-MARKER-TAIL";
        let mut source = marker_head.to_vec();
        source.extend(pattern(CHUNK_BYTES + 1024, 3));
        source.extend_from_slice(marker_tail);
        let source_path = dir.source("note.bin", &source);

        let descriptor = store
            .import_file(&FileToken::from_local_path(&source_path), "会议纪要.txt")
            .expect("import file");
        assert_eq!(descriptor.plain_size, source.len() as u64);
        assert_eq!(descriptor.chunk_size, CHUNK_BYTES as u32);
        assert_eq!(descriptor.chunk_count, 2);
        assert_eq!(descriptor.display_name, "会议纪要.txt");
        assert_eq!(descriptor.content_digest.len(), 64);
        assert!(descriptor.attachment_id.starts_with(ATTACHMENT_ID_PREFIX));
        assert_eq!(descriptor.attachment_id.len(), ATTACHMENT_ID_PREFIX.len() + 32);

        let status = store.attachment_status(&descriptor.attachment_id).unwrap();
        assert_eq!(status.state, AttachmentState::Imported);
        assert_eq!(status.chunk_count, 2);
        assert_eq!(status.available_chunks, 2, "all chunks are cached locally");
        assert_eq!(status.transferred_chunks, 0, "no network transfer happened");
        assert!(!status.cancel_requested);

        let manifest = store.attachment_manifest(&descriptor.attachment_id).unwrap();
        assert_eq!(manifest.plain_size, descriptor.plain_size);
        assert_eq!(manifest.chunk_count, descriptor.chunk_count);
        assert_eq!(manifest.ciphertext_digest.len(), 64);
        assert!(
            manifest.ciphertext_bytes > manifest.plain_size,
            "ciphertext carries per-chunk nonce and tag"
        );

        let destination = dir.path("export.bin");
        let receipt = export_to(&store, &descriptor.attachment_id, &destination).unwrap();
        assert_eq!(receipt.attachment_id, descriptor.attachment_id);
        assert_eq!(receipt.written_bytes, source.len() as u64);
        assert_eq!(receipt.content_digest, descriptor.content_digest);
        assert_eq!(fs::read(&destination).unwrap(), source);

        // 缓存里只有分块密文：不能出现明文标记，也不能出现明文摘要。
        let mut files = Vec::new();
        collect_files(store.root(), &mut files);
        assert_eq!(files.len(), 2, "one file per chunk");
        for (path, bytes) in &files {
            assert!(
                !contains(bytes, marker_head),
                "plaintext head leaked into {}",
                path.display()
            );
            assert!(
                !contains(bytes, marker_tail),
                "plaintext tail leaked into {}",
                path.display()
            );
            assert!(
                !contains(bytes, descriptor.content_digest.as_bytes()),
                "plaintext digest leaked into {}",
                path.display()
            );
        }
    }

    #[test]
    fn exactly_25_mib_is_accepted_and_oversize_is_rejected() {
        let dir = TempDir::new("limit");
        let store = dir.store();

        // 恰好 25 MiB：用 set_len 生成零填充文件，避免测试写 25 MiB 随机数据。
        let exact_path = dir.path("exact.bin");
        File::create(&exact_path)
            .unwrap()
            .set_len(MAX_ATTACHMENT_BYTES_LIMIT)
            .unwrap();
        let descriptor = store
            .import_file(&FileToken::from_local_path(&exact_path), "exact.bin")
            .expect("25 MiB must be accepted");
        assert_eq!(descriptor.plain_size, MAX_ATTACHMENT_BYTES_LIMIT);
        assert_eq!(descriptor.chunk_count, 100);

        let destination = dir.path("exact.out");
        let receipt = export_to(&store, &descriptor.attachment_id, &destination).unwrap();
        assert_eq!(receipt.written_bytes, MAX_ATTACHMENT_BYTES_LIMIT);
        assert_eq!(
            fs::metadata(&destination).unwrap().len(),
            MAX_ATTACHMENT_BYTES_LIMIT
        );
        // 导出文件的摘要必须与描述符一致。
        let digest = {
            let mut hasher = Sha256::new();
            let mut file = File::open(&destination).unwrap();
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                let read = file.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            hex(&hasher.finalize())
        };
        assert_eq!(digest, descriptor.content_digest);

        // 25 MiB + 1：必须在上限处拒绝，且不留下任何缓存目录（用独立缓存根验证）。
        let oversize_dir = TempDir::new("limit-oversize");
        let oversize_store = oversize_dir.store();
        let oversize_path = oversize_dir.path("oversize.bin");
        File::create(&oversize_path)
            .unwrap()
            .set_len(MAX_ATTACHMENT_BYTES_LIMIT + 1)
            .unwrap();
        let error = oversize_store
            .import_file(&FileToken::from_local_path(&oversize_path), "oversize.bin")
            .unwrap_err();
        assert_eq!(error.code(), "CONTENT_TOO_LARGE");
        let mut cache = Vec::new();
        collect_files(oversize_store.root(), &mut cache);
        assert!(
            cache.is_empty(),
            "rejected import must not leave cache files behind"
        );
    }

    #[test]
    fn oversized_or_misdeclared_streams_stop_at_the_bound() {
        let dir = TempDir::new("stream");
        let store = dir.store();
        // 声明大小已超上限：必须在读取任何字节前拒绝。
        let error = store
            .import_reader(Cursor::new(vec![0u8; 8]), MAX_ATTACHMENT_BYTES_LIMIT + 1, "big")
            .unwrap_err();
        assert_eq!(error.code(), "CONTENT_TOO_LARGE");
        // 声明 1 KiB 却给出 4 KiB：大小漂移必须显式失败，不能静默截断。
        let payload = vec![7u8; 4096];
        let error = store
            .import_reader(Cursor::new(payload), 1024, "misdeclared")
            .unwrap_err();
        assert_eq!(error.code(), "FILE_IO_FAILURE");
        let mut cache = Vec::new();
        collect_files(store.root(), &mut cache);
        assert!(cache.is_empty(), "failed import must clean its cache");
        // 声明 1 KiB 但流更短：同样必须失败。
        let error = store
            .import_reader(Cursor::new(vec![1u8; 100]), 1024, "short")
            .unwrap_err();
        assert_eq!(error.code(), "FILE_IO_FAILURE");
    }

    fn mutated_export_error(
        tag: &str,
        mutate: &dyn Fn(&AttachmentStore, &str),
    ) -> &'static str {
        let dir = TempDir::new(tag);
        let store = dir.store();
        let mut source = pattern(CHUNK_BYTES, 5);
        source.extend(pattern(200, 9));
        let source_path = dir.source("source.bin", &source);
        let descriptor = store
            .import_file(&FileToken::from_local_path(&source_path), "source.bin")
            .unwrap();
        mutate(&store, &descriptor.attachment_id);
        let destination = dir.path("out.bin");
        let error = export_to(&store, &descriptor.attachment_id, &destination).unwrap_err();
        assert!(
            !destination.exists(),
            "no destination may be created for a rejected export ({tag})"
        );
        // 失败后不能留下任何 export-partial 临时文件。
        for entry in fs::read_dir(&dir.path).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(
                !name.contains("export-partial"),
                "partial export file left behind: {name}"
            );
        }
        error.code()
    }

    #[test]
    fn tampered_chunks_are_rejected_on_export() {
        let code = mutated_export_error("tamper-byte", &|store, id| {
            let path = store.chunk_path(id, 0).unwrap();
            let mut bytes = fs::read(&path).unwrap();
            let last = bytes.len() - 20;
            bytes[last] ^= 0x01;
            fs::write(&path, bytes).unwrap();
        });
        assert_eq!(code, "ATTACHMENT_CORRUPTED");

        let code = mutated_export_error("tamper-truncate-chunk", &|store, id| {
            let path = store.chunk_path(id, 0).unwrap();
            let mut bytes = fs::read(&path).unwrap();
            bytes.pop();
            fs::write(&path, bytes).unwrap();
        });
        assert_eq!(code, "ATTACHMENT_CORRUPTED");

        let code = mutated_export_error("tamper-missing-chunk", &|store, id| {
            let path = store.chunk_path(id, 1).unwrap();
            fs::remove_file(&path).unwrap();
        });
        assert_eq!(code, "ATTACHMENT_CORRUPTED");

        let code = mutated_export_error("tamper-reorder", &|store, id| {
            let first = store.chunk_path(id, 0).unwrap();
            let second = store.chunk_path(id, 1).unwrap();
            let first_bytes = fs::read(&first).unwrap();
            let second_bytes = fs::read(&second).unwrap();
            fs::write(&first, second_bytes).unwrap();
            fs::write(&second, first_bytes).unwrap();
        });
        assert_eq!(code, "ATTACHMENT_CORRUPTED");
    }

    #[test]
    fn export_rejects_a_digest_mismatch() {
        let dir = TempDir::new("digest");
        let store = dir.store();
        let source_path = dir.source("source.bin", &pattern(2048, 1));
        let descriptor = store
            .import_file(&FileToken::from_local_path(&source_path), "source.bin")
            .unwrap();
        // 模拟本地记录被篡改（只有本模块测试能改内部记录）：
        // 密文本身仍然完整，但记录里的明文摘要与内容不符，导出必须拒绝。
        {
            let mut records = store.records.lock().unwrap();
            let record = records.get_mut(&descriptor.attachment_id).unwrap();
            record.descriptor.content_digest = "00".repeat(32);
        }
        let destination = dir.path("out.bin");
        let error = export_to(&store, &descriptor.attachment_id, &destination).unwrap_err();
        assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");
        assert!(!destination.exists());
    }

    #[test]
    fn cancel_transfer_state_machine_preserves_progress() {
        let dir = TempDir::new("cancel");
        let store = dir.store();
        let source_path = dir.source("source.bin", &pattern(1000, 2));
        let descriptor = store
            .import_file(&FileToken::from_local_path(&source_path), "source.bin")
            .unwrap();

        let status = store.cancel_transfer(&descriptor.attachment_id).unwrap();
        assert_eq!(status.state, AttachmentState::Cancelled);
        assert!(status.cancel_requested);
        assert_eq!(status.available_chunks, status.chunk_count);

        // 幂等：重复取消不报错、状态不变。
        let again = store.cancel_transfer(&descriptor.attachment_id).unwrap();
        assert_eq!(again.state, AttachmentState::Cancelled);
        assert_eq!(again.chunk_count, status.chunk_count);

        // 已取消后仍可查询，且本地密文块未被删除（可继续导出）。
        let status = store.attachment_status(&descriptor.attachment_id).unwrap();
        assert_eq!(status.state, AttachmentState::Cancelled);
        let destination = dir.path("cancelled.out");
        export_to(&store, &descriptor.attachment_id, &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), fs::read(&source_path).unwrap());

        // 已完成（Available）的附件拒绝取消。
        {
            let mut records = store.records.lock().unwrap();
            records.get_mut(&descriptor.attachment_id).unwrap().state = AttachmentState::Available;
        }
        let error = store.cancel_transfer(&descriptor.attachment_id).unwrap_err();
        assert_eq!(error.code(), "TRANSFER_ALREADY_COMPLETE");
    }

    #[test]
    fn invalid_inputs_fail_explicitly() {
        let dir = TempDir::new("invalid");
        let store = dir.store();
        let source_path = dir.source("source.bin", b"hello");

        // 未知/畸形附件 ID（含路径穿越尝试）都返回 ATTACHMENT_NOT_FOUND，不拼接路径。
        for id in [
            "att-00000000000000000000000000000000",
            "../../etc/passwd",
            "att-../../etc",
            "att-0123456789ABCDEF0123456789ABCDEF",
            "",
        ] {
            assert_eq!(
                store.attachment_status(id).unwrap_err().code(),
                "ATTACHMENT_NOT_FOUND",
                "id: {id}"
            );
            assert_eq!(
                store.build_attachment_descriptor(id).unwrap_err().code(),
                "ATTACHMENT_NOT_FOUND"
            );
        }

        // 来源令牌无效：不存在、目录、展示名非法。
        let missing = dir.path("missing.bin");
        assert_eq!(
            store
                .import_file(&FileToken::from_local_path(&missing), "missing.bin")
                .unwrap_err()
                .code(),
            "FILE_IO_FAILURE"
        );
        assert_eq!(
            store
                .import_file(&FileToken::from_local_path(&dir.path), "dir")
                .unwrap_err()
                .code(),
            "INVALID_FILE_TOKEN"
        );
        for name in ["", "..\\..\\evil", "a/b", "line\nbreak", &"x".repeat(MAX_DISPLAY_NAME_BYTES + 1)] {
            assert_eq!(
                store
                    .import_file(&FileToken::from_local_path(&source_path), name)
                    .unwrap_err()
                    .code(),
                "INVALID_DISPLAY_NAME",
                "name: {name:?}"
            );
        }

        // 导出目标必须是文件路径。
        assert_eq!(
            store
                .export_attachment(
                    "att-00000000000000000000000000000000",
                    &FileToken::from_local_path(&dir.path)
                )
                .unwrap_err()
                .code(),
            "ATTACHMENT_NOT_FOUND"
        );
        let descriptor = store
            .import_file(&FileToken::from_local_path(&source_path), "ok.bin")
            .unwrap();
        assert_eq!(
            store
                .export_attachment(&descriptor.attachment_id, &FileToken::from_local_path(&dir.path))
                .unwrap_err()
                .code(),
            "INVALID_FILE_TOKEN"
        );
    }

    #[test]
    fn empty_file_roundtrip() {
        let dir = TempDir::new("empty");
        let store = dir.store();
        let source_path = dir.source("empty.bin", b"");
        let descriptor = store
            .import_file(&FileToken::from_local_path(&source_path), "empty.bin")
            .unwrap();
        assert_eq!(descriptor.plain_size, 0);
        assert_eq!(descriptor.chunk_count, 1);
        let destination = dir.path("empty.out");
        let receipt = export_to(&store, &descriptor.attachment_id, &destination).unwrap();
        assert_eq!(receipt.written_bytes, 0);
        assert_eq!(fs::metadata(&destination).unwrap().len(), 0);
    }

    #[test]
    fn cache_paths_derive_only_from_attachment_ids() {
        let dir = TempDir::new("paths");
        let store = dir.store();
        let source_path = dir.source("source.bin", b"display name must not become a path");
        let descriptor = store
            .import_file(&FileToken::from_local_path(&source_path), "报告 v1.bin")
            .unwrap();
        let entries: Vec<String> = fs::read_dir(store.root())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec![descriptor.attachment_id.clone()]);
        let chunk_names: Vec<String> = fs::read_dir(store.root().join(&descriptor.attachment_id))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(chunk_names, vec!["chunk-000000.bin".to_owned()]);
        // 展示名不出现在任何缓存路径里。
        let mut files = Vec::new();
        collect_files(store.root(), &mut files);
        for (path, _) in files {
            assert!(!path.to_string_lossy().contains("报告"));
        }
    }

    fn formal_store(path: &Path) -> FormalStore {
        chat_storage::formal::open_store(
            chat_storage::formal::StoreOptions::new(
                path,
                chat_storage::formal::KeyProvenance::Raw32,
            ),
            chat_storage::formal::ProtectedStorageKey::from_bytes([7u8; 32]),
        )
        .expect("open formal store")
    }

    #[test]
    fn persisted_export_works_after_restart_with_persisted_secret() {
        let dir = TempDir::new("persisted-restart");
        let db_path = dir.path("chat.db");
        let cache_root = dir.path("cache");
        let marker = b"PERSISTED-ATTACHMENT-PLAINTEXT-MARKER";
        let mut source = marker.to_vec();
        source.extend(pattern(CHUNK_BYTES + 16, 4));
        let source_path = dir.source("persisted.bin", &source);
        let attachment_id;
        let file_id_bytes;
        let mut file_key_bytes;
        {
            let store = PersistedAttachmentStore::open(&cache_root, formal_store(&db_path))
                .expect("open persisted store");
            let descriptor = store
                .import_file(&FileToken::from_local_path(&source_path), "持久化.pdf")
                .expect("import");
            attachment_id = descriptor.attachment_id.clone();
            assert_eq!(descriptor.chunk_count, 2);
            let material = store
                .cache
                .secret_material(&attachment_id)
                .expect("secret material");
            file_id_bytes = material.file_id;
            file_key_bytes = material.file_key;
            let record = store
                .attachment_record(&attachment_id)
                .expect("record")
                .expect("record persisted");
            assert_eq!(record.state, "imported");
            assert_eq!(record.plain_size, descriptor.plain_size);
            assert_eq!(record.chunk_count, descriptor.chunk_count);
            assert_eq!(record.content_digest, descriptor.content_digest);
            assert_eq!(record.display_name, "持久化.pdf");
            assert!(record.already_existed, "reads always report an existing record");
            assert!(record.created_at_ms >= 0);
            assert_eq!(store.attachment_records(10).expect("list").len(), 1);
            assert_eq!(
                store.attachment_status(&attachment_id).expect("status").state,
                AttachmentState::Imported
            );
            // 同进程内 FileKey 还在内存：可以导出。
            let destination = dir.path("export.bin");
            let receipt = store
                .export_attachment(&attachment_id, &FileToken::from_local_path(&destination))
                .expect("export");
            assert_eq!(receipt.content_digest, descriptor.content_digest);
            assert_eq!(fs::read(&destination).expect("read export"), source);
        }

        // 关闭后同主密钥重开：进程内没有 FileKey，但 attachment_secrets 里有，
        // 导出必须**真实成功**且逐字节一致（这是本轮修复的核心行为）。
        {
            let store = PersistedAttachmentStore::open(&cache_root, formal_store(&db_path))
                .expect("reopen persisted store");
            let record = store
                .attachment_record(&attachment_id)
                .expect("record")
                .expect("record survives restart");
            assert_eq!(record.state, "imported");
            assert_eq!(record.display_name, "持久化.pdf");
            assert_eq!(store.attachment_records(10).expect("list").len(), 1);
            let status = store.attachment_status(&attachment_id).expect("status");
            assert_eq!(status.state, AttachmentState::Imported);
            assert_eq!(status.available_chunks, 0, "no in-memory chunks after restart");
            let destination = dir.path("after-restart.out");
            let receipt = store
                .export_attachment(&attachment_id, &FileToken::from_local_path(&destination))
                .expect("export after restart must succeed");
            assert_eq!(receipt.written_bytes, source.len() as u64);
            assert_eq!(fs::read(&destination).expect("read export"), source);
            // 恢复出来的缓存记录可以复用：再导出一次仍然一致（幂等）。
            let again = dir.path("after-restart-again.out");
            store
                .export_attachment(&attachment_id, &FileToken::from_local_path(&again))
                .expect("second export");
            assert_eq!(fs::read(&again).expect("read second export"), source);
        }

        // 诚实路径仍然存在：显式删掉密钥记录后，新进程（无缓存）导出必须显式失败。
        {
            let store = PersistedAttachmentStore::open(&cache_root, formal_store(&db_path))
                .expect("reopen persisted store");
            {
                let mut guard = store.store.lock().expect("formal store lock");
                assert!(guard
                    .delete_attachment_secret(&attachment_id)
                    .expect("delete secret"));
            }
            let destination = dir.path("no-secret.out");
            let error = store
                .export_attachment(&attachment_id, &FileToken::from_local_path(&destination))
                .unwrap_err();
            assert_eq!(error.code(), "ATTACHMENT_KEY_UNAVAILABLE");
            assert!(!destination.exists(), "no bogus export may be produced");
            // 未知附件仍然是 ATTACHMENT_NOT_FOUND（不把“缺密钥”泛化成“不存在”）。
            let missing = store
                .export_attachment(
                    "att-00000000000000000000000000000000",
                    &FileToken::from_local_path(&dir.path("unknown.out")),
                )
                .unwrap_err();
            assert_eq!(missing.code(), "ATTACHMENT_NOT_FOUND");
        }

        // 缓存与数据库文件里不出现明文或展示名（展示名只在密封列里）。
        let mut files = Vec::new();
        collect_files(&cache_root, &mut files);
        for suffix in ["", "-wal", "-shm"] {
            let path = PathBuf::from(format!("{}{suffix}", db_path.display()));
            if let Ok(bytes) = fs::read(path) {
                files.push((PathBuf::from(format!("db{suffix}")), bytes));
            }
        }
        assert!(!files.is_empty(), "cache and database files must be scanned");
        for (path, bytes) in files {
            assert!(
                !contains(&bytes, marker),
                "plaintext leaked into {}",
                path.display()
            );
            assert!(
                !contains(&bytes, "持久化.pdf".as_bytes()),
                "display name leaked into {}",
                path.display()
            );
            assert!(
                !contains(&bytes, &file_key_bytes),
                "file key leaked into {}",
                path.display()
            );
            assert!(
                !contains(&bytes, &file_id_bytes),
                "file id leaked into {}",
                path.display()
            );
        }
        file_key_bytes.fill(0);
    }


    #[test]
    fn persisted_secret_tamper_is_state_corrupted() {
        let dir = TempDir::new("secret-tamper");
        let db_path = dir.path("chat.db");
        let cache_root = dir.path("cache");
        let source_path = dir.source("tamper.bin", &pattern(700, 6));
        let attachment_id = {
            let store = PersistedAttachmentStore::open(&cache_root, formal_store(&db_path))
                .expect("open persisted store");
            let descriptor = store
                .import_file(&FileToken::from_local_path(&source_path), "tamper.bin")
                .expect("import");
            let id = descriptor.attachment_id.clone();
            // 测试专用篡改：直接改写密封列（绕过存储 API），模拟磁盘被改写。
            {
                let guard = store.store.lock().expect("store lock");
                guard
                    .connection()
                    .execute(
                        "UPDATE attachment_secrets SET protected_secret = X'00ff' WHERE attachment_id = ?1",
                        [&id],
                    )
                    .expect("tamper secret");
            }
            id
        };
        // 重启（进程内没有别的东西可依赖）：读取密封密钥必须显式 STATE_CORRUPTED。
        let store = PersistedAttachmentStore::open(&cache_root, formal_store(&db_path))
            .expect("reopen persisted store");
        assert_eq!(
            store
                .attachment_record(&attachment_id)
                .expect("record is still readable")
                .expect("present")
                .state,
            "imported"
        );
        let destination = dir.path("tampered.out");
        let error = store
            .export_attachment(&attachment_id, &FileToken::from_local_path(&destination))
            .unwrap_err();
        assert_eq!(error.code(), "STATE_CORRUPTED");
        assert!(!destination.exists(), "tampered secret must not produce a file");
    }

    #[test]
    fn persisted_delete_removes_the_stored_secret() {
        let dir = TempDir::new("secret-delete");
        let source_path = dir.source("delete-secret.bin", &pattern(400, 7));
        let store =
            PersistedAttachmentStore::open(dir.path("cache"), formal_store(&dir.path("chat.db")))
                .expect("open persisted store");
        let descriptor = store
            .import_file(&FileToken::from_local_path(&source_path), "delete-secret.bin")
            .expect("import");
        let attachment_id = descriptor.attachment_id.clone();
        {
            let guard = store.store.lock().expect("store lock");
            assert!(guard
                .load_attachment_secret(&attachment_id)
                .expect("load secret")
                .is_some());
        }
        assert!(store.delete_attachment(&attachment_id).expect("delete"));
        let guard = store.store.lock().expect("store lock");
        let count: i64 = guard
            .connection()
            .query_row("SELECT COUNT(*) FROM attachment_secrets", [], |row| row.get(0))
            .expect("count secrets");
        assert_eq!(count, 0, "deleting an attachment must clear its file key");
        assert!(guard
            .load_attachment_secret(&attachment_id)
            .expect("load secret")
            .is_none());
    }

    #[test]
    fn persisted_export_secret_descriptor_roundtrip() {
        let dir = TempDir::new("secret-descriptor");
        let source_path = dir.source("descriptor.bin", &pattern(900, 8));
        let store =
            PersistedAttachmentStore::open(dir.path("cache"), formal_store(&dir.path("chat.db")))
                .expect("open persisted store");
        let descriptor = store
            .import_file(&FileToken::from_local_path(&source_path), "descriptor.bin")
            .expect("import");
        let attachment_id = descriptor.attachment_id.clone();
        let mut material = store
            .cache
            .secret_material(&attachment_id)
            .expect("secret material");
        let file_key = FileKey::try_from_slice(&material.file_key).expect("file key");
        let wrapping_key = [0x5au8; 32];
        let wrapped = store
            .export_secret_descriptor(&attachment_id, &wrapping_key)
            .expect("export descriptor");
        assert!(!contains(wrapped.as_bytes(), &material.file_key));
        let opened = chat_crypto::secret_descriptor::open_secret_descriptor(
            &wrapping_key,
            &attachment_id,
            &wrapped,
        )
        .expect("open descriptor");
        assert!(opened.equals(&file_key), "descriptor must wrap the same file key");
        // 错包装密钥与错附件 ID 都显式拒绝。
        assert_eq!(
            chat_crypto::secret_descriptor::open_secret_descriptor(
                &[0x5bu8; 32],
                &attachment_id,
                &wrapped
            )
            .unwrap_err()
            .code(),
            "ATTACHMENT_CORRUPTED"
        );
        assert_eq!(
            chat_crypto::secret_descriptor::open_secret_descriptor(
                &wrapping_key,
                "att-00000000000000000000000000000000",
                &wrapped
            )
            .unwrap_err()
            .code(),
            "ATTACHMENT_CORRUPTED"
        );
        material.file_key.fill(0);
    }

    #[test]
    fn persisted_export_compares_record_with_cache() {
        let dir = TempDir::new("persisted-match");
        let store = PersistedAttachmentStore::open(
            dir.path("cache"),
            formal_store(&dir.path("chat.db")),
        )
        .expect("open persisted store");
        let source_path = dir.source("match.bin", &pattern(2048, 1));
        let descriptor = store
            .import_file(&FileToken::from_local_path(&source_path), "match.bin")
            .expect("import");

        // 篡改进程内缓存描述符的明文摘要：与持久化记录不一致 → 拒绝导出。
        {
            let mut records = store.cache.records.lock().expect("cache lock");
            let record = records.get_mut(&descriptor.attachment_id).expect("cached");
            record.descriptor.content_digest = "00".repeat(32);
        }
        let destination = dir.path("match.out");
        let error = store
            .export_attachment(&descriptor.attachment_id, &FileToken::from_local_path(&destination))
            .unwrap_err();
        assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");
        assert!(!destination.exists());
        // 畸形 ID 一律按不存在处理（不拼接路径）。
        assert_eq!(
            store
                .attachment_record("../../etc/passwd")
                .unwrap_err()
                .code(),
            "ATTACHMENT_NOT_FOUND"
        );
    }

    #[test]
    fn persisted_delete_removes_record_and_cache() {
        let dir = TempDir::new("persisted-delete");
        let store = PersistedAttachmentStore::open(
            dir.path("cache"),
            formal_store(&dir.path("chat.db")),
        )
        .expect("open persisted store");
        let source_path = dir.source("delete.bin", &pattern(512, 2));
        let descriptor = store
            .import_file(&FileToken::from_local_path(&source_path), "delete.bin")
            .expect("import");
        assert!(store
            .cache_root()
            .join(&descriptor.attachment_id)
            .is_dir());
        assert!(store.delete_attachment(&descriptor.attachment_id).expect("delete"));
        assert!(store
            .attachment_record(&descriptor.attachment_id)
            .expect("record")
            .is_none());
        assert!(!store.cache_root().join(&descriptor.attachment_id).exists());
        assert!(!store.delete_attachment(&descriptor.attachment_id).expect("delete again"));
        assert_eq!(
            store
                .attachment_status(&descriptor.attachment_id)
                .unwrap_err()
                .code(),
            "ATTACHMENT_NOT_FOUND"
        );
        assert!(store.attachment_records(10).expect("list").is_empty());
    }

    #[test]
    fn persisted_cancel_updates_record_and_survives_restart() {
        let dir = TempDir::new("persisted-cancel");
        let db_path = dir.path("chat.db");
        let cache_root = dir.path("cache");
        let source_path = dir.source("cancel.bin", &pattern(700, 3));
        let attachment_id;
        {
            let store = PersistedAttachmentStore::open(&cache_root, formal_store(&db_path))
                .expect("open persisted store");
            let descriptor = store
                .import_file(&FileToken::from_local_path(&source_path), "cancel.bin")
                .expect("import");
            attachment_id = descriptor.attachment_id.clone();
            let status = store.cancel_transfer(&attachment_id).expect("cancel");
            assert_eq!(status.state, AttachmentState::Cancelled);
            assert!(status.cancel_requested);
            let record = store.attachment_record(&attachment_id).unwrap().unwrap();
            assert_eq!(record.state, "cancelled");
            // 已取消再取消是幂等的。
            assert_eq!(
                store.cancel_transfer(&attachment_id).expect("cancel again").state,
                AttachmentState::Cancelled
            );
        }
        // 重开后记录仍是 cancelled；没有缓存时状态从记录派生。
        let store = PersistedAttachmentStore::open(&cache_root, formal_store(&db_path))
            .expect("reopen persisted store");
        let status = store.attachment_status(&attachment_id).expect("status");
        assert_eq!(status.state, AttachmentState::Cancelled);
        assert!(status.cancel_requested);
        assert_eq!(
            store.cancel_transfer(&attachment_id).expect("cancel after restart").state,
            AttachmentState::Cancelled
        );
    }

    #[test]
    fn persisted_import_cleans_cache_when_record_write_fails() {
        let dir = TempDir::new("persisted-fail");
        let db_path = dir.path("chat.db");
        let source_path = dir.source("fail.bin", &pattern(300, 5));
        // 注入故障：存储连接已 fail-closed，记录一定写不进去。
        let mut formal = formal_store(&db_path);
        formal.mark_faulted("injected: record store is fail-closed");
        let store = PersistedAttachmentStore::open(dir.path("cache"), formal)
            .expect("open persisted store");
        let error = store
            .import_file(&FileToken::from_local_path(&source_path), "fail.bin")
            .unwrap_err();
        assert_eq!(error.code(), "STORAGE_FAILURE");
        // 缓存被清理，不留“只有缓存没有记录”的半成品。
        let mut files = Vec::new();
        collect_files(store.cache_root(), &mut files);
        assert!(
            files.is_empty(),
            "failed record write must clean the attachment cache"
        );
        assert!(store.attachment_records(10).is_err());
    }


    // ------------------------------------------------------------ 块传输端口测试

    /// 内存块传输端口：可配置“第 N 块失败”、可篡改/缺失，便于验证中断与重试。
    #[derive(Default)]
    struct FakeBlobTransport {
        sessions: HashMap<String, FakeSession>,
        next_session: u64,
        fail_put_at: Option<u32>,
        fail_read_at: Option<u32>,
        puts: usize,
        reads: usize,
        completes: usize,
        completed_digest: Option<String>,
    }

    struct FakeSession {
        request: UploadRequest,
        chunks: Vec<Option<Vec<u8>>>,
    }

    impl FakeBlobTransport {
        fn failing_at(index: u32) -> Self {
            let mut transport = Self::default();
            transport.fail_put_at = Some(index);
            transport
        }

        fn session_for(&self, blob_id: &str) -> Option<&FakeSession> {
            self.sessions
                .values()
                .find(|session| session.request.attachment_id == blob_id)
        }

        fn stored_chunk(&self, blob_id: &str, index: u32) -> Option<&Vec<u8>> {
            self.session_for(blob_id)
                .and_then(|session| session.chunks.get(index as usize))
                .and_then(|chunk| chunk.as_ref())
        }

        fn tamper(&mut self, blob_id: &str, index: u32, bytes: &[u8]) -> bool {
            for session in self.sessions.values_mut() {
                if session.request.attachment_id == blob_id
                    && let Some(slot) = session.chunks.get_mut(index as usize)
                {
                    *slot = Some(bytes.to_vec());
                    return true;
                }
            }
            false
        }

        fn drop_blob(&mut self, blob_id: &str) {
            self.sessions
                .retain(|_, session| session.request.attachment_id != blob_id);
        }
    }

    impl BlobTransport for FakeBlobTransport {
        fn begin_upload(&mut self, request: UploadRequest) -> AttachmentResult<UploadSession> {
            self.next_session = self.next_session.saturating_add(1);
            let session_id = format!("sess-{}", self.next_session);
            let slots = vec![None; request.chunk_count as usize];
            self.sessions.insert(
                session_id.clone(),
                FakeSession {
                    request,
                    chunks: slots,
                },
            );
            Ok(UploadSession::new(session_id))
        }

        fn put_chunk(&mut self, session: &str, index: u32, bytes: &[u8]) -> AttachmentResult<()> {
            if self.fail_put_at == Some(index) {
                return Err(AttachmentError::transport(
                    "NODE_UNAVAILABLE",
                    "fake node rejects the chunk",
                ));
            }
            // 先确认会话存在：过期会话不算“节点已接受的块”（puts 只统计真正写入的块）。
            if !self.sessions.contains_key(session) {
                return Err(AttachmentError::transport(
                    "UPLOAD_EXPIRED",
                    "fake upload session is gone",
                ));
            }
            self.puts = self.puts.saturating_add(1);
            let entry = self.sessions.get_mut(session).ok_or_else(|| {
                AttachmentError::transport("UPLOAD_EXPIRED", "fake upload session is gone")
            })?;
            let slot = entry.chunks.get_mut(index as usize).ok_or_else(|| {
                AttachmentError::transport("INVALID_TARGET", "chunk index is out of range")
            })?;
            // 幂等：同一 (session, index) 重复写同一字节只保留一份。
            *slot = Some(bytes.to_vec());
            Ok(())
        }

        fn complete(
            &mut self,
            session: &str,
            total_ciphertext_digest: &str,
        ) -> AttachmentResult<()> {
            self.completes = self.completes.saturating_add(1);
            let complete = {
                let entry = self.sessions.get(session).ok_or_else(|| {
                    AttachmentError::transport("UPLOAD_EXPIRED", "fake upload session is gone")
                })?;
                entry.chunks.iter().all(|chunk| chunk.is_some())
            };
            if !complete {
                return Err(AttachmentError::transport(
                    "INVALID_TARGET",
                    "fake node is missing chunks",
                ));
            }
            self.completed_digest = Some(total_ciphertext_digest.to_owned());
            Ok(())
        }

        fn read_chunk(&mut self, blob_id: &str, index: u32) -> AttachmentResult<Vec<u8>> {
            if self.fail_read_at == Some(index) {
                return Err(AttachmentError::transport(
                    "NODE_UNAVAILABLE",
                    "fake node rejects the read",
                ));
            }
            self.reads = self.reads.saturating_add(1);
            self.stored_chunk(blob_id, index)
                .cloned()
                .ok_or(AttachmentError::AttachmentNotFound)
        }
    }

    /// 三块的测试源（2 * CHUNK_BYTES + 777），保证能验证“第 3 块失败”。
    fn upload_source(dir: &TempDir, name: &str) -> (PathBuf, Vec<u8>) {
        let mut source = pattern(2 * CHUNK_BYTES + 777, 9);
        source.extend_from_slice(b"ATTACHMENT-UPLOAD-PLAINTEXT-MARKER");
        let path = dir.source(name, &source);
        (path, source)
    }

    #[test]
    fn upload_sends_the_cached_ciphertext_chunks_verbatim() {
        let dir = TempDir::new("upload-ok");
        let (source_path, source) = upload_source(&dir, "upload.bin");
        let store =
            PersistedAttachmentStore::open(dir.path("cache"), formal_store(&dir.path("chat.db")))
                .expect("open persisted store");
        let descriptor = store
            .import_file(&FileToken::from_local_path(&source_path), "上传.bin")
            .expect("import");
        let manifest = store
            .cache
            .attachment_manifest(&descriptor.attachment_id)
            .expect("manifest");
        let mut transport = FakeBlobTransport::default();
        let status = store
            .upload_attachment(&descriptor.attachment_id, &mut transport)
            .expect("upload");

        assert_eq!(status.state, AttachmentState::Available);
        assert_eq!(status.transferred_chunks, descriptor.chunk_count);
        assert_eq!(status.available_chunks, descriptor.chunk_count);
        assert_eq!(transport.completes, 1);
        assert_eq!(transport.completed_digest.as_deref(), Some(manifest.ciphertext_digest.as_str()));
        let session = transport
            .session_for(&descriptor.attachment_id)
            .expect("fake node has the blob");
        assert_eq!(session.request.plain_size, descriptor.plain_size);
        assert_eq!(session.request.chunk_count, descriptor.chunk_count);
        assert_eq!(session.request.chunk_size, descriptor.chunk_size);
        assert_eq!(session.request.ciphertext_digest, manifest.ciphertext_digest);
        assert_eq!(session.request.ciphertext_bytes, manifest.ciphertext_bytes);

        // 节点收到的每一块必须与本地缓存密文逐字节一致。
        for index in 0..descriptor.chunk_count {
            let local = store
                .cache
                .ciphertext_chunk(&descriptor.attachment_id, index)
                .expect("local ciphertext chunk");
            let remote = transport
                .stored_chunk(&descriptor.attachment_id, index)
                .expect("node ciphertext chunk");
            assert_eq!(local, *remote, "chunk {index} must match byte for byte");
            assert!(
                !contains(&local, &source),
                "the node must never receive plaintext (chunk {index})"
            );
        }
        let record = store
            .attachment_record(&descriptor.attachment_id)
            .expect("record")
            .expect("record exists");
        assert_eq!(record.state, "available");

        // 重复上传幂等：不再建会话、不再发块。
        let puts_before = transport.puts;
        let completes_before = transport.completes;
        let repeated = store
            .upload_attachment(&descriptor.attachment_id, &mut transport)
            .expect("idempotent upload");
        assert_eq!(repeated.state, AttachmentState::Available);
        assert_eq!(transport.puts, puts_before, "an available attachment is not re-uploaded");
        assert_eq!(transport.completes, completes_before);
    }

    #[test]
    fn upload_failure_keeps_progress_and_retry_is_idempotent() {
        let dir = TempDir::new("upload-retry");
        let (source_path, _source) = upload_source(&dir, "retry.bin");
        let store =
            PersistedAttachmentStore::open(dir.path("cache"), formal_store(&dir.path("chat.db")))
                .expect("open persisted store");
        let descriptor = store
            .import_file(&FileToken::from_local_path(&source_path), "重试.bin")
            .expect("import");
        let manifest = store
            .cache
            .attachment_manifest(&descriptor.attachment_id)
            .expect("manifest");

        // 第 3 块失败（0 起）：前 2 块已发出，进度与缓存都必须保留。
        let mut failing = FakeBlobTransport::failing_at(2);
        let error = store
            .upload_attachment(&descriptor.attachment_id, &mut failing)
            .expect_err("the fake node rejects the third chunk");
        assert_eq!(error.code(), "NODE_UNAVAILABLE");
        assert_eq!(failing.puts, 2, "exactly the chunks before the failure were sent");
        assert_eq!(failing.completes, 0, "a failed upload must never complete");
        let status = store
            .attachment_status(&descriptor.attachment_id)
            .expect("status after failure");
        assert_eq!(status.state, AttachmentState::Uploading);
        assert_eq!(status.transferred_chunks, 2);
        assert_eq!(
            status.available_chunks, descriptor.chunk_count,
            "the local ciphertext cache must survive a failed upload"
        );
        assert_eq!(status.failure, Some("upload failed"));
        let record = store
            .attachment_record(&descriptor.attachment_id)
            .expect("record")
            .expect("record exists");
        assert_eq!(record.state, "uploading", "state stays retryable");

        // 重试：新会话、全部块重发（同字节），成功后 available。
        let mut retry = FakeBlobTransport::default();
        let uploaded = store
            .upload_attachment(&descriptor.attachment_id, &mut retry)
            .expect("retry succeeds");
        assert_eq!(uploaded.state, AttachmentState::Available);
        assert_eq!(retry.puts, descriptor.chunk_count as usize);
        for index in 0..descriptor.chunk_count {
            let local = store
                .cache
                .ciphertext_chunk(&descriptor.attachment_id, index)
                .expect("local chunk");
            assert_eq!(
                retry.stored_chunk(&descriptor.attachment_id, index),
                Some(&local),
                "retry must reuse the same ciphertext bytes"
            );
        }
        let record = store
            .attachment_record(&descriptor.attachment_id)
            .expect("record")
            .expect("record exists");
        assert_eq!(record.state, "available");
        assert_eq!(record.ciphertext_digest, manifest.ciphertext_digest);
    }

    #[test]
    fn download_roundtrip_matches_the_original_plaintext() {
        let dir = TempDir::new("download-ok");
        let (source_path, source) = upload_source(&dir, "roundtrip.bin");
        let store =
            PersistedAttachmentStore::open(dir.path("cache"), formal_store(&dir.path("chat.db")))
                .expect("open persisted store");
        let descriptor = store
            .import_file(&FileToken::from_local_path(&source_path), "往返.bin")
            .expect("import");
        let manifest = store
            .cache
            .attachment_manifest(&descriptor.attachment_id)
            .expect("manifest");
        let mut transport = FakeBlobTransport::default();
        store
            .upload_attachment(&descriptor.attachment_id, &mut transport)
            .expect("upload");

        let destination = dir.path("downloaded.bin");
        let receipt = store
            .download_attachment(
                &descriptor.attachment_id,
                &manifest,
                &FileToken::from_local_path(&destination),
                &mut transport,
            )
            .expect("download");
        assert_eq!(receipt.attachment_id, descriptor.attachment_id);
        assert_eq!(receipt.written_bytes, source.len() as u64);
        assert_eq!(receipt.content_digest, descriptor.content_digest);
        assert_eq!(fs::read(&destination).expect("read target"), source);
        assert_eq!(transport.reads, descriptor.chunk_count as usize);
        let record = store
            .attachment_record(&descriptor.attachment_id)
            .expect("record")
            .expect("record exists");
        assert_eq!(record.state, "available");
    }

    #[test]
    fn download_rejects_tampered_chunks_and_leaves_no_partial_file() {
        let dir = TempDir::new("download-tamper");
        let (source_path, _source) = upload_source(&dir, "tamper.bin");
        let store =
            PersistedAttachmentStore::open(dir.path("cache"), formal_store(&dir.path("chat.db")))
                .expect("open persisted store");
        let descriptor = store
            .import_file(&FileToken::from_local_path(&source_path), "篡改.bin")
            .expect("import");
        let manifest = store
            .cache
            .attachment_manifest(&descriptor.attachment_id)
            .expect("manifest");
        let mut transport = FakeBlobTransport::default();
        store
            .upload_attachment(&descriptor.attachment_id, &mut transport)
            .expect("upload");
        assert!(transport.tamper(&descriptor.attachment_id, 1, b"tampered ciphertext"));

        let destination = dir.path("tampered.out");
        let error = store
            .download_attachment(
                &descriptor.attachment_id,
                &manifest,
                &FileToken::from_local_path(&destination),
                &mut transport,
            )
            .expect_err("a tampered chunk must be rejected");
        assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");
        assert!(!destination.exists(), "no unverified file may be written");
        assert!(
            !dir.path("tampered.out.download-partial").exists(),
            "the temporary file must be removed on failure"
        );
        let record = store
            .attachment_record(&descriptor.attachment_id)
            .expect("record")
            .expect("record exists");
        assert_eq!(record.state, "downloading", "a failed download stays retryable");
    }

    #[test]
    fn download_rejects_a_wrong_manifest_before_reading_any_chunk() {
        let dir = TempDir::new("download-manifest");
        let (source_path, _source) = upload_source(&dir, "manifest.bin");
        let store =
            PersistedAttachmentStore::open(dir.path("cache"), formal_store(&dir.path("chat.db")))
                .expect("open persisted store");
        let descriptor = store
            .import_file(&FileToken::from_local_path(&source_path), "清单.bin")
            .expect("import");
        let manifest = store
            .cache
            .attachment_manifest(&descriptor.attachment_id)
            .expect("manifest");
        let mut transport = FakeBlobTransport::default();
        store
            .upload_attachment(&descriptor.attachment_id, &mut transport)
            .expect("upload");
        transport.reads = 0;

        let mut wrong_digest = manifest.clone();
        let mut digest = wrong_digest.ciphertext_digest.clone();
        let last = digest.pop().expect("digest is not empty");
        digest.push(if last == '0' { '1' } else { '0' });
        wrong_digest.ciphertext_digest = digest;
        let error = store
            .download_attachment(
                &descriptor.attachment_id,
                &wrong_digest,
                &FileToken::from_local_path(&dir.path("wrong-digest.out")),
                &mut transport,
            )
            .expect_err("a divergent manifest must be rejected");
        assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");
        assert_eq!(transport.reads, 0, "the manifest is checked before any network read");

        let mut wrong_owner = manifest.clone();
        wrong_owner.attachment_id = "att-00000000000000000000000000000000".to_owned();
        let error = store
            .download_attachment(
                &descriptor.attachment_id,
                &wrong_owner,
                &FileToken::from_local_path(&dir.path("wrong-owner.out")),
                &mut transport,
            )
            .expect_err("a manifest for another attachment must be rejected");
        assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");
        assert_eq!(transport.reads, 0);
    }

    #[test]
    fn download_works_after_restart_and_rejects_unknown_blobs() {
        let dir = TempDir::new("download-unknown");
        let (source_path, source_bytes) = upload_source(&dir, "unknown.bin");
        let db_path = dir.path("chat.db");
        let cache_root = dir.path("cache");
        let (attachment_id, manifest) = {
            let store = PersistedAttachmentStore::open(&cache_root, formal_store(&db_path))
                .expect("open persisted store");
            let descriptor = store
                .import_file(&FileToken::from_local_path(&source_path), "未知.bin")
                .expect("import");
            let manifest = store
                .cache
                .attachment_manifest(&descriptor.attachment_id)
                .expect("manifest");
            let mut transport = FakeBlobTransport::default();
            store
                .upload_attachment(&descriptor.attachment_id, &mut transport)
                .expect("upload");
            // 节点侧记录消失：读取必须显式失败，不能返回空字节。
            transport.drop_blob(&descriptor.attachment_id);
            let error = store
                .download_attachment(
                    &descriptor.attachment_id,
                    &manifest,
                    &FileToken::from_local_path(&dir.path("gone.out")),
                    &mut transport,
                )
                .expect_err("a missing blob must be rejected");
            assert_eq!(error.code(), "ATTACHMENT_NOT_FOUND");
            // 完全未知的附件（合法 ID 形式，但没有记录）。
            let error = store
                .download_attachment(
                    "att-00000000000000000000000000000000",
                    &manifest,
                    &FileToken::from_local_path(&dir.path("unknown.out")),
                    &mut transport,
                )
                .expect_err("an unknown attachment must be rejected");
            assert_eq!(error.code(), "ATTACHMENT_NOT_FOUND");
            (descriptor.attachment_id, manifest)
        };

        // “重启”：进程内 FileKey 消失，但 attachment_secrets 里有，下载必须真实成功。
        let reopened =
            PersistedAttachmentStore::open(&cache_root, formal_store(&db_path)).expect("reopen");
        // 节点侧仍有已发布密文（用缓存里的块播种一个假节点，模拟“同一 blob 还在”）。
        let mut transport = FakeBlobTransport::default();
        let session = transport
            .begin_upload(UploadRequest {
                attachment_id: attachment_id.clone(),
                plain_size: manifest.plain_size,
                chunk_size: manifest.chunk_size,
                chunk_count: manifest.chunk_count,
                ciphertext_bytes: manifest.ciphertext_bytes,
                ciphertext_digest: manifest.ciphertext_digest.clone(),
            })
            .expect("seed begin");
        for index in 0..manifest.chunk_count {
            let chunk = reopened
                .cache
                .ciphertext_chunk(&attachment_id, index)
                .expect("cached ciphertext chunk");
            transport
                .put_chunk(session.as_str(), index, &chunk)
                .expect("seed chunk");
        }
        transport
            .complete(session.as_str(), &manifest.ciphertext_digest)
            .expect("seed complete");
        let destination = dir.path("after-restart.out");
        let receipt = reopened
            .download_attachment(
                &attachment_id,
                &manifest,
                &FileToken::from_local_path(&destination),
                &mut transport,
            )
            .expect("download after restart must succeed");
        assert_eq!(receipt.written_bytes, source_bytes.len() as u64);
        assert_eq!(fs::read(&destination).expect("read download"), source_bytes);
        assert_eq!(transport.reads as u32, manifest.chunk_count);

        // 密钥记录被删掉后（新进程、无缓存）：显式 ATTACHMENT_KEY_UNAVAILABLE 且不读字节。
        drop(reopened);
        let reopened_again =
            PersistedAttachmentStore::open(&cache_root, formal_store(&db_path)).expect("reopen again");
        {
            let mut guard = reopened_again.store.lock().expect("formal store lock");
            assert!(guard
                .delete_attachment_secret(&attachment_id)
                .expect("delete secret"));
        }
        let mut transport = FakeBlobTransport::default();
        let error = reopened_again
            .download_attachment(
                &attachment_id,
                &manifest,
                &FileToken::from_local_path(&dir.path("no-key.out")),
                &mut transport,
            )
            .expect_err("the file key must be gone");
        assert_eq!(error.code(), "ATTACHMENT_KEY_UNAVAILABLE");
        assert!(!dir.path("no-key.out").exists());
        assert_eq!(transport.reads, 0, "no bytes are read without the file key");
    }


    #[test]
    fn download_fills_the_local_cache_for_offline_export() {
        let dir = TempDir::new("download-cache");
        let db_path = dir.path("chat.db");
        let cache_root = dir.path("cache");
        let (source_path, source) = upload_source(&dir, "cache.bin");
        let mut node = FakeBlobTransport::default();
        let attachment_id;
        let manifest;
        {
            let store = PersistedAttachmentStore::open(&cache_root, formal_store(&db_path))
                .expect("open persisted store");
            let descriptor = store
                .import_file(&FileToken::from_local_path(&source_path), "离线缓存.bin")
                .expect("import");
            attachment_id = descriptor.attachment_id.clone();
            manifest = store
                .cache
                .attachment_manifest(&attachment_id)
                .expect("manifest");
            store
                .upload_attachment(&attachment_id, &mut node)
                .expect("upload");
        }
        // 清掉本地缓存块：模拟“只有记录与 FileKey、没有缓存”的另一设备/新进程。
        fs::remove_dir_all(cache_root.join(&attachment_id)).expect("clear cache");
        let store = PersistedAttachmentStore::open(&cache_root, formal_store(&db_path))
            .expect("reopen persisted store");
        assert_eq!(
            store
                .cache
                .available_chunks(&attachment_id, manifest.chunk_count),
            0,
            "the cache starts empty"
        );

        let destination = dir.path("downloaded.bin");
        let receipt = store
            .download_attachment(
                &attachment_id,
                &manifest,
                &FileToken::from_local_path(&destination),
                &mut node,
            )
            .expect("download");
        assert_eq!(receipt.written_bytes, source.len() as u64);
        assert_eq!(fs::read(&destination).expect("read download"), source);

        // 下载后缓存块与节点密文逐字节一致，状态 available 且可用块数满。
        for index in 0..manifest.chunk_count {
            let cached = store
                .cache
                .ciphertext_chunk(&attachment_id, index)
                .expect("cached chunk");
            assert_eq!(
                Some(&cached),
                node.stored_chunk(&attachment_id, index),
                "cached ciphertext must match the node bytes"
            );
        }
        let status = store.attachment_status(&attachment_id).expect("status");
        assert_eq!(status.state, AttachmentState::Available);
        assert_eq!(status.available_chunks, manifest.chunk_count);
        assert_eq!(status.transferred_chunks, manifest.chunk_count);

        // 离线导出：不给任何块传输端口也能导出（缓存 + 持久 FileKey）。
        let offline = dir.path("offline.bin");
        let exported = store
            .export_attachment(&attachment_id, &FileToken::from_local_path(&offline))
            .expect("offline export after download");
        assert_eq!(exported.written_bytes, source.len() as u64);
        assert_eq!(fs::read(&offline).expect("read offline export"), source);
        assert_eq!(
            exported.content_digest,
            store
                .attachment_record(&attachment_id)
                .expect("record")
                .expect("present")
                .content_digest
        );
    }

    #[test]
    fn failed_download_rolls_back_new_cache_chunks() {
        let dir = TempDir::new("download-fail-cache");
        let db_path = dir.path("chat.db");
        let cache_root = dir.path("cache");
        let (source_path, _source) = upload_source(&dir, "fail-cache.bin");
        let mut node = FakeBlobTransport::default();
        let attachment_id;
        let manifest;
        {
            let store = PersistedAttachmentStore::open(&cache_root, formal_store(&db_path))
                .expect("open persisted store");
            let descriptor = store
                .import_file(&FileToken::from_local_path(&source_path), "失败不写缓存.bin")
                .expect("import");
            attachment_id = descriptor.attachment_id.clone();
            manifest = store
                .cache
                .attachment_manifest(&attachment_id)
                .expect("manifest");
            store
                .upload_attachment(&attachment_id, &mut node)
                .expect("upload");
        }
        fs::remove_dir_all(cache_root.join(&attachment_id)).expect("clear cache");
        let store = PersistedAttachmentStore::open(&cache_root, formal_store(&db_path))
            .expect("reopen persisted store");
        // 第 2 块被篡改：分块 AAD 认证失败属终态错误。
        assert!(
            node.tamper(&attachment_id, 1, b"tampered-ciphertext-chunk"),
            "the fake node must own the blob"
        );
        let destination = dir.path("failed.bin");
        let error = store
            .download_attachment(
                &attachment_id,
                &manifest,
                &FileToken::from_local_path(&destination),
                &mut node,
            )
            .expect_err("a tampered chunk must fail");
        assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");
        assert!(!destination.exists(), "no unverified output");
        assert_eq!(
            store
                .cache
                .available_chunks(&attachment_id, manifest.chunk_count),
            0,
            "a failed download must not leave new cache chunks"
        );
        assert!(
            store
                .load_transfer_progress(&attachment_id, "download")
                .expect("progress")
                .is_none(),
            "a terminal failure clears the transfer progress"
        );
        assert!(!dir.path("failed.bin.download-partial").exists());
    }

    #[test]
    fn upload_resumes_after_restart_and_sends_only_missing_chunks() {
        let dir = TempDir::new("upload-resume");
        let db_path = dir.path("chat.db");
        let cache_root = dir.path("cache");
        let (source_path, _source) = upload_source(&dir, "resume.bin");
        let attachment_id;
        let chunk_count;
        let mut transport = FakeBlobTransport::failing_at(2);
        {
            let store = PersistedAttachmentStore::open(&cache_root, formal_store(&db_path))
                .expect("open persisted store");
            let descriptor = store
                .import_file(&FileToken::from_local_path(&source_path), "续传.bin")
                .expect("import");
            attachment_id = descriptor.attachment_id.clone();
            chunk_count = descriptor.chunk_count;
            assert_eq!(chunk_count, 3, "the fixture must have three chunks");
            let error = store
                .upload_attachment(&attachment_id, &mut transport)
                .expect_err("the third chunk fails");
            assert_eq!(error.code(), "NODE_UNAVAILABLE");
            assert_eq!(transport.puts, 2, "only the first two chunks reached the node");
            // 进度在失败后必须已持久化（位图 2/3 + 会话）。
            let progress = store
                .load_transfer_progress(&attachment_id, "upload")
                .expect("load progress")
                .expect("progress persisted");
            assert_eq!(progress.confirmed_count(), 2);
            assert_eq!(progress.chunk_count, chunk_count);
            assert!(progress.session_id.is_some(), "the upload session is persisted");
        }

        // 重启（同库同主密钥 + 同一个假节点会话仍活着）：只补发剩余 1 块。
        transport.fail_put_at = None;
        let store = PersistedAttachmentStore::open(&cache_root, formal_store(&db_path))
            .expect("reopen persisted store");
        let puts_before = transport.puts;
        let completes_before = transport.completes;
        let status = store
            .upload_attachment(&attachment_id, &mut transport)
            .expect("resume upload");
        assert_eq!(status.state, AttachmentState::Available);
        assert_eq!(
            transport.puts - puts_before,
            1,
            "only the missing chunk is re-sent after restart"
        );
        assert_eq!(transport.completes - completes_before, 1);
        // 节点收到的全部块仍与本地密文逐字节一致。
        for index in 0..chunk_count {
            let local = store
                .cache
                .ciphertext_chunk(&attachment_id, index)
                .expect("local ciphertext chunk");
            assert_eq!(
                transport.stored_chunk(&attachment_id, index),
                Some(&local),
                "chunk {index} must match after resumption"
            );
        }
        // 完成后进度必须清除。
        assert!(
            store
                .load_transfer_progress(&attachment_id, "upload")
                .expect("load progress")
                .is_none(),
            "a completed upload clears its progress"
        );
        // 重复上传仍然幂等无发送。
        let puts_after = transport.puts;
        store
            .upload_attachment(&attachment_id, &mut transport)
            .expect("repeat upload");
        assert_eq!(transport.puts, puts_after);
    }

    #[test]
    fn download_resumes_after_restart_and_reads_only_missing_chunks() {
        let dir = TempDir::new("download-resume");
        let db_path = dir.path("chat.db");
        let cache_root = dir.path("cache");
        let (source_path, source) = upload_source(&dir, "download-resume.bin");
        let destination = dir.path("resumed.bin");
        let attachment_id;
        let manifest;
        let mut node = FakeBlobTransport::default();
        {
            let store = PersistedAttachmentStore::open(&cache_root, formal_store(&db_path))
                .expect("open persisted store");
            let descriptor = store
                .import_file(&FileToken::from_local_path(&source_path), "下载续传.bin")
                .expect("import");
            attachment_id = descriptor.attachment_id.clone();
            manifest = store
                .cache
                .attachment_manifest(&attachment_id)
                .expect("manifest");
            store
                .upload_attachment(&attachment_id, &mut node)
                .expect("upload");
            // 第 3 块读取失败：前 2 块已解密写出，进度与半成品必须保留。
            node.fail_read_at = Some(2);
            let error = store
                .download_attachment(
                    &attachment_id,
                    &manifest,
                    &FileToken::from_local_path(&destination),
                    &mut node,
                )
                .expect_err("the third read fails");
            assert_eq!(error.code(), "NODE_UNAVAILABLE");
            let progress = store
                .load_transfer_progress(&attachment_id, "download")
                .expect("load progress")
                .expect("progress persisted");
            assert_eq!(progress.confirmed_count(), 2);
            assert!(progress.prefix_digest.is_some(), "the written prefix digest is persisted");
            assert!(
                dir.path("resumed.bin.download-partial").exists(),
                "the verified partial file is kept for resumption"
            );
            assert!(!destination.exists(), "no unverified output");
        }

        // 重启：只读剩余 1 块，输出与原文逐字节一致，进度清除。
        node.fail_read_at = None;
        let store = PersistedAttachmentStore::open(&cache_root, formal_store(&db_path))
            .expect("reopen persisted store");
        let reads_before = node.reads;
        let receipt = store
            .download_attachment(
                &attachment_id,
                &manifest,
                &FileToken::from_local_path(&destination),
                &mut node,
            )
            .expect("resume download");
        assert_eq!(
            node.reads - reads_before,
            1,
            "only the missing chunk is downloaded again"
        );
        assert_eq!(receipt.written_bytes, source.len() as u64);
        assert_eq!(fs::read(&destination).expect("read output"), source);
        assert!(store
            .load_transfer_progress(&attachment_id, "download")
            .expect("load progress")
            .is_none());
        assert!(!dir.path("resumed.bin.download-partial").exists());

        // 半成品被篡改后：拒绝续传并重下（不静默接受坏前缀）。
        {
            let mut guard = store.store.lock().expect("formal store lock");
            guard
                .save_attachment_progress(chat_storage::formal::AttachmentProgressInput {
                    attachment_id: attachment_id.clone(),
                    direction: "download".to_owned(),
                    chunk_count: manifest.chunk_count,
                    confirmed: vec![true, true, false],
                    session_id: None,
                    blob_id: Some(attachment_id.clone()),
                    prefix_digest: Some("00".repeat(32)),
                })
                .expect("seed bogus progress");
        }
        fs::write(
            dir.path("resumed.bin.download-partial"),
            b"tampered partial prefix",
        )
        .expect("write tampered partial");
        let receipt = store
            .download_attachment(
                &attachment_id,
                &manifest,
                &FileToken::from_local_path(&dir.path("resumed-again.bin")),
                &mut node,
            )
            .expect("a bad partial prefix restarts the download");
        assert_eq!(receipt.written_bytes, source.len() as u64);
        assert_eq!(fs::read(dir.path("resumed-again.bin")).expect("read"), source);
    }

    #[test]
    fn transfer_progress_corruption_is_state_corrupted() {
        let dir = TempDir::new("progress-corrupt");
        let (source_path, _source) = upload_source(&dir, "corrupt-progress.bin");
        let store =
            PersistedAttachmentStore::open(dir.path("cache"), formal_store(&dir.path("chat.db")))
                .expect("open persisted store");
        let descriptor = store
            .import_file(&FileToken::from_local_path(&source_path), "损坏进度.bin")
            .expect("import");
        let mut transport = FakeBlobTransport::failing_at(1);
        store
            .upload_attachment(&descriptor.attachment_id, &mut transport)
            .expect_err("the second chunk fails");
        // 测试专用篡改：直接改写密封列（绕过存储 API），模拟磁盘被改写。
        {
            let guard = store.store.lock().expect("store lock");
            guard
                .connection()
                .execute(
                    "UPDATE attachment_transfer_progress SET protected_progress = X'00ff'
                      WHERE attachment_id = ?1 AND direction = 'upload'",
                    [&descriptor.attachment_id],
                )
                .expect("tamper progress");
        }
        transport.fail_put_at = None;
        let error = store
            .upload_attachment(&descriptor.attachment_id, &mut transport)
            .expect_err("corrupted progress must fail closed");
        assert_eq!(error.code(), "STATE_CORRUPTED");
    }

}

