//! 附件记录的正式持久化（P1 增量）。
//!
//! 表结构与隐私边界（迁移 0005_attachments.sql，复用 0001 的 attachments 主表与密封列
//! protected_descriptor）：
//!
//! - 明文列只有不透明 attachment_id、state（白名单约束）、明文大小/块大小/块数与
//!   时间戳——与 TYPES.md 的公开清单同一级别；
//! - 敏感元数据（明文内容摘要、密文摘要、文件名展示值）用用途
//!   [`SealPurpose::AttachmentRecord`] 密封，AAD 绑定 attachment_id 与记录格式版本：
//!   把密文换到另一行、改版本或换主密钥都会认证失败（STATE_CORRUPTED）；
//! - **FileKey 不落盘**：内容密钥当前只在 core 的进程内 AttachmentStore 中，本层不保存
//!   任何可用于解密的材料，因此重启后导出必须显式失败，本层不提供伪造的可导出路径。
//!
//! 本层只做结构/边界/状态机校验，不做业务授权（附件可见性由 core 与 DOMAIN 负责）。

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::error::StorageError;
use super::seal::{ProtectedStorageKey, RecordSealer, SealPurpose};
use super::{
    CoreTransaction, Result, Store, WriteScope, commit_transaction, ensure_id, hex, now_ms,
    rollback_transaction,
};

/// 附件 ID 文本上限（字节）。
pub const MAX_ATTACHMENT_ID_BYTES: usize = 128;
/// 文件名展示值上限（字节，与 core 的 MAX_DISPLAY_NAME_BYTES 一致）。
pub const MAX_ATTACHMENT_DISPLAY_NAME_BYTES: usize = 200;
/// 单附件明文上限（字节，25 MiB，与 CRYPTO-011 一致）。
pub const MAX_ATTACHMENT_PLAIN_BYTES: u64 = 25 * 1024 * 1024;
/// 单块明文上限（字节）。
pub const MAX_ATTACHMENT_CHUNK_SIZE: u32 = 1024 * 1024;
/// 单附件分块数上限。
pub const MAX_ATTACHMENT_CHUNKS: u32 = 4096;
/// 附件记录条数上限（超过即显式拒绝，不静默丢弃）。
pub const MAX_ATTACHMENTS: u32 = 10_000;
/// 单次列表查询上限。
pub const MAX_ATTACHMENT_LIST: u32 = 512;
/// 摘要长度（字节）。
pub const ATTACHMENT_DIGEST_BYTES: usize = 32;

/// 附件记录格式版本（进入 AAD）。
const ATTACHMENT_AAD_VERSION: &[u8] = b"attachment-record-v1";
/// 密封元数据内部 schema 版本。
const ATTACHMENT_RECORD_VERSION: u8 = 1;
const DIGEST_HEX_LEN: usize = ATTACHMENT_DIGEST_BYTES * 2;

/// 允许的附件状态（与 attachments.state 的白名单一致）。
pub const ATTACHMENT_STATES: [&str; 7] = [
    "staging",
    "imported",
    "uploading",
    "available",
    "downloading",
    "failed",
    "cancelled",
];

/// 允许的状态迁移（同状态重复标记是幂等的，不需要列出）。
pub const ATTACHMENT_STATE_TRANSITIONS: &[(&str, &[&str])] = &[
    ("staging", &["imported", "failed", "cancelled"]),
    ("imported", &["uploading", "available", "failed", "cancelled"]),
    ("uploading", &["available", "failed", "cancelled"]),
    ("available", &["downloading"]),
    ("downloading", &["available", "failed", "cancelled"]),
    ("failed", &["imported", "cancelled"]),
    ("cancelled", &[]),
];

/// 附件记录写入输入。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentRecordInput {
    /// 本地附件 ID（不透明文本，core 生成）。
    pub attachment_id: String,
    /// 状态（见 [`ATTACHMENT_STATES`]）。
    pub state: String,
    /// 明文总字节数。
    pub plain_size: u64,
    /// 分块大小（字节）。
    pub chunk_size: u32,
    /// 分块总数。
    pub chunk_count: u32,
    /// 明文内容摘要（小写 hex，64 字符）。
    pub content_digest: String,
    /// 密文摘要（小写 hex，64 字符）。
    pub ciphertext_digest: String,
    /// 文件名展示值（1..=200 字节，不含控制字符）。
    pub display_name: String,
    /// 创建时间（Unix 毫秒；只在首次写入时使用）。
    pub created_at_ms: i64,
}

