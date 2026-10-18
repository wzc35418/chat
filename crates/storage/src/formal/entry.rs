//! STORAGE-008：把一条已通过协议/领域/加密校验的条目落成正式本地状态。
//!
//! 一次调用在**单笔 `BEGIN IMMEDIATE` 事务**内完成四件事，任一步失败整体回滚：
//!
//! 1. 会话行不存在时按条目携带的标题自动建行（`kind`/`coordinator_id`/`state` 用
//!    文档化的占位值，见 `AUTO_CREATED_KIND` 等常量），存在时只更新摘要；
//! 2. 用 `message-content` 用途把正文封装后写 `messages.protected_content`
//!    （AAD 与 STORAGE-012 的读取契约完全一致：`(conversation_id, message_id)`）；
//! 3. 写入满足 P0 schema 外键/唯一约束的 `message_attempts` 记账行
//!    （`messages.submission_id` 非空且引用它），SubmissionId 由
//!    `(conversation_id, message_id)` 确定性派生；
//! 4. 更新 `conversations.protected_metadata` 里的本地会话摘要（标题、预览文本、
//!    `updated_at_ms`、本机作者身份），用于会话列表展示与 `mine` 判定。
//!
//! 幂等语义按 `message_id`：同内容重放返回同一结果且不推进库版本；同一
//! `message_id` 对应不同内容（正文/作者/展示时间/会话不一致）显式返回
//! `MESSAGE_ID_CONFLICT`，绝不覆盖已写入的正式消息。
//!
//! 已知限制（详见 docs/implementation/storage.md 的 STORAGE-008 实现状态）：
//! - 本轮输入不携带 MLS 密文与日志序号：`group_seq` 由本函数按会话内 `MAX+1`
//!   单调分配，`message_attempts` 的记账行以 `immutable_ciphertext = 封装后的正文`
//!   与 `origin = 'remote'` 表达“该条目来自已验证日志路径，而非本机草稿准备”；
//! - `messages` 没有 `mine` 列，`mine` 以会话摘要记录的本机作者身份在读取时判定；
//! - 会话摘要暂存在 `conversations.protected_metadata`（P0 schema 没有独立的会话
//!   摘要表，本轮不新增迁移以免与其它模块的 schema 版本假设冲突）。

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::error::StorageError;
use super::seal::{ProtectedStorageKey, RecordSealer, SealPurpose};
use super::{
    CoreTransaction, MessageContent, Result, Store, WriteScope, candidate_hash,
    commit_transaction, digest_parts, ensure_id, hex, rollback_transaction,
};

/// 会话标题上限（UTF-8 字节）。
pub const MAX_SESSION_TITLE_BYTES: usize = 512;
/// 会话摘要预览的字符上限（按 Unicode 标量截断，不会截断半个字符）。
pub const MAX_SESSION_PREVIEW_CHARS: usize = 120;

/// 会话摘要封装格式版本（进入 AAD，格式变化必须换新值）。
const SESSION_SUMMARY_VERSION: &[u8] = b"session-summary-v1";
/// 派生 SubmissionId 的域分隔。
const ENTRY_SUBMISSION_DOMAIN: &[u8] = b"chat/storage/verified-entry-submission/v1";
/// 自动建行时的占位会话种类：本轮输入不携带会话种类，正式值由会话建立流程写入。
const AUTO_CREATED_KIND: &str = "direct";
/// 自动建行时的占位协调者：同上。
const AUTO_CREATED_COORDINATOR: &str = "";
/// 自动建行时的会话状态：能接收已认证条目即为 active。
const AUTO_CREATED_STATE: &str = "active";

