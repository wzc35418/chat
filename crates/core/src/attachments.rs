//! 附件本地导入/导出（ATTACHMENTS-001/002/004/006/007 的本地部分，不做网络）。
//!
//! 本轮交付的边界：
//! - 只做"本地加密缓存 + 本地导出"：不发起上传/下载，不写节点，不伪造传输成功；
//! - 每块明文用 CRYPTO-011 的 FileKey 独立认证加密，缓存目录里只有密文块；
//! - FileKey 与明文摘要只留在本进程内存与（未来的）受保护本地记录中，不进入公开描述符；
//! - 记录（附件 ID、状态、进度）是进程内状态机：重启后缓存视为未引用，由
//!   ATTACHMENTS-008 回收；正式持久化需要接入 storage 的 attachments 表（未实现）。
//!
//! 已知限制（详见 docs/implementation/attachments.md）：
//! - 未知大小流按 fail-closed 显式拒绝：分块 AAD 必须绑定 total_chunks，无法在不知道
//!   总数时安全导入，因此本实现要求来源声明大小（本地文件用 metadata 大小）；
//! - 上传/下载（ATTACHMENTS-003/005）、含 FileKey 的 E2EE SecretDescriptor、
//!   取消导出（EXPORT_CANCELLED）与持久化状态机均未实现。

use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use chat_crypto::attachment::{
    AttachmentCryptoError, CHUNK_BYTES, CipherChunk, FileId, FileKey, MAX_ATTACHMENT_BYTES,
    chunk_context_for, chunk_count, chunk_plain_length, decrypt_attachment_chunk,
    encrypt_attachment_chunk,
};
use sha2::{Digest, Sha256};

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
}

/// 附件层稳定错误码。安全校验失败必须显式失败，绝不降级继续。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
}

impl AttachmentError {
    /// 稳定错误码字符串。
    pub const fn code(self) -> &'static str {
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
}