impl AttachmentRecordInput {
    /// 有界结构校验。
    pub fn validate(&self) -> Result<()> {
        validate_attachment_id(&self.attachment_id)?;
        validate_state(&self.state)?;
        if self.plain_size > MAX_ATTACHMENT_PLAIN_BYTES {
            return Err(StorageError::InvalidInput(format!(
                "attachment plain size exceeds {MAX_ATTACHMENT_PLAIN_BYTES} bytes"
            )));
        }
        if self.chunk_size == 0 || self.chunk_size > MAX_ATTACHMENT_CHUNK_SIZE {
            return Err(StorageError::InvalidInput(format!(
                "attachment chunk size must be 1..={MAX_ATTACHMENT_CHUNK_SIZE} bytes"
            )));
        }
        if self.chunk_count == 0 || self.chunk_count > MAX_ATTACHMENT_CHUNKS {
            return Err(StorageError::InvalidInput(format!(
                "attachment chunk count must be 1..={MAX_ATTACHMENT_CHUNKS}"
            )));
        }
        validate_digest("attachment content digest", &self.content_digest)?;
        validate_digest("attachment ciphertext digest", &self.ciphertext_digest)?;
        validate_display_name(&self.display_name)?;
        if self.created_at_ms < 0 {
            return Err(StorageError::InvalidInput(
                "attachment created_at_ms must be a non-negative timestamp".to_owned(),
            ));
        }
        Ok(())
    }

    fn same_content(&self, other: &AttachmentRecord) -> bool {
        self.plain_size == other.plain_size
            && self.chunk_size == other.chunk_size
            && self.chunk_count == other.chunk_count
            && self.content_digest == other.content_digest
            && self.ciphertext_digest == other.ciphertext_digest
            && self.display_name == other.display_name
    }
}

/// 附件记录投影（密封元数据已解密；**不含 FileKey**）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentRecord {
    /// 本地附件 ID。
    pub attachment_id: String,
    /// 状态。
    pub state: String,
    /// 明文总字节数。
    pub plain_size: u64,
    /// 分块大小。
    pub chunk_size: u32,
    /// 分块总数。
    pub chunk_count: u32,
    /// 明文内容摘要（小写 hex）。
    pub content_digest: String,
    /// 密文摘要（小写 hex）。
    pub ciphertext_digest: String,
    /// 文件名展示值。
    pub display_name: String,
    /// 创建时间。
    pub created_at_ms: i64,
    /// 最近更新时间。
    pub updated_at_ms: i64,
    /// 本次调用是否命中既有记录（读取路径恒为 true）。
    pub already_existed: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredAttachmentMetadata {
    version: u8,
    content_digest: String,
    ciphertext_digest: String,
    display_name: String,
}

impl StoredAttachmentMetadata {
    fn validate(&self) -> Result<()> {
        if self.version != ATTACHMENT_RECORD_VERSION {
            return Err(StorageError::StateCorrupted(
                "stored attachment metadata has an unsupported record version".to_owned(),
            ));
        }
        validate_digest("stored attachment content digest", &self.content_digest).map_err(|_| {
            StorageError::StateCorrupted(
                "stored attachment content digest is invalid".to_owned(),
            )
        })?;
        validate_digest("stored attachment ciphertext digest", &self.ciphertext_digest).map_err(
            |_| {
                StorageError::StateCorrupted(
                    "stored attachment ciphertext digest is invalid".to_owned(),
                )
            },
        )?;
        validate_display_name(&self.display_name).map_err(|_| {
            StorageError::StateCorrupted(
                "stored attachment display name is invalid".to_owned(),
            )
        })?;
        Ok(())
    }
}