/// 本地会话摘要：会话列表展示所需的本地状态，不与对端共享。
///
/// 封装在 `conversations.protected_metadata` 里（用途 `conversation-metadata`，
/// AAD 绑定会话 ID 与格式版本），因此磁盘上不出现标题或消息预览的明文。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    /// 会话标题（空字符串表示尚未得知标题，展示层自行回退）。
    pub title: String,
    /// 最新条目的预览文本（按字符截断，不含正文之外的任何衍生信息）。
    pub preview: String,
    /// 摘要最后更新时间（Unix 毫秒）。
    pub updated_at_ms: i64,
    /// 本机作者账户：由 `mine = true` 的条目记录，用于读取时判定 `mine`。
    pub local_account: Option<String>,
    /// 本机作者设备：同上（跨设备场景下作为补充证据）。
    pub local_device: Option<String>,
}

impl SessionSummary {
    /// 读取路径上的有界校验：越界或畸形摘要一律 `STATE_CORRUPTED`。
    fn validate_stored(&self) -> Result<()> {
        if self.title.len() > MAX_SESSION_TITLE_BYTES {
            return Err(StorageError::StateCorrupted(
                "stored session title exceeds the supported size".to_owned(),
            ));
        }
        if self.preview.chars().count() > MAX_SESSION_PREVIEW_CHARS {
            return Err(StorageError::StateCorrupted(
                "stored session preview exceeds the supported size".to_owned(),
            ));
        }
        if self.updated_at_ms < 0 {
            return Err(StorageError::StateCorrupted(
                "stored session summary carries a negative timestamp".to_owned(),
            ));
        }
        Ok(())
    }
}

/// STORAGE-008 输入：一条已通过外层校验的条目（形状由 core/FFI 竖直切片冻结）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedEntryInput {
    /// 会话 ID。
    pub conversation_id: String,
    /// 会话标题（仅在会话行不存在时用于建行；已存在时只在本地标题为空时补写）。
    pub title: String,
    /// 正式消息 ID。
    pub message_id: String,
    /// 作者账户 ID。
    pub author_account: String,
    /// 作者设备 ID。
    pub author_device: String,
    /// 明文正文（只在内存与封装缓冲中出现，绝不落盘）。
    pub body: String,
    /// 是否本机作者（只用于记录本地摘要里的本机身份，不写入 `messages`）。
    pub mine: bool,
    /// 展示时间（Unix 毫秒，来自已验证条目的展示时间）。
    pub created_at_ms: i64,
}

impl VerifiedEntryInput {
    /// 有界结构校验：ID 格式、标题/正文上限、时间戳。
    pub fn validate(&self) -> Result<()> {
        ensure_id("conversation", &self.conversation_id)?;
        ensure_id("message", &self.message_id)?;
        ensure_id("author account", &self.author_account)?;
        ensure_id("author device", &self.author_device)?;
        validate_title(&self.title)?;
        if self.created_at_ms < 0 {
            return Err(StorageError::InvalidInput(
                "created_at_ms must be a non-negative Unix millisecond timestamp".to_owned(),
            ));
        }
        chat_protocol::wire::validate_text(&self.body)
            .map_err(|error| StorageError::InvalidInput(error.to_string()))?;
        Ok(())
    }
}

/// STORAGE-008 结果：已提交的条目投影。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedEntry {
    /// 消息 ID。
    pub message_id: String,
    /// 会话 ID。
    pub conversation_id: String,
    /// 作者账户。
    pub author_account: String,
    /// 作者设备。
    pub author_device: String,
    /// 已解密的正文（可信核心路径内部使用）。
    pub body: String,
    /// 是否本机作者（按输入与本机摘要还原）。
    pub mine: bool,
    /// 展示时间。
    pub created_at_ms: i64,
    /// 本条目在会话内的正式序号。
    pub group_seq: u64,
    /// 关联的 SubmissionId（确定性派生）。
    pub submission_id: String,
    /// 本次调用是否命中已存在的同一消息（幂等重放）。
    pub already_existed: bool,
}

fn validate_title(title: &str) -> Result<()> {
    if title.len() > MAX_SESSION_TITLE_BYTES {
        return Err(StorageError::InvalidInput(format!(
            "session title exceeds {MAX_SESSION_TITLE_BYTES} bytes"
        )));
    }
    Ok(())
}

fn preview_of(body: &str) -> String {
    body.chars().take(MAX_SESSION_PREVIEW_CHARS).collect()
}

