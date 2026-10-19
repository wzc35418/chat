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
    CoreTransaction, MAX_ID_BYTES, Result, Store, WriteScope, commit_transaction, ensure_id, hex,
    now_ms, rollback_transaction,
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

    /// 列出全部附件 ID（按 ID 升序，上限 MAX_ATTACHMENTS）：供缓存回收判断“有无记录引用”。
    ///
    /// 只返回 ID，不解密任何记录；坏记录不会阻塞回收扫描（回收只看 ID 引用）。
    pub fn attachment_ids(&self) -> Result<Vec<String>> {
        self.ensure_ready()?;
        let mut statement = self.connection().prepare(
            "SELECT attachment_id FROM attachments ORDER BY attachment_id ASC LIMIT ?1",
        )?;
        let rows =
            statement.query_map([i64::from(MAX_ATTACHMENTS)], |row| row.get::<_, String>(0))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
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

// ==== 消息对附件的引用（迁移 0009_message_attachments.sql） ====

/// 单个附件的消息引用列表上限。
pub const MAX_MESSAGE_ATTACHMENT_LIST: u32 = 512;

fn validate_message_id(message_id: &str) -> Result<()> {
    ensure_id("message", message_id)?;
    if message_id.len() > MAX_ID_BYTES {
        return Err(StorageError::InvalidInput(format!(
            "message ID exceeds {MAX_ID_BYTES} bytes"
        )));
    }
    Ok(())
}

fn message_exists(connection: &Connection, message_id: &str) -> Result<bool> {
    let row: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM messages WHERE message_id = ?1",
            [message_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(row.is_some())
}

impl Store {
    /// 记录一条消息对附件的引用；同 (message, attachment) 重复调用幂等。
    ///
    /// 附件记录必须存在（ATTACHMENT_NOT_FOUND）；消息必须存在（INVALID_INPUT），
    /// 因此不会留下悬空引用。外键同时保证删除消息/附件记录时关联级联消失。
    pub fn attach_message_attachment(&mut self, message_id: &str, attachment_id: &str) -> Result<()> {
        self.ensure_ready()?;
        validate_message_id(message_id)?;
        validate_attachment_id(attachment_id)?;
        let mut transaction = self.begin_transaction(WriteScope::attachments())?;
        let outcome = (|| -> Result<()> {
            let attachment_exists =
                attachment_chunk_count(transaction.connection(), attachment_id)?.is_some();
            if !attachment_exists {
                return Err(StorageError::AttachmentNotFound(attachment_id.to_owned()));
            }
            if !message_exists(transaction.connection(), message_id)? {
                return Err(StorageError::InvalidInput(
                    "message does not exist".to_owned(),
                ));
            }
            let inserted = transaction.transaction.execute(
                "INSERT OR IGNORE INTO message_attachments
                     (message_id, attachment_id, created_at)
                 VALUES (?1, ?2, ?3)",
                params![message_id, attachment_id, now_ms()],
            )?;
            if inserted > 0 {
                transaction.note(inserted);
            }
            Ok(())
        })();
        match outcome {
            Ok(()) => {
                commit_transaction(transaction)?;
                Ok(())
            }
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }

    /// 解除一条消息对某个附件的引用；返回是否真的删除（幂等）。
    pub fn detach_message_attachment(
        &mut self,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<bool> {
        self.ensure_ready()?;
        validate_message_id(message_id)?;
        validate_attachment_id(attachment_id)?;
        let mut transaction = self.begin_transaction(WriteScope::attachments())?;
        let outcome = (|| -> Result<bool> {
            let deleted = transaction.transaction.execute(
                "DELETE FROM message_attachments
                  WHERE message_id = ?1 AND attachment_id = ?2",
                params![message_id, attachment_id],
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

    /// 解除一条消息的全部附件引用；返回删除条数。
    pub fn detach_message_attachments(&mut self, message_id: &str) -> Result<u64> {
        self.ensure_ready()?;
        validate_message_id(message_id)?;
        let mut transaction = self.begin_transaction(WriteScope::attachments())?;
        let outcome = (|| -> Result<u64> {
            let deleted = transaction.transaction.execute(
                "DELETE FROM message_attachments WHERE message_id = ?1",
                [message_id],
            )?;
            if deleted > 0 {
                transaction.note(deleted);
            }
            Ok(u64::try_from(deleted).unwrap_or(u64::MAX))
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

    /// 引用某附件的消息数（ATTACHMENTS-008 引用计数）。
    pub fn message_attachment_count(&self, attachment_id: &str) -> Result<u64> {
        self.ensure_ready()?;
        validate_attachment_id(attachment_id)?;
        let count: i64 = self.connection().query_row(
            "SELECT COUNT(*) FROM message_attachments WHERE attachment_id = ?1",
            [attachment_id],
            |row| row.get(0),
        )?;
        Ok(u64::try_from(count).unwrap_or(u64::MAX))
    }

    /// 引用某附件的消息 ID 列表（按消息 ID 升序，上限 MAX_MESSAGE_ATTACHMENT_LIST）。
    pub fn message_attachment_ids(&self, attachment_id: &str) -> Result<Vec<String>> {
        self.ensure_ready()?;
        validate_attachment_id(attachment_id)?;
        let mut statement = self.connection().prepare(
            "SELECT message_id FROM message_attachments
              WHERE attachment_id = ?1 ORDER BY message_id ASC LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![attachment_id, i64::from(MAX_MESSAGE_ATTACHMENT_LIST)],
            |row| row.get::<_, String>(0),
        )?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }

    /// 全部被消息引用的附件 ID（去重、升序、有界）；缓存回收一次性取用。
    pub fn message_referenced_attachment_ids(&self) -> Result<Vec<String>> {
        self.ensure_ready()?;
        let mut statement = self.connection().prepare(
            "SELECT DISTINCT attachment_id FROM message_attachments
              ORDER BY attachment_id ASC LIMIT ?1",
        )?;
        let rows = statement.query_map([i64::from(MAX_ATTACHMENTS)], |row| {
            row.get::<_, String>(0)
        })?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }

    /// 某条消息引用的附件 ID 列表（按 ID 升序，上限 MAX_MESSAGE_ATTACHMENT_LIST）。
    ///
    /// 供界面展示消息附件；只返回 ID，不解密任何记录。
    pub fn message_attachment_refs_for_message(&self, message_id: &str) -> Result<Vec<String>> {
        self.ensure_ready()?;
        validate_message_id(message_id)?;
        let mut statement = self.connection().prepare(
            "SELECT attachment_id FROM message_attachments
              WHERE message_id = ?1 ORDER BY attachment_id ASC LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![message_id, i64::from(MAX_MESSAGE_ATTACHMENT_LIST)],
            |row| row.get::<_, String>(0),
        )?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }

    /// 显式删除一条消息（引用随外键级联清除）；返回是否真的删除。
    ///
    /// 只删 messages 行本身：message_attempts/草稿等由各自的生命周期管理；
    /// 附件引用（message_attachments）与回执（receipts）由外键级联删除。
    pub fn delete_message(&mut self, message_id: &str) -> Result<bool> {
        self.ensure_ready()?;
        validate_message_id(message_id)?;
        let mut transaction = self.begin_transaction(WriteScope::attachments())?;
        let outcome = (|| -> Result<bool> {
            let deleted = transaction
                .transaction
                .execute("DELETE FROM messages WHERE message_id = ?1", [message_id])?;
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

    /// GC 用：状态为 state 且 created_at 早于 cutoff 的附件 ID（升序、有界）。
    pub fn attachment_ids_with_state_before(
        &self,
        state: &str,
        cutoff_ms: i64,
        limit: u32,
    ) -> Result<Vec<String>> {
        self.ensure_ready()?;
        validate_state(state)?;
        if cutoff_ms < 0 {
            return Err(StorageError::InvalidInput(
                "attachment cutoff must be a non-negative timestamp".to_owned(),
            ));
        }
        if limit == 0 || limit > MAX_ATTACHMENT_LIST {
            return Err(StorageError::InvalidInput(format!(
                "attachment list limit must be 1..={MAX_ATTACHMENT_LIST}"
            )));
        }
        let mut statement = self.connection().prepare(
            "SELECT attachment_id FROM attachments
              WHERE state = ?1 AND created_at < ?2
              ORDER BY created_at ASC, attachment_id ASC LIMIT ?3",
        )?;
        let rows = statement.query_map(params![state, cutoff_ms, i64::from(limit)], |row| {
            row.get::<_, String>(0)
        })?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }
}

// ==== 附件传输进度（断点续传，迁移 0008_attachment_transfer_progress.sql） ====

/// 允许的传输方向（白名单，与列 CHECK 一致）。
pub const ATTACHMENT_DIRECTIONS: [&str; 2] = ["upload", "download"];
/// 上传会话 / blob 标识的文本上限（字节）。
pub const MAX_ATTACHMENT_PROGRESS_ID_BYTES: usize = 256;
/// 进度记录格式版本（进入密封明文）。
const ATTACHMENT_PROGRESS_RECORD_VERSION: u8 = 1;
/// 进度密封记录的 AAD 版本。
const ATTACHMENT_PROGRESS_AAD_VERSION: &[u8] = b"attachment-transfer-progress-v1";

/// 附件传输进度写入输入。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentProgressInput {
    /// 本地附件 ID（必须已存在对应 attachments 记录）。
    pub attachment_id: String,
    /// 方向（upload/download）。
    pub direction: String,
    /// 声明的分块总数（必须与 attachments.chunk_count 一致）。
    pub chunk_count: u32,
    /// 已确认块位图（长度必须等于 chunk_count）。
    pub confirmed: Vec<bool>,
    /// 节点上传会话 ID（下载方向为 None）。
    pub session_id: Option<String>,
    /// 节点 blob 标识（可为 None）。
    pub blob_id: Option<String>,
    /// 下载方向：已写前缀的明文 SHA-256（小写 hex），用于续传时校验本地临时文件。
    pub prefix_digest: Option<String>,
}

impl AttachmentProgressInput {
    /// 有界结构校验（chunk_count 与附件记录的一致性在 Store::save_attachment_progress 校验）。
    pub fn validate(&self) -> Result<()> {
        validate_attachment_id(&self.attachment_id)?;
        validate_direction(&self.direction)?;
        if self.chunk_count == 0 || self.chunk_count > MAX_ATTACHMENT_CHUNKS {
            return Err(StorageError::InvalidInput(format!(
                "attachment progress chunk count must be 1..={MAX_ATTACHMENT_CHUNKS}"
            )));
        }
        if self.confirmed.len() != self.chunk_count as usize {
            return Err(StorageError::InvalidInput(
                "attachment progress bitmap length must equal chunk_count".to_owned(),
            ));
        }
        for (label, value) in [
            ("session_id", self.session_id.as_deref()),
            ("blob_id", self.blob_id.as_deref()),
        ] {
            if let Some(value) = value
                && (value.is_empty() || value.len() > MAX_ATTACHMENT_PROGRESS_ID_BYTES)
            {
                return Err(StorageError::InvalidInput(format!(
                    "attachment progress {label} must be 1..={MAX_ATTACHMENT_PROGRESS_ID_BYTES} bytes"
                )));
            }
        }
        if let Some(digest) = &self.prefix_digest {
            validate_digest("attachment progress prefix digest", digest)?;
        }
        Ok(())
    }
}

/// 附件传输进度投影（密封字段已解密）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentProgress {
    /// 本地附件 ID。
    pub attachment_id: String,
    /// 方向。
    pub direction: String,
    /// 分块总数。
    pub chunk_count: u32,
    /// 已确认块位图。
    pub confirmed: Vec<bool>,
    /// 节点上传会话 ID。
    pub session_id: Option<String>,
    /// 节点 blob 标识。
    pub blob_id: Option<String>,
    /// 下载方向：已写前缀的明文摘要。
    pub prefix_digest: Option<String>,
    /// 最近更新时间。
    pub updated_at_ms: i64,
    /// 本次调用是否命中既有记录（读取路径恒为 true）。
    pub already_existed: bool,
}

impl AttachmentProgress {
    /// 已确认块数。
    pub fn confirmed_count(&self) -> u32 {
        self.confirmed.iter().filter(|confirmed| **confirmed).count() as u32
    }

    /// 连续的已确认前缀长度（顺序传输的续传起点）。
    pub fn contiguous_prefix(&self) -> u32 {
        self.confirmed
            .iter()
            .take_while(|confirmed| **confirmed)
            .count() as u32
    }

    /// 是否全部块都已确认。
    pub fn is_complete(&self) -> bool {
        self.confirmed.iter().all(|confirmed| *confirmed)
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredProgress {
    version: u8,
    chunk_count: u32,
    confirmed: Vec<u8>,
    session_id: Option<String>,
    blob_id: Option<String>,
    prefix_digest: Option<String>,
}

fn validate_direction(direction: &str) -> Result<()> {
    if !ATTACHMENT_DIRECTIONS.contains(&direction) {
        return Err(StorageError::InvalidInput(
            "attachment progress direction must be upload or download".to_owned(),
        ));
    }
    Ok(())
}

fn pack_bits(bits: &[bool]) -> Vec<u8> {
    let mut packed = vec![0u8; bits.len().div_ceil(8)];
    for (index, bit) in bits.iter().enumerate() {
        if *bit {
            packed[index / 8] |= 1 << (index % 8);
        }
    }
    packed
}

fn unpack_bits(packed: &[u8], chunk_count: u32) -> Result<Vec<bool>> {
    let expected = (chunk_count as usize).div_ceil(8);
    if packed.len() != expected {
        return Err(StorageError::StateCorrupted(
            "stored attachment progress bitmap has an invalid length".to_owned(),
        ));
    }
    let mut bits = Vec::with_capacity(chunk_count as usize);
    for index in 0..chunk_count as usize {
        bits.push(packed[index / 8] & (1 << (index % 8)) != 0);
    }
    // 尾部填充位必须为 0，否则位图不是本实现写出的。
    let used = chunk_count as usize % 8;
    if used != 0
        && let Some(last) = packed.last()
        && (last >> used) != 0
    {
        return Err(StorageError::StateCorrupted(
            "stored attachment progress bitmap has non-zero padding bits".to_owned(),
        ));
    }
    Ok(bits)
}

fn attachment_progress_aad<'a>(attachment_id: &'a str, direction: &'a str) -> [&'a [u8]; 3] {
    [
        attachment_id.as_bytes(),
        direction.as_bytes(),
        ATTACHMENT_PROGRESS_AAD_VERSION,
    ]
}

fn seal_progress(
    key: &ProtectedStorageKey,
    attachment_id: &str,
    direction: &str,
    stored: &StoredProgress,
) -> Result<Vec<u8>> {
    let plaintext = serde_json::to_vec(stored).map_err(|error| {
        StorageError::Failure(format!("cannot encode attachment progress: {error}"))
    })?;
    let plaintext = Zeroizing::new(plaintext);
    let sealer = RecordSealer::new(key, SealPurpose::AttachmentTransferProgress);
    sealer.seal(
        &attachment_progress_aad(attachment_id, direction),
        plaintext.as_slice(),
    )
}

fn open_progress(
    key: &ProtectedStorageKey,
    attachment_id: &str,
    direction: &str,
    chunk_count: u32,
    sealed: &[u8],
) -> Result<AttachmentProgress> {
    let sealer = RecordSealer::new(key, SealPurpose::AttachmentTransferProgress);
    let plaintext = sealer.open(&attachment_progress_aad(attachment_id, direction), sealed)?;
    let stored: StoredProgress = serde_json::from_slice(plaintext.as_slice()).map_err(|_| {
        StorageError::StateCorrupted("stored attachment progress is not readable".to_owned())
    })?;
    if stored.version != ATTACHMENT_PROGRESS_RECORD_VERSION {
        return Err(StorageError::StateCorrupted(
            "stored attachment progress has an unsupported record version".to_owned(),
        ));
    }
    if stored.chunk_count != chunk_count {
        return Err(StorageError::StateCorrupted(
            "stored attachment progress chunk count does not match its column".to_owned(),
        ));
    }
    let confirmed = unpack_bits(&stored.confirmed, chunk_count)?;
    if let Some(digest) = &stored.prefix_digest {
        validate_digest("stored attachment progress prefix digest", digest).map_err(|_| {
            StorageError::StateCorrupted(
                "stored attachment progress prefix digest is invalid".to_owned(),
            )
        })?;
    }
    Ok(AttachmentProgress {
        attachment_id: attachment_id.to_owned(),
        direction: direction.to_owned(),
        chunk_count,
        confirmed,
        session_id: stored.session_id,
        blob_id: stored.blob_id,
        prefix_digest: stored.prefix_digest,
        updated_at_ms: 0,
        already_existed: true,
    })
}

fn attachment_chunk_count(connection: &Connection, attachment_id: &str) -> Result<Option<u32>> {
    let row: Option<i64> = connection
        .query_row(
            "SELECT chunk_count FROM attachments WHERE attachment_id = ?1",
            [attachment_id],
            |row| row.get(0),
        )
        .optional()?;
    match row {
        None => Ok(None),
        Some(chunk_count) if chunk_count > 0 && chunk_count <= i64::from(MAX_ATTACHMENT_CHUNKS) => {
            Ok(Some(chunk_count as u32))
        }
        Some(_) => Err(StorageError::StateCorrupted(
            "attachment record has an invalid chunk count".to_owned(),
        )),
    }
}

fn load_progress_row(
    connection: &Connection,
    key: &ProtectedStorageKey,
    attachment_id: &str,
    direction: &str,
) -> Result<Option<AttachmentProgress>> {
    let row: Option<(i64, Vec<u8>, i64)> = connection
        .query_row(
            "SELECT chunk_count, protected_progress, updated_at
               FROM attachment_transfer_progress
              WHERE attachment_id = ?1 AND direction = ?2",
            params![attachment_id, direction],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((chunk_count, sealed, updated_at)) = row else {
        return Ok(None);
    };
    if chunk_count <= 0 || chunk_count > i64::from(MAX_ATTACHMENT_CHUNKS) || updated_at < 0 {
        return Err(StorageError::StateCorrupted(
            "stored attachment progress has invalid numeric columns".to_owned(),
        ));
    }
    let chunk_count = chunk_count as u32;
    // 位图长度必须与当前附件记录一致，否则不接受该进度。
    let current = attachment_chunk_count(connection, attachment_id)?
        .ok_or_else(|| StorageError::AttachmentNotFound(attachment_id.to_owned()))?;
    if current != chunk_count {
        return Err(StorageError::StateCorrupted(
            "stored attachment progress does not match the attachment chunk count".to_owned(),
        ));
    }
    let mut progress = open_progress(key, attachment_id, direction, chunk_count, &sealed)?;
    progress.updated_at_ms = updated_at;
    Ok(Some(progress))
}

fn save_progress_inner(
    transaction: &mut CoreTransaction<'_>,
    input: &AttachmentProgressInput,
) -> Result<AttachmentProgress> {
    let current = attachment_chunk_count(transaction.connection(), &input.attachment_id)?
        .ok_or_else(|| StorageError::AttachmentNotFound(input.attachment_id.clone()))?;
    if current != input.chunk_count {
        return Err(StorageError::InvalidInput(
            "attachment progress chunk count must match the attachment record".to_owned(),
        ));
    }
    let stored = StoredProgress {
        version: ATTACHMENT_PROGRESS_RECORD_VERSION,
        chunk_count: input.chunk_count,
        confirmed: pack_bits(&input.confirmed),
        session_id: input.session_id.clone(),
        blob_id: input.blob_id.clone(),
        prefix_digest: input.prefix_digest.clone(),
    };
    let sealed = seal_progress(
        transaction.key,
        &input.attachment_id,
        &input.direction,
        &stored,
    )?;
    let now = now_ms();
    let existed: Option<i64> = transaction
        .connection()
        .query_row(
            "SELECT 1 FROM attachment_transfer_progress
              WHERE attachment_id = ?1 AND direction = ?2",
            params![input.attachment_id, input.direction],
            |row| row.get(0),
        )
        .optional()?;
    transaction.transaction.execute(
        "INSERT INTO attachment_transfer_progress
             (attachment_id, direction, chunk_count, protected_progress, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(attachment_id, direction) DO UPDATE SET
             chunk_count = excluded.chunk_count,
             protected_progress = excluded.protected_progress,
             updated_at = excluded.updated_at",
        params![
            input.attachment_id,
            input.direction,
            i64::from(input.chunk_count),
            sealed,
            now
        ],
    )?;
    transaction.note(1);
    Ok(AttachmentProgress {
        attachment_id: input.attachment_id.clone(),
        direction: input.direction.clone(),
        chunk_count: input.chunk_count,
        confirmed: input.confirmed.clone(),
        session_id: input.session_id.clone(),
        blob_id: input.blob_id.clone(),
        prefix_digest: input.prefix_digest.clone(),
        updated_at_ms: now,
        already_existed: existed.is_some(),
    })
}

impl Store {
    /// 保存（或覆盖）附件传输进度快照。
    ///
    /// 幂等：进度是“可推进的快照”，重复保存同一份内容不报错；chunk_count 必须与
    /// attachments.chunk_count 一致，位图长度必须等于 chunk_count，方向必须在白名单内。
    pub fn save_attachment_progress(
        &mut self,
        input: AttachmentProgressInput,
    ) -> Result<AttachmentProgress> {
        self.ensure_ready()?;
        input.validate()?;
        let mut transaction = self.begin_transaction(WriteScope::attachments())?;
        match save_progress_inner(&mut transaction, &input) {
            Ok(progress) => {
                commit_transaction(transaction)?;
                Ok(progress)
            }
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }

    /// 读取附件传输进度；不存在返回 None，损坏返回 STATE_CORRUPTED。
    pub fn load_attachment_progress(
        &self,
        attachment_id: &str,
        direction: &str,
    ) -> Result<Option<AttachmentProgress>> {
        self.ensure_ready()?;
        validate_attachment_id(attachment_id)?;
        validate_direction(direction)?;
        load_progress_row(self.connection(), &self.key, attachment_id, direction)
    }

    /// 清理“超过 max_age_ms 未更新”的传输进度；返回删除条数。
    ///
    /// 用于异常中断后残留进度的终端清理（完成时由 core 显式清除，删除附件时由外键级联清除）。
    /// max_age_ms 必须为正；同一条附件按 (attachment_id, direction) 各自计数。
    pub fn purge_stale_transfer_progress(&mut self, max_age_ms: i64) -> Result<u64> {
        self.ensure_ready()?;
        if max_age_ms <= 0 {
            return Err(StorageError::InvalidInput(
                "transfer progress max age must be positive".to_owned(),
            ));
        }
        let cutoff = now_ms().saturating_sub(max_age_ms);
        let mut transaction = self.begin_transaction(WriteScope::attachments())?;
        let outcome = (|| -> Result<u64> {
            let deleted = transaction.transaction.execute(
                "DELETE FROM attachment_transfer_progress WHERE updated_at < ?1",
                [cutoff],
            )?;
            if deleted > 0 {
                transaction.note(deleted);
            }
            Ok(u64::try_from(deleted).unwrap_or(u64::MAX))
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

    /// 清除某个方向的传输进度；返回是否真的删除了记录。
    pub fn clear_attachment_progress(
        &mut self,
        attachment_id: &str,
        direction: &str,
    ) -> Result<bool> {
        self.ensure_ready()?;
        validate_attachment_id(attachment_id)?;
        validate_direction(direction)?;
        let mut transaction = self.begin_transaction(WriteScope::attachments())?;
        let outcome = (|| -> Result<bool> {
            let deleted = transaction.transaction.execute(
                "DELETE FROM attachment_transfer_progress
                  WHERE attachment_id = ?1 AND direction = ?2",
                params![attachment_id, direction],
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
// ==== 两阶段回收（迁移 0012_attachment_gc.sql，ATTACHMENTS-008） ====

/// 单次回收计划的候选上限（有界：计划不会无界膨胀）。
pub const MAX_ATTACHMENT_GC_BATCH: usize = 256;

/// 回收计划的候选（只含不透明 ID 与创建时间，不含任何内容/密钥）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentGcCandidate {
    /// 附件 ID。
    pub attachment_id: String,
    /// 附件创建时间（Unix 毫秒；宽限期按它计算）。
    pub created_at_ms: i64,
}

/// 回收计划：**只选不做**（可安全重复执行）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AttachmentGcPlan {
    /// 候选附件。
    pub candidates: Vec<AttachmentGcCandidate>,
    /// 候选附件记录数（= candidates.len()）。
    pub blobs: u32,
    /// 候选里带密钥记录的条数。
    pub secrets: u32,
    /// 候选里的传输进度行数。
    pub progress_rows: u32,
    /// 候选附件明文总字节数（对外报告用，不读内容）。
    pub bytes: u64,
    /// 因仍有消息引用而保留的条数。
    pub skipped_referenced: u32,
    /// 因仍在宽限期内而保留的条数。
    pub skipped_within_grace: u32,
    /// 因有 outbox/job 引用而保留的条数。
    pub skipped_outbox: u32,
    /// 是否因为上限而截断（true 表示还有候选未进计划）。
    pub truncated: bool,
}

/// 回收执行报告。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AttachmentGcReport {
    /// 本次标记（gc_pending = 1）的行数。
    pub marked: u32,
    /// 本次真正删除的附件记录数。
    pub removed: u32,
    /// 计划里的候选已不存在（上一轮已删）或未标记成功的条数。
    pub skipped: u32,
}

impl Store {
    /// 计算回收计划：**只看不写**。
    ///
    /// 候选条件（全部满足）：
    /// - 没有 `message_attachments` 引用；
    /// - 没有 `outbox_jobs.payload_ref` 引用（进行中的上传/投递任务）；
    /// - `created_at` 早于 `now_ms - grace_ms`（宽限期内一律保留）；
    /// - 已标记未删除的行（`gc_pending = 1`，上一次执行中断）**仍然进入候选**，因此可重入。
    ///
    /// 只读：不修改任何行，因此崩溃后重新计划会得到同样的候选（直到 execute 成功）。
    pub fn plan_attachment_gc(
        &self,
        now_ms: i64,
        grace_ms: i64,
        limit: usize,
    ) -> Result<AttachmentGcPlan> {
        self.ensure_ready()?;
        if now_ms < 0 || grace_ms < 0 {
            return Err(StorageError::InvalidInput(
                "attachment gc window must be non-negative".to_owned(),
            ));
        }
        let limit = limit.min(MAX_ATTACHMENT_GC_BATCH).max(1);
        let cutoff = now_ms.saturating_sub(grace_ms);
        let connection = self.connection();
        let mut plan = AttachmentGcPlan::default();
        // 保留原因计数（候选之外的行）。
        let (within_grace, outbox_only): (i64, i64) = connection
            .query_row(
                "SELECT
                    COALESCE(SUM(CASE WHEN a.created_at > ?1 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN a.created_at <= ?1
                                      AND EXISTS (SELECT 1 FROM outbox_jobs o WHERE o.payload_ref = a.attachment_id)
                                 THEN 1 ELSE 0 END), 0)
                   FROM attachments a",
                [cutoff],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(StorageError::from)?;
        plan.skipped_within_grace = u32::try_from(within_grace).unwrap_or(u32::MAX);
        plan.skipped_outbox = u32::try_from(outbox_only).unwrap_or(u32::MAX);
        // 候选：宽限期外 + 无消息引用 + 无 outbox 引用（已标记未删的行同样入选）。
        let mut statement = connection
            .prepare(
                "SELECT a.attachment_id, a.created_at, a.plain_size,
                        (SELECT COUNT(*) FROM attachment_secrets s WHERE s.attachment_id = a.attachment_id),
                        (SELECT COUNT(*) FROM attachment_transfer_progress p WHERE p.attachment_id = a.attachment_id)
                   FROM attachments a
                  WHERE a.created_at <= ?1
                    AND NOT EXISTS (SELECT 1 FROM message_attachments m WHERE m.attachment_id = a.attachment_id)
                    AND NOT EXISTS (SELECT 1 FROM outbox_jobs o WHERE o.payload_ref = a.attachment_id)
                  ORDER BY a.created_at ASC, a.attachment_id ASC
                  LIMIT ?2",
            )
            .map_err(StorageError::from)?;
        let row_limit = i64::try_from(limit.saturating_add(1))
            .map_err(|_| StorageError::InvalidInput("attachment gc limit".to_owned()))?;
        let rows = statement
            .query_map(params![cutoff, row_limit], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(StorageError::from)?;
        let mut seen = 0usize;
        for row in rows {
            let (attachment_id, created_at, plain_size, secrets, progress) =
                row.map_err(StorageError::from)?;
            seen += 1;
            if seen > limit {
                plan.truncated = true;
                break;
            }
            plan.candidates.push(AttachmentGcCandidate {
                attachment_id,
                created_at_ms: created_at,
            });
            plan.blobs = plan.blobs.saturating_add(1);
            plan.secrets = plan.secrets.saturating_add(u32::try_from(secrets).unwrap_or(0));
            plan.progress_rows = plan
                .progress_rows
                .saturating_add(u32::try_from(progress).unwrap_or(0));
            plan.bytes = plan
                .bytes
                .saturating_add(u64::try_from(plain_size).unwrap_or(0));
        }
        // 仍被消息引用、但宽度期已过的条数（诊断用）。
        let referenced: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM attachments a
                  WHERE a.created_at <= ?1
                    AND EXISTS (SELECT 1 FROM message_attachments m WHERE m.attachment_id = a.attachment_id)",
                [cutoff],
                |row| row.get(0),
            )
            .map_err(StorageError::from)?;
        plan.skipped_referenced = u32::try_from(referenced).unwrap_or(u32::MAX);
        Ok(plan)
    }

    /// 执行回收计划：**先标记（gc_pending = 1）再删除**，可重入。
    ///
    /// - 已经不存在（上一轮删除成功/重复执行）的候选计入 `skipped`，不会重复删除；
    /// - 标记后删除失败（进程崩溃）时，下一次 `plan_attachment_gc` 仍会选中该行；
    /// - 删除附件行时密钥/块/进度/消息引用由外键级联清除。
    pub fn execute_attachment_gc(&mut self, plan: &AttachmentGcPlan) -> Result<AttachmentGcReport> {
        self.ensure_ready()?;
        let mut report = AttachmentGcReport::default();
        if plan.candidates.is_empty() {
            return Ok(report);
        }
        let marked_at = now_ms();
        for candidate in &plan.candidates {
            validate_attachment_id(&candidate.attachment_id)?;
        }
        // 第一阶段：标记（幂等：已标记的行保持原时间戳）。
        let mut transaction = self.begin_transaction(WriteScope::attachments())?;
        let mark_outcome = (|| -> Result<u32> {
            let mut marked = 0u32;
            for candidate in &plan.candidates {
                let updated = transaction
                    .transaction
                    .execute(
                        "UPDATE attachments SET gc_pending = 1, gc_pending_at = ?2
                          WHERE attachment_id = ?1 AND gc_pending = 0",
                        params![candidate.attachment_id, marked_at],
                    )
                    .map_err(StorageError::from)?;
                if updated > 0 {
                    transaction.note(1);
                    marked = marked.saturating_add(1);
                }
            }
            Ok(marked)
        })();
        let marked = match mark_outcome {
            Ok(marked) => {
                commit_transaction(transaction)?;
                marked
            }
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                return Err(error);
            }
        };
        report.marked = marked;
        // 第二阶段：删除（只删已标记的行；已删除的候选自动计入 skipped）。
        let mut transaction = self.begin_transaction(WriteScope::attachments())?;
        let delete_outcome = (|| -> Result<u32> {
            let mut removed = 0u32;
            for candidate in &plan.candidates {
                let deleted = transaction
                    .transaction
                    .execute(
                        "DELETE FROM attachments WHERE attachment_id = ?1 AND gc_pending = 1",
                        params![candidate.attachment_id],
                    )
                    .map_err(StorageError::from)?;
                if deleted > 0 {
                    transaction.note(deleted);
                    removed = removed.saturating_add(u32::try_from(deleted).unwrap_or(0));
                }
            }
            Ok(removed)
        })();
        match delete_outcome {
            Ok(removed) => {
                commit_transaction(transaction)?;
                report.removed = removed;
                let planned = u32::try_from(plan.candidates.len()).unwrap_or(u32::MAX);
                report.skipped = planned.saturating_sub(removed);
                Ok(report)
            }
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }

    /// 已标记待删除的附件 ID（崩溃恢复诊断用；最多 `limit` 条）。
    pub fn pending_attachment_gc(&self, limit: usize) -> Result<Vec<String>> {
        self.ensure_ready()?;
        let limit = limit.min(MAX_ATTACHMENT_GC_BATCH).max(1);
        let row_limit = i64::try_from(limit)
            .map_err(|_| StorageError::InvalidInput("attachment gc limit".to_owned()))?;
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT attachment_id FROM attachments WHERE gc_pending = 1
                  ORDER BY gc_pending_at ASC, attachment_id ASC LIMIT ?1",
            )
            .map_err(StorageError::from)?;
        let rows = statement
            .query_map([row_limit], |row| row.get::<_, String>(0))
            .map_err(StorageError::from)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(StorageError::from)?);
        }
        Ok(out)
    }
}