fn validate_attachment_id(attachment_id: &str) -> Result<()> {
    ensure_id("attachment", attachment_id)?;
    if attachment_id.len() > MAX_ATTACHMENT_ID_BYTES {
        return Err(StorageError::InvalidInput(format!(
            "attachment ID exceeds {MAX_ATTACHMENT_ID_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_state(state: &str) -> Result<()> {
    if !ATTACHMENT_STATES.contains(&state) {
        return Err(StorageError::InvalidInput(
            "attachment state is not one of the allowed states".to_owned(),
        ));
    }
    Ok(())
}

fn transition_allowed(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    ATTACHMENT_STATE_TRANSITIONS
        .iter()
        .find(|(state, _)| *state == from)
        .is_some_and(|(_, allowed)| allowed.contains(&to))
}

fn validate_digest(label: &str, value: &str) -> Result<()> {
    let valid = value.len() == DIGEST_HEX_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !valid {
        return Err(StorageError::InvalidInput(format!(
            "{label} must be {DIGEST_HEX_LEN} lowercase hex characters"
        )));
    }
    Ok(())
}

fn validate_display_name(display_name: &str) -> Result<()> {
    if display_name.is_empty() || display_name.len() > MAX_ATTACHMENT_DISPLAY_NAME_BYTES {
        return Err(StorageError::InvalidInput(format!(
            "attachment display name must be 1..={MAX_ATTACHMENT_DISPLAY_NAME_BYTES} bytes"
        )));
    }
    if display_name.chars().any(char::is_control) {
        return Err(StorageError::InvalidInput(
            "attachment display name must not contain control characters".to_owned(),
        ));
    }
    Ok(())
}

fn attachment_aad(attachment_id: &str) -> [&[u8]; 2] {
    [attachment_id.as_bytes(), ATTACHMENT_AAD_VERSION]
}

fn seal_metadata(
    key: &ProtectedStorageKey,
    attachment_id: &str,
    metadata: &StoredAttachmentMetadata,
) -> Result<Vec<u8>> {
    let plaintext = serde_json::to_vec(metadata).map_err(|error| {
        StorageError::Failure(format!("cannot encode attachment metadata: {error}"))
    })?;
    let plaintext = Zeroizing::new(plaintext);
    let sealer = RecordSealer::new(key, SealPurpose::AttachmentRecord);
    sealer.seal(&attachment_aad(attachment_id), plaintext.as_slice())
}

fn open_metadata(
    key: &ProtectedStorageKey,
    attachment_id: &str,
    sealed: &[u8],
) -> Result<StoredAttachmentMetadata> {
    let sealer = RecordSealer::new(key, SealPurpose::AttachmentRecord);
    let plaintext = sealer.open(&attachment_aad(attachment_id), sealed)?;
    let metadata: StoredAttachmentMetadata =
        serde_json::from_slice(plaintext.as_slice()).map_err(|_| {
            StorageError::StateCorrupted(
                "stored attachment metadata is not readable".to_owned(),
            )
        })?;
    metadata.validate()?;
    Ok(metadata)
}

fn load_attachment_row(
    connection: &Connection,
    key: &ProtectedStorageKey,
    attachment_id: &str,
) -> Result<Option<AttachmentRecord>> {
    let row: Option<(String, i64, i64, Vec<u8>, i64, i64)> = connection
        .query_row(
            "SELECT state, plain_size, chunk_count, protected_descriptor, chunk_size, created_at
               FROM attachments WHERE attachment_id = ?1",
            [attachment_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((state, plain_size, chunk_count, sealed, chunk_size, created_at)) = row else {
        return Ok(None);
    };
    if plain_size < 0 || chunk_count <= 0 || chunk_size <= 0 || created_at < 0 {
        return Err(StorageError::StateCorrupted(
            "stored attachment record has invalid numeric columns".to_owned(),
        ));
    }
    let updated_at: i64 = connection
        .query_row(
            "SELECT updated_at FROM attachments WHERE attachment_id = ?1",
            [attachment_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| {
            StorageError::StateCorrupted("stored attachment record disappeared".to_owned())
        })?;
    if updated_at < 0 {
        return Err(StorageError::StateCorrupted(
            "stored attachment record has a negative update timestamp".to_owned(),
        ));
    }
    validate_state(&state).map_err(|_| {
        StorageError::StateCorrupted("stored attachment state is not allowed".to_owned())
    })?;
    if chunk_count > i64::from(MAX_ATTACHMENT_CHUNKS) {
        return Err(StorageError::StateCorrupted(
            "stored attachment chunk count exceeds the supported bound".to_owned(),
        ));
    }
    if u64::try_from(plain_size).unwrap_or(u64::MAX) > MAX_ATTACHMENT_PLAIN_BYTES {
        return Err(StorageError::StateCorrupted(
            "stored attachment plain size exceeds the supported bound".to_owned(),
        ));
    }
    if chunk_size > i64::from(MAX_ATTACHMENT_CHUNK_SIZE) {
        return Err(StorageError::StateCorrupted(
            "stored attachment chunk size exceeds the supported bound".to_owned(),
        ));
    }
    let metadata = open_metadata(key, attachment_id, &sealed)?;
    Ok(Some(AttachmentRecord {
        attachment_id: attachment_id.to_owned(),
        state,
        plain_size: plain_size as u64,
        chunk_size: chunk_size as u32,
        chunk_count: chunk_count as u32,
        content_digest: metadata.content_digest,
        ciphertext_digest: metadata.ciphertext_digest,
        display_name: metadata.display_name,
        created_at_ms: created_at,
        updated_at_ms: updated_at,
        already_existed: true,
    }))
}

fn save_attachment_inner(
    transaction: &mut CoreTransaction<'_>,
    input: &AttachmentRecordInput,
) -> Result<AttachmentRecord> {
    if let Some(existing) =
        load_attachment_row(transaction.connection(), transaction.key, &input.attachment_id)?
    {
        // 幂等：内容一致时返回既有记录（状态由 mark_attachment_state 推进，不被覆盖）。
        if input.same_content(&existing) {
            return Ok(existing);
        }
        return Err(StorageError::OperationIdConflict(
            "attachment ID is already bound to different content".to_owned(),
        ));
    }
    let count: i64 =
        transaction
            .connection()
            .query_row("SELECT COUNT(*) FROM attachments", [], |row| row.get(0))?;
    if u64::try_from(count).unwrap_or(u64::MAX) >= u64::from(MAX_ATTACHMENTS) {
        return Err(StorageError::InvalidInput(format!(
            "attachment record limit of {MAX_ATTACHMENTS} reached"
        )));
    }
    let metadata = StoredAttachmentMetadata {
        version: ATTACHMENT_RECORD_VERSION,
        content_digest: input.content_digest.clone(),
        ciphertext_digest: input.ciphertext_digest.clone(),
        display_name: input.display_name.clone(),
    };
    let sealed = seal_metadata(transaction.key, &input.attachment_id, &metadata)?;
    let now = now_ms();
    // 创建时间按调用方给出的值保存（已校验非负），不静默改写调用方语义。
    let created_at = input.created_at_ms;
    transaction.transaction.execute(
        "INSERT INTO attachments
             (attachment_id, protected_descriptor, plain_size, chunk_count, state, chunk_size, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            input.attachment_id,
            sealed,
            i64::try_from(input.plain_size).unwrap_or(i64::MAX),
            i64::from(input.chunk_count),
            input.state,
            i64::from(input.chunk_size),
            created_at,
            now
        ],
    )?;
    transaction.note(1);
    Ok(AttachmentRecord {
        attachment_id: input.attachment_id.clone(),
        state: input.state.clone(),
        plain_size: input.plain_size,
        chunk_size: input.chunk_size,
        chunk_count: input.chunk_count,
        content_digest: input.content_digest.clone(),
        ciphertext_digest: input.ciphertext_digest.clone(),
        display_name: input.display_name.clone(),
        created_at_ms: created_at,
        updated_at_ms: now,
        already_existed: false,
    })
}

impl Store {
    /// 保存（或幂等重放）一条附件记录。
    ///
    /// 同一 attachment_id 重复保存时，只有内容完全一致才返回既有记录；内容不同返回
    /// OPERATION_ID_CONFLICT，绝不静默覆盖。状态迁移必须走 [`Self::mark_attachment_state`]。
    pub fn save_attachment(&mut self, input: AttachmentRecordInput) -> Result<AttachmentRecord> {
        self.ensure_ready()?;
        input.validate()?;
        let mut transaction = self.begin_transaction(WriteScope::attachments())?;
        match save_attachment_inner(&mut transaction, &input) {
            Ok(record) => {
                commit_transaction(transaction)?;
                Ok(record)
            }
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }

    /// 读取一条附件记录；不存在返回 None，损坏返回 STATE_CORRUPTED。
    pub fn load_attachment(&self, attachment_id: &str) -> Result<Option<AttachmentRecord>> {
        self.ensure_ready()?;
        validate_attachment_id(attachment_id)?;
        load_attachment_row(self.connection(), &self.key, attachment_id)
    }

    /// 按创建时间倒序列出附件记录（limit 1..=MAX_ATTACHMENT_LIST）。
    pub fn list_attachments(&self, limit: u32) -> Result<Vec<AttachmentRecord>> {
        self.ensure_ready()?;
        if limit == 0 || limit > MAX_ATTACHMENT_LIST {
            return Err(StorageError::InvalidInput(format!(
                "attachment list limit must be 1..={MAX_ATTACHMENT_LIST}"
            )));
        }
        let mut statement = self.connection().prepare(
            "SELECT attachment_id FROM attachments
             ORDER BY created_at DESC, attachment_id ASC LIMIT ?1",
        )?;
        let rows = statement.query_map([i64::from(limit)], |row| row.get::<_, String>(0))?;
        let mut records = Vec::new();
        for row in rows {
            let attachment_id = row?;
            let record = load_attachment_row(self.connection(), &self.key, &attachment_id)?
                .ok_or_else(|| {
                    StorageError::StateCorrupted(
                        "attachment record disappeared during listing".to_owned(),
                    )
                })?;
            records.push(record);
        }
        Ok(records)
    }

    /// 按白名单迁移附件状态（同状态重复标记幂等）。
    pub fn mark_attachment_state(
        &mut self,
        attachment_id: &str,
        state: &str,
    ) -> Result<AttachmentRecord> {
        self.ensure_ready()?;
        validate_attachment_id(attachment_id)?;
        validate_state(state)?;
        let mut transaction = self.begin_transaction(WriteScope::attachments())?;
        let outcome = mark_attachment_state_inner(&mut transaction, attachment_id, state);
        match outcome {
            Ok(record) => {
                commit_transaction(transaction)?;
                Ok(record)
            }
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }

    /// 删除一条附件记录；返回是否真的删除了记录。FileKey 与缓存由 core 负责清理。
    pub fn delete_attachment(&mut self, attachment_id: &str) -> Result<bool> {
        self.ensure_ready()?;
        validate_attachment_id(attachment_id)?;
        let mut transaction = self.begin_transaction(WriteScope::attachments())?;
        let outcome = (|| -> Result<bool> {
            let deleted = transaction
                .transaction
                .execute("DELETE FROM attachments WHERE attachment_id = ?1", [attachment_id])?;
            if deleted > 0 {
                transaction.note(deleted);
            }
            Ok(deleted > 0)
        })();
        match outcome {
            Ok(deleted) => {
                commit_transaction(transaction)?;
                Ok(deleted)
            }
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }
}

fn mark_attachment_state_inner(
    transaction: &mut CoreTransaction<'_>,
    attachment_id: &str,
    state: &str,
) -> Result<AttachmentRecord> {
    let Some(existing) =
        load_attachment_row(transaction.connection(), transaction.key, attachment_id)?
    else {
        return Err(StorageError::AttachmentNotFound(attachment_id.to_owned()));
    };
    if existing.state == state {
        return Ok(existing);
    }
    if !transition_allowed(&existing.state, state) {
        return Err(StorageError::InvalidStateTransition(format!(
            "attachment state {} -> {state} is not allowed",
            existing.state
        )));
    }
    let now = now_ms();
    transaction.transaction.execute(
        "UPDATE attachments SET state = ?2, updated_at = ?3 WHERE attachment_id = ?1",
        params![attachment_id, state, now],
    )?;
    transaction.note(1);
    Ok(AttachmentRecord {
        state: state.to_owned(),
        updated_at_ms: now,
        ..existing
    })
}

// ==== 附件 FileKey 的持久化（迁移 0007_attachment_secrets.sql） ====

/// FileKey 长度（字节），与 crypto 的 FILE_KEY_BYTES 一致。
pub const ATTACHMENT_FILE_KEY_BYTES: usize = 32;
/// FileId 长度（字节），与 crypto 的 FILE_ID_BYTES 一致。
pub const ATTACHMENT_FILE_ID_BYTES: usize = 16;
/// 附件密钥记录格式版本（进入 AAD 与明文列）。
const ATTACHMENT_SECRET_RECORD_VERSION: u8 = 1;
/// 附件密钥密封记录的 AAD 版本。
const ATTACHMENT_SECRET_AAD_VERSION: &[u8] = b"attachment-secret-v1";
/// 附件密钥明文负载长度：version(1) + file_id(16) + file_key(32)。
const ATTACHMENT_SECRET_PLAINTEXT_BYTES: usize =
    1 + ATTACHMENT_FILE_ID_BYTES + ATTACHMENT_FILE_KEY_BYTES;

/// 附件密钥写入输入。file_key 是机密材料：Debug 只打印长度，
/// storage 内部立即转为 Zeroizing 缓冲并在落盘前清零。
pub struct AttachmentSecretInput {
    /// 本地附件 ID（必须已存在对应的 attachments 记录）。
    pub attachment_id: String,
    /// 附件文件标识（进入每块 AAD，决定能否解密已有缓存块）。
    pub file_id: [u8; ATTACHMENT_FILE_ID_BYTES],
    /// 文件内容密钥（恰好 32 字节；只在内存与密封缓冲中出现）。
    pub file_key: Vec<u8>,
    /// 创建时间（Unix 毫秒；只在首次写入时使用）。
    pub created_at_ms: i64,
}

impl std::fmt::Debug for AttachmentSecretInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttachmentSecretInput")
            .field("attachment_id", &self.attachment_id)
            .field("file_id", &hex(&self.file_id))
            .field("file_key_len", &self.file_key.len())
            .finish_non_exhaustive()
    }
}

impl AttachmentSecretInput {
    /// 有界结构校验（密钥长度由调用方传入的缓冲校验，见 [Store::save_attachment_secret]）。
    pub fn validate_with_key(&self, file_key: &[u8]) -> Result<()> {
        validate_attachment_id(&self.attachment_id)?;
        if file_key.len() != ATTACHMENT_FILE_KEY_BYTES {
            return Err(StorageError::InvalidInput(format!(
                "attachment file key must be exactly {ATTACHMENT_FILE_KEY_BYTES} bytes"
            )));
        }
        if self.created_at_ms < 0 {
            return Err(StorageError::InvalidInput(
                "attachment secret created_at_ms must be non-negative".to_owned(),
            ));
        }
        Ok(())
    }
}

/// 附件密钥投影：file_key 用 Zeroizing 缓冲，Drop 时清零；Debug 不输出密钥。
pub struct AttachmentSecretRecord {
    /// 本地附件 ID。
    pub attachment_id: String,
    /// 附件文件标识。
    pub file_id: [u8; ATTACHMENT_FILE_ID_BYTES],
    /// 文件内容密钥（32 字节；Drop 时清零）。
    pub file_key: Zeroizing<Vec<u8>>,
    /// 记录格式版本。
    pub version: u8,
    /// 创建时间。
    pub created_at_ms: i64,
    /// 最近更新时间。
    pub updated_at_ms: i64,
    /// 本次调用是否命中既有记录（读取路径恒为 true）。
    pub already_existed: bool,
}

impl std::fmt::Debug for AttachmentSecretRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttachmentSecretRecord")
            .field("attachment_id", &self.attachment_id)
            .field("file_id", &hex(&self.file_id))
            .field("file_key_len", &self.file_key.len())
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

fn attachment_secret_aad(attachment_id: &str) -> [&[u8]; 2] {
    [attachment_id.as_bytes(), ATTACHMENT_SECRET_AAD_VERSION]
}

fn seal_attachment_secret(
    key: &ProtectedStorageKey,
    attachment_id: &str,
    file_id: &[u8; ATTACHMENT_FILE_ID_BYTES],
    file_key: &[u8],
) -> Result<Vec<u8>> {
    let mut plaintext = Zeroizing::new(Vec::with_capacity(ATTACHMENT_SECRET_PLAINTEXT_BYTES));
    plaintext.push(ATTACHMENT_SECRET_RECORD_VERSION);
    plaintext.extend_from_slice(file_id);
    plaintext.extend_from_slice(file_key);
    let sealer = RecordSealer::new(key, SealPurpose::AttachmentSecret);
    sealer.seal(&attachment_secret_aad(attachment_id), plaintext.as_slice())
}

fn open_attachment_secret(
    key: &ProtectedStorageKey,
    attachment_id: &str,
    sealed: &[u8],
) -> Result<([u8; ATTACHMENT_FILE_ID_BYTES], Zeroizing<Vec<u8>>, u8)> {
    let sealer = RecordSealer::new(key, SealPurpose::AttachmentSecret);
    let plaintext = sealer.open(&attachment_secret_aad(attachment_id), sealed)?;
    if plaintext.len() != ATTACHMENT_SECRET_PLAINTEXT_BYTES {
        return Err(StorageError::StateCorrupted(
            "stored attachment secret has an invalid length".to_owned(),
        ));
    }
    let version = plaintext[0];
    if version != ATTACHMENT_SECRET_RECORD_VERSION {
        return Err(StorageError::StateCorrupted(
            "stored attachment secret has an unsupported record version".to_owned(),
        ));
    }
    let mut file_id = [0u8; ATTACHMENT_FILE_ID_BYTES];
    file_id.copy_from_slice(&plaintext[1..1 + ATTACHMENT_FILE_ID_BYTES]);
    let file_key = Zeroizing::new(
        plaintext[1 + ATTACHMENT_FILE_ID_BYTES..ATTACHMENT_SECRET_PLAINTEXT_BYTES].to_vec(),
    );
    Ok((file_id, file_key, version))
}

fn load_attachment_secret_row(
    connection: &Connection,
    key: &ProtectedStorageKey,
    attachment_id: &str,
) -> Result<Option<AttachmentSecretRecord>> {
    let row: Option<(i64, Vec<u8>, i64, i64)> = connection
        .query_row(
            "SELECT version, protected_secret, created_at, updated_at
               FROM attachment_secrets WHERE attachment_id = ?1",
            [attachment_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((version, sealed, created_at, updated_at)) = row else {
        return Ok(None);
    };
    if version <= 0 || version > i64::from(u8::MAX) || created_at < 0 || updated_at < 0 {
        return Err(StorageError::StateCorrupted(
            "stored attachment secret has invalid numeric columns".to_owned(),
        ));
    }
    let (file_id, file_key, record_version) =
        open_attachment_secret(key, attachment_id, &sealed)?;
    Ok(Some(AttachmentSecretRecord {
        attachment_id: attachment_id.to_owned(),
        file_id,
        file_key,
        version: record_version,
        created_at_ms: created_at,
        updated_at_ms: updated_at,
        already_existed: true,
    }))
}

fn save_attachment_secret_inner(
    transaction: &mut CoreTransaction<'_>,
    input: &AttachmentSecretInput,
    file_key: &[u8],
) -> Result<AttachmentSecretRecord> {
    // 附件记录必须存在（外键也会兜底，这里给出确定的错误码）。
    let attachment_exists: Option<i64> = transaction
        .connection()
        .query_row(
            "SELECT 1 FROM attachments WHERE attachment_id = ?1",
            [&input.attachment_id],
            |row| row.get(0),
        )
        .optional()?;
    if attachment_exists.is_none() {
        return Err(StorageError::AttachmentNotFound(input.attachment_id.clone()));
    }
    if let Some(existing) = load_attachment_secret_row(
        transaction.connection(),
        transaction.key,
        &input.attachment_id,
    )? {
        // 幂等：同 file_id + 同密钥返回既有记录；否则显式冲突（绝不静默换钥）。
        let same_key = existing.file_key.len() == file_key.len()
            && existing
                .file_key
                .iter()
                .zip(file_key.iter())
                .all(|(left, right)| left == right);
        if existing.file_id == input.file_id && same_key {
            return Ok(existing);
        }
        return Err(StorageError::OperationIdConflict(
            "attachment secret is already bound to a different file key".to_owned(),
        ));
    }
    let count: i64 =
        transaction
            .connection()
            .query_row("SELECT COUNT(*) FROM attachment_secrets", [], |row| row.get(0))?;
    if u64::try_from(count).unwrap_or(u64::MAX) >= u64::from(MAX_ATTACHMENTS) {
        return Err(StorageError::InvalidInput(format!(
            "attachment secret limit of {MAX_ATTACHMENTS} reached"
        )));
    }
    let sealed = seal_attachment_secret(
        transaction.key,
        &input.attachment_id,
        &input.file_id,
        file_key,
    )?;
    let now = now_ms();
    transaction.transaction.execute(
        "INSERT INTO attachment_secrets
             (attachment_id, version, protected_secret, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            input.attachment_id,
            i64::from(ATTACHMENT_SECRET_RECORD_VERSION),
            sealed,
            input.created_at_ms,
            now
        ],
    )?;
    transaction.note(1);
    Ok(AttachmentSecretRecord {
        attachment_id: input.attachment_id.clone(),
        file_id: input.file_id,
        file_key: Zeroizing::new(file_key.to_vec()),
        version: ATTACHMENT_SECRET_RECORD_VERSION,
        created_at_ms: input.created_at_ms,
        updated_at_ms: now,
        already_existed: false,
    })
}

impl Store {
    /// 保存（或幂等重放）附件 FileKey。
    ///
    /// 同 attachment_id 同 file_id + 同密钥是幂等重放；换密钥返回 OPERATION_ID_CONFLICT，
    /// 绝不静默覆盖（否则已有缓存块将永久无法解密）。附件记录不存在返回 ATTACHMENT_NOT_FOUND。
    pub fn save_attachment_secret(
        &mut self,
        mut input: AttachmentSecretInput,
    ) -> Result<AttachmentSecretRecord> {
        self.ensure_ready()?;
        // 立刻把调用方交来的密钥接管到 Zeroizing 缓冲，避免中途失败时明文残留。
        let file_key = Zeroizing::new(std::mem::take(&mut input.file_key));
        input.validate_with_key(file_key.as_slice())?;
        let mut transaction = self.begin_transaction(WriteScope::attachments())?;
        match save_attachment_secret_inner(&mut transaction, &input, file_key.as_slice()) {
            Ok(record) => {
                commit_transaction(transaction)?;
                Ok(record)
            }
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }

    /// 读取附件 FileKey；不存在返回 None，损坏返回 STATE_CORRUPTED。
    pub fn load_attachment_secret(
        &self,
        attachment_id: &str,
    ) -> Result<Option<AttachmentSecretRecord>> {
        self.ensure_ready()?;
        validate_attachment_id(attachment_id)?;
        load_attachment_secret_row(self.connection(), &self.key, attachment_id)
    }

    /// 删除附件 FileKey；返回是否真的删除了记录。
    pub fn delete_attachment_secret(&mut self, attachment_id: &str) -> Result<bool> {
        self.ensure_ready()?;
        validate_attachment_id(attachment_id)?;
        let mut transaction = self.begin_transaction(WriteScope::attachments())?;
        let outcome = (|| -> Result<bool> {
            let deleted = transaction.transaction.execute(
                "DELETE FROM attachment_secrets WHERE attachment_id = ?1",
                [attachment_id],
            )?;
            if deleted > 0 {
                transaction.note(deleted);
            }
            Ok(deleted > 0)
        })();
        match outcome {
            Ok(deleted) => {
                commit_transaction(transaction)?;
                Ok(deleted)
            }
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }
}