/// 由 `(conversation_id, message_id)` 确定性派生 SubmissionId（PROTOCOL-001 规范文本）。
///
/// 用于满足 `messages.submission_id` 的非空外键：同一消息重放派生同一 ID，
/// 因此不会因为重放产生新的 attempt 行。
pub fn verified_entry_submission_id(conversation_id: &str, message_id: &str) -> String {
    let digest = digest_parts(
        ENTRY_SUBMISSION_DOMAIN,
        &[conversation_id.as_bytes(), message_id.as_bytes()],
    );
    format!("sub:1:{}", hex(&digest))
}

fn session_aad(conversation_id: &str) -> [&[u8]; 2] {
    [conversation_id.as_bytes(), SESSION_SUMMARY_VERSION]
}

fn seal_session_summary(
    key: &ProtectedStorageKey,
    conversation_id: &str,
    summary: &SessionSummary,
) -> Result<Vec<u8>> {
    let plaintext = serde_json::to_vec(summary)
        .map_err(|error| StorageError::Failure(format!("cannot encode session summary: {error}")))?;
    let plaintext = Zeroizing::new(plaintext);
    let sealer = RecordSealer::new(key, SealPurpose::ConversationMetadata);
    sealer.seal(&session_aad(conversation_id), plaintext.as_slice())
}

fn open_session_summary(
    key: &ProtectedStorageKey,
    conversation_id: &str,
    sealed: &[u8],
) -> Result<SessionSummary> {
    let sealer = RecordSealer::new(key, SealPurpose::ConversationMetadata);
    let plaintext = sealer.open(&session_aad(conversation_id), sealed)?;
    let summary: SessionSummary = serde_json::from_slice(plaintext.as_slice()).map_err(|_| {
        StorageError::StateCorrupted("stored session summary is not readable".to_owned())
    })?;
    summary.validate_stored()?;
    Ok(summary)
}

fn load_session_summary(
    connection: &Connection,
    key: &ProtectedStorageKey,
    conversation_id: &str,
) -> Result<Option<SessionSummary>> {
    let sealed: Option<Vec<u8>> = connection
        .query_row(
            "SELECT protected_metadata FROM conversations WHERE conversation_id = ?1",
            [conversation_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(sealed) = sealed else {
        return Ok(None);
    };
    open_session_summary(key, conversation_id, &sealed).map(Some)
}

fn insert_conversation(
    transaction: &mut CoreTransaction<'_>,
    conversation_id: &str,
    summary: &SessionSummary,
) -> Result<()> {
    let sealed = seal_session_summary(transaction.key, conversation_id, summary)?;
    let rows = transaction.transaction.execute(
        "INSERT INTO conversations (conversation_id, kind, coordinator_id, protected_metadata, state)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            conversation_id,
            AUTO_CREATED_KIND,
            AUTO_CREATED_COORDINATOR,
            sealed,
            AUTO_CREATED_STATE
        ],
    )?;
    transaction.note(rows);
    Ok(())
}

fn update_session_summary(
    transaction: &mut CoreTransaction<'_>,
    conversation_id: &str,
    summary: &SessionSummary,
) -> Result<()> {
    let sealed = seal_session_summary(transaction.key, conversation_id, summary)?;
    let rows = transaction.transaction.execute(
        "UPDATE conversations SET protected_metadata = ?2 WHERE conversation_id = ?1",
        params![conversation_id, sealed],
    )?;
    if rows != 1 {
        return Err(StorageError::StateCorrupted(format!(
            "conversation {conversation_id} disappeared during the entry write"
        )));
    }
    transaction.note(rows);
    Ok(())
}

/// 在调用方事务内确保会话行存在（不存在则按标题建行），返回当前摘要。
///
/// 只允许 intent（T1 草稿）与 inbound（T3 已验证条目）作用域使用：恢复与 outbox
/// 作用域不应隐式创建会话。
pub fn materialize_session(
    transaction: &mut CoreTransaction<'_>,
    conversation_id: &str,
    title: &str,
) -> Result<SessionSummary> {
    ensure_id("conversation", conversation_id)?;
    validate_title(title)?;
    if !matches!(
        transaction.scope().kind(),
        super::WriteScopeKind::Intent | super::WriteScopeKind::Inbound
    ) {
        return Err(StorageError::InvalidInput(
            "materialize_session requires an intent or inbound write scope".to_owned(),
        ));
    }
    if let Some(summary) =
        load_session_summary(&transaction.transaction, transaction.key, conversation_id)?
    {
        return Ok(summary);
    }
    let summary = SessionSummary {
        title: title.to_owned(),
        preview: String::new(),
        updated_at_ms: super::now_ms(),
        local_account: None,
        local_device: None,
    };
    insert_conversation(transaction, conversation_id, &summary)?;
    Ok(summary)
}

struct StoredMessage {
    conversation_id: String,
    author_account: String,
    author_device: String,
    group_seq: i64,
    submission_id: String,
    display_created_at: i64,
    protected_content: Vec<u8>,
}

fn load_message(connection: &Connection, message_id: &str) -> Result<Option<StoredMessage>> {
    let row: Option<(String, String, String, i64, String, i64, Vec<u8>)> = connection
        .query_row(
            "SELECT conversation_id, author_account_id, author_device_id, group_seq,
                    submission_id, display_created_at, protected_content
               FROM messages WHERE message_id = ?1",
            [message_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;
    let Some((
        conversation_id,
        author_account,
        author_device,
        group_seq,
        submission_id,
        display_created_at,
        protected_content,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(StoredMessage {
        conversation_id,
        author_account,
        author_device,
        group_seq,
        submission_id,
        display_created_at,
        protected_content,
    }))
}

fn open_message_body(
    key: &ProtectedStorageKey,
    conversation_id: &str,
    message_id: &str,
    sealed: &[u8],
) -> Result<MessageContent> {
    let sealer = RecordSealer::new(key, SealPurpose::MessageContent);
    let plaintext = sealer.open(
        &[conversation_id.as_bytes(), message_id.as_bytes()],
        sealed,
    )?;
    MessageContent::from_canonical(plaintext.as_slice())
}

fn next_group_seq(connection: &Connection, conversation_id: &str) -> Result<i64> {
    let maximum: Option<i64> = connection.query_row(
        "SELECT MAX(group_seq) FROM messages WHERE conversation_id = ?1",
        [conversation_id],
        |row| row.get(0),
    )?;
    maximum.unwrap_or(0).checked_add(1).ok_or_else(|| {
        StorageError::StateCorrupted("conversation message sequence overflowed".to_owned())
    })
}

fn map_message_write_error(error: rusqlite::Error, message_id: &str) -> StorageError {
    if matches!(
        &error,
        rusqlite::Error::SqliteFailure(code, _) if code.code == rusqlite::ErrorCode::ConstraintViolation
    ) {
        return StorageError::MessageIdConflict(format!(
            "message {message_id} violates a message constraint"
        ));
    }
    error.into()
}

/// 按 `message_id` 比对已存在的正式消息。
///
/// 返回 `Ok(Some(..))` 表示同一内容（幂等重放），`Err(MESSAGE_ID_CONFLICT)` 表示同一
/// ID 绑定了不同条目，`Ok(None)` 表示尚未写入。
fn compare_existing_entry(
    connection: &Connection,
    key: &ProtectedStorageKey,
    input: &VerifiedEntryInput,
    content: &MessageContent,
) -> Result<Option<AppliedEntry>> {
    let Some(existing) = load_message(connection, &input.message_id)? else {
        return Ok(None);
    };
    let stored = open_message_body(
        key,
        &existing.conversation_id,
        &input.message_id,
        &existing.protected_content,
    )?;
    let same_entry = existing.conversation_id == input.conversation_id
        && existing.author_account == input.author_account
        && existing.author_device == input.author_device
        && existing.display_created_at == input.created_at_ms
        && stored == *content;
    if !same_entry {
        return Err(StorageError::MessageIdConflict(format!(
            "message {} is already bound to a different verified entry",
            input.message_id
        )));
    }
    Ok(Some(AppliedEntry {
        message_id: input.message_id.clone(),
        conversation_id: existing.conversation_id,
        author_account: existing.author_account,
        author_device: existing.author_device,
        body: input.body.clone(),
        mine: input.mine,
        created_at_ms: existing.display_created_at,
        group_seq: u64::try_from(existing.group_seq).map_err(|_| {
            StorageError::StateCorrupted("stored message sequence is negative".to_owned())
        })?,
        submission_id: existing.submission_id,
        already_existed: true,
    }))
}

fn write_verified_entry(
    transaction: &mut CoreTransaction<'_>,
    input: VerifiedEntryInput,
) -> Result<AppliedEntry> {
    input.validate()?;
    if transaction.scope().kind() != super::WriteScopeKind::Inbound {
        return Err(StorageError::InvalidInput(
            "apply_verified_entry requires an inbound write scope".to_owned(),
        ));
    }
    if let Some(scope_conversation) = transaction.scope().conversation_id()
        && scope_conversation != input.conversation_id
    {
        return Err(StorageError::InvalidInput(
            "write scope covers a different conversation".to_owned(),
        ));
    }

    let content = MessageContent::text(input.body.clone())?;

    // 步骤 1：按 message_id 幂等。同内容直接返回，不推进库版本、不重复写行。
    if let Some(applied) =
        compare_existing_entry(&transaction.transaction, transaction.key, &input, &content)?
    {
        return Ok(applied);
    }

    // 步骤 2：会话行自动创建 / 摘要就地更新。
    let mut summary = match load_session_summary(
        &transaction.transaction,
        transaction.key,
        &input.conversation_id,
    )? {
        Some(existing) => existing,
        None => {
            let created = SessionSummary {
                title: input.title.clone(),
                preview: String::new(),
                updated_at_ms: 0,
                local_account: None,
                local_device: None,
            };
            insert_conversation(transaction, &input.conversation_id, &created)?;
            created
        }
    };
    if summary.title.is_empty() && !input.title.is_empty() {
        summary.title = input.title.clone();
    }
    if summary.preview.is_empty() || input.created_at_ms >= summary.updated_at_ms {
        summary.preview = preview_of(&input.body);
    }
    summary.updated_at_ms = summary.updated_at_ms.max(input.created_at_ms);
    if input.mine {
        summary.local_account = Some(input.author_account.clone());
        summary.local_device = Some(input.author_device.clone());
    }
    update_session_summary(transaction, &input.conversation_id, &summary)?;

    // 步骤 3：满足外键/唯一约束的 attempt 记账行（正文由 messages 行承载）。
    let submission_id = verified_entry_submission_id(&input.conversation_id, &input.message_id);
    let existing_attempt: Option<String> = transaction
        .transaction
        .query_row(
            "SELECT conversation_id FROM message_attempts WHERE submission_id = ?1",
            [&submission_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(attempt_conversation) = existing_attempt {
        if attempt_conversation != input.conversation_id {
            return Err(StorageError::StateCorrupted(format!(
                "derived submission {submission_id} is bound to another conversation"
            )));
        }
    } else {
        let sealer = RecordSealer::new(transaction.key, SealPurpose::MessageContent);
        let sealed_body = sealer.seal(
            &[
                input.conversation_id.as_bytes(),
                input.message_id.as_bytes(),
            ],
            content.canonical_bytes(),
        )?;
        let candidate = candidate_hash(&input.conversation_id, &submission_id, 0, &sealed_body);
        transaction.transaction.execute(
            "INSERT INTO message_attempts (submission_id, origin, draft_id, operation_id, conversation_id, mls_epoch, immutable_ciphertext, candidate_hash, state)
             VALUES (?1, 'remote', NULL, NULL, ?2, 0, ?3, ?4, 'accepted')",
            params![
                submission_id,
                input.conversation_id,
                sealed_body,
                candidate.as_slice()
            ],
        )?;
        transaction.note(1);
    }

    // 步骤 4：正式消息行（正文只以封装形式落盘）。
    let group_seq = next_group_seq(&transaction.transaction, &input.conversation_id)?;
    let sealed = {
        let sealer = RecordSealer::new(transaction.key, SealPurpose::MessageContent);
        sealer.seal(
            &[
                input.conversation_id.as_bytes(),
                input.message_id.as_bytes(),
            ],
            content.canonical_bytes(),
        )?
    };
    let inserted = transaction.transaction.execute(
        "INSERT INTO messages (message_id, conversation_id, draft_id, submission_id,
                               author_account_id, author_device_id, group_seq,
                               protected_content, display_created_at, send_state)
         VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, 'accepted')",
        params![
            input.message_id,
            input.conversation_id,
            submission_id,
            input.author_account,
            input.author_device,
            group_seq,
            sealed,
            input.created_at_ms
        ],
    );
    match inserted {
        Ok(rows) => transaction.note(rows),
        Err(error) => return Err(map_message_write_error(error, &input.message_id)),
    }

    Ok(AppliedEntry {
        message_id: input.message_id,
        conversation_id: input.conversation_id,
        author_account: input.author_account,
        author_device: input.author_device,
        body: input.body,
        mine: input.mine,
        created_at_ms: input.created_at_ms,
        group_seq: u64::try_from(group_seq)
            .map_err(|_| StorageError::StateCorrupted("assigned sequence is negative".to_owned()))?,
        submission_id,
        already_existed: false,
    })
}

/// STORAGE-008：在调用方事务内写入一条已验证条目，失败时把事务标记为 poisoned
/// （拒绝后续提交，保证不半提交）。
pub fn apply_verified_entry(
    transaction: &mut CoreTransaction<'_>,
    input: VerifiedEntryInput,
) -> Result<AppliedEntry> {
    let outcome = write_verified_entry(transaction, input);
    if outcome.is_err() {
        transaction.poison();
    }
    outcome
}

impl Store {
    /// STORAGE-008 的生产入口：开启单笔 inbound 事务、写入并提交；失败整体回滚。
    pub fn apply_verified_entry(&mut self, input: VerifiedEntryInput) -> Result<AppliedEntry> {
        self.ensure_ready()?;
        input.validate()?;
        // 幂等重放走只读路径：不开启事务，因此也不推进库版本。
        let content = MessageContent::text(input.body.clone())?;
        if let Some(applied) = compare_existing_entry(self.connection(), &self.key, &input, &content)?
        {
            return Ok(applied);
        }
        let mut transaction = self.begin_transaction(WriteScope::inbound(&input.conversation_id))?;
        match apply_verified_entry(&mut transaction, input) {
            Ok(applied) => {
                commit_transaction(transaction)?;
                Ok(applied)
            }
            Err(error) => match rollback_transaction(transaction) {
                Ok(()) => Err(error),
                Err(rollback) => Err(rollback),
            },
        }
    }

    /// 读取本地会话摘要；会话行不存在返回 `None`。
    ///
    /// 摘要封装认证失败（被篡改、挪到别的会话或不是本格式）一律
    /// `STATE_CORRUPTED`，绝不猜测性降级。
    pub fn session_summary(&self, conversation_id: &str) -> Result<Option<SessionSummary>> {
        self.ensure_ready()?;
        ensure_id("conversation", conversation_id)?;
        load_session_summary(self.connection(), &self.key, conversation_id)
    }

    /// 草稿当前关联的 SubmissionId（尚无尝试时返回 `None`）。
    pub fn draft_submission_id(&self, draft_id: &str) -> Result<Option<String>> {
        self.ensure_ready()?;
        ensure_id("draft", draft_id)?;
        let submission: Option<String> = self
            .connection
            .query_row(
                "SELECT submission_id FROM message_attempts
                  WHERE draft_id = ?1 ORDER BY submission_id DESC LIMIT 1",
                [draft_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(submission)
    }
}
