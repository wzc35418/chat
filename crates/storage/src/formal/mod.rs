//! 正式加密客户端存储（STORAGE-001/003/006/007/009/010/011 + P0-04 基础；
//! 分页查询 012/013 见子模块 `query`、outbox 租约 014/015 见子模块 `outbox_lease`）。
//!
//! 与 crate::store（隔离的 P0-03 未加密实验）完全分开，互不复用连接或表：
//! - 打开时取得独占写锁，配置 journal_mode=WAL / synchronous=FULL / foreign_keys=ON；
//! - 主密钥（32 字节）只用于派生用途子密钥，敏感列（消息正文、MLS 秘密、隔离诊断、
//!   密钥校验记录）一律用 ChaCha20-Poly1305 封装，磁盘上不出现明文与密钥字节；
//! - 所有写入只能通过 CoreTransaction（BEGIN IMMEDIATE）完成，句柄只能消费一次，
//!   出错的事务被标记为 poisoned 并拒绝提交（禁止半提交）；
//! - 提交只有在 SQLite 明确 COMMIT 成功后才返回 CommitToken；失败先回滚；
//! - recover_incomplete_writes 用恢复标记核对已提交状态，只按已存在的字节恢复，
//!   绝不重新加密、也绝不从标记重建消息。

mod attachments;
mod cursor;
mod directory;
mod entry;
mod error;
mod identity;
mod mls_state;
mod migrations;
mod outbox_lease;
mod plugins;
mod query;
mod seal;

pub use attachments::{
    ATTACHMENT_DIGEST_BYTES, ATTACHMENT_DIRECTIONS, ATTACHMENT_FILE_ID_BYTES,
    ATTACHMENT_FILE_KEY_BYTES, ATTACHMENT_STATES, ATTACHMENT_STATE_TRANSITIONS, AttachmentProgress,
    AttachmentProgressInput, AttachmentRecord, AttachmentRecordInput, AttachmentSecretInput,
    AttachmentSecretRecord, MAX_ATTACHMENTS, MAX_ATTACHMENT_CHUNKS, MAX_ATTACHMENT_CHUNK_SIZE,
    MAX_ATTACHMENT_DISPLAY_NAME_BYTES, MAX_ATTACHMENT_ID_BYTES, MAX_ATTACHMENT_LIST,
    MAX_ATTACHMENT_PLAIN_BYTES, MAX_ATTACHMENT_PROGRESS_ID_BYTES, MAX_MESSAGE_ATTACHMENT_LIST,
};
pub use cursor::PageCursor;
pub use directory::{
    CREDENTIAL_DIRECTORY_ID_BYTES, CredentialDirectoryRecord, MAX_CREDENTIAL_BYTES,
    MAX_CREDENTIAL_DIRECTORY_ROWS,
};
pub use entry::{
    AppliedEntry, MAX_SESSION_PREVIEW_CHARS, MAX_SESSION_TITLE_BYTES, SessionSummary,
    VerifiedEntryInput, apply_verified_entry, materialize_session, verified_entry_submission_id,
};
pub use error::StorageError;
pub use identity::{
    ACCOUNT_SEED_BYTES, ContactRecord, ContactUpsertInput, ConsumedInviteRecord,
    DEVICE_CAPABILITY_MASK, DEVICE_SEED_BYTES, DEVICE_SIGNATURE_BYTES, DeviceCredentialInput,
    DeviceCredentialRecord, DeviceRevocationRecord, DeviceRevocationSetRecord, FINGERPRINT_BYTES,
    IdentityAccountInput, IdentityAccountRecord, MAX_CONTACTS, MAX_DEVICE_CREDENTIALS,
    MAX_DEVICE_REVOCATIONS, MAX_IDENTITY_DISPLAY_NAME_BYTES, PUBLIC_KEY_BYTES, contact_id_for,
};
pub use mls_state::{
    MAX_GROUP_STATE_LIST, MAX_MLS_GROUP_ID_BYTES, MAX_MLS_GROUP_RECORD_BYTES,
    MAX_MLS_GROUP_STATE_BYTES, MAX_MLS_RECORDS_PER_GROUP, MAX_MLS_RECORD_BYTES,
    MAX_MLS_RECORD_KEY_BYTES, MlsGroupStateInput, MlsGroupStateKey, MlsGroupStateRecord,
    MlsRecordInput, MlsRecordRecord, MlsWriteCheckpoint,
};
pub use migrations::MigrationReport;
pub use plugins::{
    MAX_PLUGIN_ID_BYTES, MAX_PLUGIN_KEY_BYTES, MAX_PLUGIN_KV_ENTRIES, MAX_PLUGIN_KV_TOTAL_BYTES,
    MAX_PLUGIN_KV_VALUE_BYTES, MAX_PLUGIN_MANIFEST_BYTES, MAX_PLUGIN_ROWS, MAX_PLUGIN_STATE_BYTES,
    PluginRecord,
};
pub use outbox_lease::{
    JobOutcome, JobResult, JobState, LeasedJob, MAX_CLAIM_BATCH, MAX_CLAIM_BATCH_BYTES,
    MAX_DIAGNOSTIC_CHARS, OUTBOX_LEASE_MS, OUTBOX_RETRY_BASE_MS, OUTBOX_RETRY_MAX_MS,
    OUTBOX_UNKNOWN_RECONCILE_MS,
};
pub use query::{
    ConversationPage, ConversationView, MAX_CONVERSATION_PAGE_LIMIT, MAX_MESSAGE_PAGE_LIMIT,
    MessagePage, MessageView, MessageViewState,
};
pub use seal::{
    MASTER_KEY_BYTES, MAX_SEAL_PLAINTEXT_BYTES, MAX_SEALED_BYTES, ProtectedStorageKey, SealPurpose,
};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

use seal::RecordSealer;

/// 结果类型：稳定错误码见 StorageError::code。
pub type Result<T> = std::result::Result<T, StorageError>;

/// ID 字段上限（字节）。
pub const MAX_ID_BYTES: usize = 128;
/// 消息正文上限（与线协议保持一致）。
pub const MAX_TEXT_BYTES: usize = chat_protocol::wire::MAX_TEXT_BYTES;
/// 单次尝试密文上限（与线协议帧一致）。
pub const MAX_CIPHERTEXT_BYTES: usize = chat_protocol::wire::MAX_FRAME_BYTES;
/// 单次事务写入的 MLS 记录变化条数上限。
pub const MAX_CRYPTO_CHANGES: usize = 256;
/// 单条 MLS 记录键上限。
pub const MAX_RECORD_KEY_BYTES: usize = 256;
/// 单条 MLS 记录值上限（未封装明文）。
pub const MAX_RECORD_VALUE_BYTES: usize = 256 * 1024;
/// 单次事务 MLS 变化总字节上限。
pub const MAX_CRYPTO_CHANGE_BYTES: usize = 1024 * 1024;
/// 单次恢复处理的标记上限；超过即视为需要人工恢复（有界拒绝）。
pub const MAX_RECOVERY_MARKERS: usize = 4096;

const DRAFT_ID_DOMAIN: &[u8] = b"chat/storage/draft-id/v1";
const JOB_ID_DOMAIN: &[u8] = b"chat/storage/outbox-job-id/v1";
const REQUEST_DOMAIN: &[u8] = b"chat/storage/send-request/v1";
const CANDIDATE_DOMAIN: &[u8] = b"chat/storage/attempt-digest/v1";
const VERIFIER_CONTEXT: &[u8] = b"store-key-check";
const VERIFIER_PLAINTEXT: &[u8] = b"chat/storage/key-verifier/v1";

/// 当前时间（Unix 毫秒，饱和到 i64）。
pub(crate) fn now_ms() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

/// 小写十六进制编码。
pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        output.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    output
}

/// 域分隔 + 长度前缀的规范化摘要。
fn digest_parts(domain: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    let digest = hasher.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(digest.as_slice());
    bytes
}

fn draft_id_for(conversation_id: &str, operation_id: &str) -> String {
    let digest = digest_parts(
        DRAFT_ID_DOMAIN,
        &[conversation_id.as_bytes(), operation_id.as_bytes()],
    );
    format!("draft-{}", hex(&digest[..16]))
}

fn prepare_job_id_for(draft_id: &str) -> String {
    let digest = digest_parts(JOB_ID_DOMAIN, &[b"prepare", draft_id.as_bytes()]);
    format!("job-{}", hex(&digest[..16]))
}

fn send_job_id_for(submission_id: &str) -> String {
    let digest = digest_parts(JOB_ID_DOMAIN, &[b"send", submission_id.as_bytes()]);
    format!("job-{}", hex(&digest[..16]))
}

fn candidate_hash(
    conversation_id: &str,
    submission_id: &str,
    mls_epoch: u64,
    ciphertext: &[u8],
) -> [u8; 32] {
    digest_parts(
        CANDIDATE_DOMAIN,
        &[
            conversation_id.as_bytes(),
            submission_id.as_bytes(),
            &mls_epoch.to_be_bytes(),
            ciphertext,
        ],
    )
}

fn ensure_id(kind: &str, value: &str) -> Result<()> {
    let valid = !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/')
        });
    if !valid {
        return Err(StorageError::InvalidInput(format!("invalid {kind} ID")));
    }
    Ok(())
}

fn digest_bytes(value: Vec<u8>) -> Result<[u8; 32]> {
    let bytes: [u8; 32] = value.try_into().map_err(|_| {
        StorageError::StateCorrupted("stored digest has an invalid length".to_owned())
    })?;
    Ok(bytes)
}

fn describe(error: anyhow::Error) -> String {
    error.to_string().chars().take(200).collect()
}

/// 主密钥的派生/加载来源。写入 store_key_check 后不可更改；重开时必须完全一致。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kdf", rename_all = "snake_case")]
pub enum KeyProvenance {
    /// 直接提供的 32 字节主密钥（平台密钥库解包后的结果）。
    Raw32,
    /// 由口令经 Argon2id 派生的主密钥；参数与盐都必须与首次创建一致。
    Argon2id {
        /// 内存成本（KiB），有界 8 MiB..=1 GiB。
        m_cost_kib: u32,
        /// 迭代次数，有界 1..=16。
        t_cost: u32,
        /// 并行度，有界 1..=8。
        parallelism: u32,
        /// 盐（16..=64 字节）。
        salt: Vec<u8>,
    },
}

impl KeyProvenance {
    /// 稳定 KDF 版本号。
    pub const fn kdf_version(&self) -> u16 {
        match self {
            Self::Raw32 => 1,
            Self::Argon2id { .. } => 2,
        }
    }

    /// 有界校验（畸形或越界参数必须显式拒绝）。
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Raw32 => Ok(()),
            Self::Argon2id {
                m_cost_kib,
                t_cost,
                parallelism,
                salt,
            } => {
                if !(8 * 1024..=1024 * 1024).contains(m_cost_kib) {
                    return Err(StorageError::InvalidInput(
                        "argon2id m_cost_kib must be within 8 MiB..=1 GiB".to_owned(),
                    ));
                }
                if !(1..=16).contains(t_cost) {
                    return Err(StorageError::InvalidInput(
                        "argon2id t_cost must be within 1..=16".to_owned(),
                    ));
                }
                if !(1..=8).contains(parallelism) {
                    return Err(StorageError::InvalidInput(
                        "argon2id parallelism must be within 1..=8".to_owned(),
                    ));
                }
                if !(16..=64).contains(&salt.len()) {
                    return Err(StorageError::InvalidInput(
                        "argon2id salt must be within 16..=64 bytes".to_owned(),
                    ));
                }
                Ok(())
            }
        }
    }

    fn encode(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self)
            .map_err(|error| StorageError::Failure(format!("cannot encode key provenance: {error}")))
    }
}

/// 打开正式存储所需的选项。数据目录必须已存在，不隐式创建目录。
#[derive(Clone, Debug)]
pub struct StoreOptions {
    /// 数据库文件路径。
    pub path: PathBuf,
    /// 主密钥来源；重开时必须与库内记录一致。
    pub key_provenance: KeyProvenance,
    /// 等锁超时；打开阶段的独占锁探测固定使用 0 超时（立即失败）。
    pub busy_timeout: Duration,
}

impl StoreOptions {
    /// 以默认 3 秒等锁超时构造。
    pub fn new(path: impl Into<PathBuf>, key_provenance: KeyProvenance) -> Self {
        Self {
            path: path.into(),
            key_provenance,
            busy_timeout: Duration::from_secs(3),
        }
    }

    /// 覆盖等锁超时。
    pub fn with_busy_timeout(mut self, timeout: Duration) -> Self {
        self.busy_timeout = timeout;
        self
    }
}

/// 写事务作用域种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteScopeKind {
    /// T1：草稿 + 准备任务。
    Intent,
    /// T2：attempt + MLS provider 记录 + 发送任务。
    Attempt,
    /// 启动恢复。
    Recovery,
    /// outbox 租约与结果标记（STORAGE-014/015）：不绑定单个会话。
    Outbox,
    /// T3：已验证条目写入（STORAGE-008），只作用在单个会话上。
    Inbound,
    /// 身份账户/联系人/邀请消费（P1 身份持久化）：只动身份表，不绑定会话。
    Identity,
    /// MLS 群状态与 provider 记录（CRYPTO-004/010 持久化切片）：只动 mls_* 表。
    MlsState,
    /// 附件记录（ATTACHMENTS-001 持久化）：只动 attachments 表，不绑定会话。
    Attachments,
}

/// 受限写事务的作用域。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteScope {
    kind: WriteScopeKind,
    conversation_id: Option<String>,
    expected_version: Option<u64>,
}

impl WriteScope {
    /// T1 草稿作用域。
    pub fn intent(conversation_id: impl Into<String>) -> Self {
        Self {
            kind: WriteScopeKind::Intent,
            conversation_id: Some(conversation_id.into()),
            expected_version: None,
        }
    }

    /// T2 准备尝试作用域。
    pub fn attempt(conversation_id: impl Into<String>) -> Self {
        Self {
            kind: WriteScopeKind::Attempt,
            conversation_id: Some(conversation_id.into()),
            expected_version: None,
        }
    }

    /// 启动恢复作用域（不受单会话限制）。
    pub fn recovery() -> Self {
        Self {
            kind: WriteScopeKind::Recovery,
            conversation_id: None,
            expected_version: None,
        }
    }

    /// STORAGE-008 已验证条目作用域（单会话）。
    pub fn inbound(conversation_id: impl Into<String>) -> Self {
        Self {
            kind: WriteScopeKind::Inbound,
            conversation_id: Some(conversation_id.into()),
            expected_version: None,
        }
    }

    /// outbox 租约作用域（不受单会话限制；只动 outbox_jobs 与 attempt 状态）。
    pub fn outbox() -> Self {
        Self {
            kind: WriteScopeKind::Outbox,
            conversation_id: None,
            expected_version: None,
        }
    }

    /// 身份持久化作用域（不受单会话限制；只动 identity_account/contacts/consumed_invites）。
    pub fn identity() -> Self {
        Self {
            kind: WriteScopeKind::Identity,
            conversation_id: None,
            expected_version: None,
        }
    }

    /// 附件记录作用域（不受单会话限制；只动 attachments 表）。
    pub fn attachments() -> Self {
        Self {
            kind: WriteScopeKind::Attachments,
            conversation_id: None,
            expected_version: None,
        }
    }

    /// MLS 状态持久化作用域（不受单会话限制；只动 mls_group_state/mls_record）。
    pub fn mls_state() -> Self {
        Self {
            kind: WriteScopeKind::MlsState,
            conversation_id: None,
            expected_version: None,
        }
    }

    /// 要求库版本严格等于 version，否则事务开启即失败。
    pub fn with_expected_version(mut self, version: u64) -> Self {
        self.expected_version = Some(version);
        self
    }

    /// 作用域种类。
    pub fn kind(&self) -> WriteScopeKind {
        self.kind
    }

    /// 绑定的会话（恢复作用域为空）。
    pub fn conversation_id(&self) -> Option<&str> {
        self.conversation_id.as_deref()
    }

    /// 期望库版本。
    pub fn expected_version(&self) -> Option<u64> {
        self.expected_version
    }

    fn validate(&self) -> Result<()> {
        if let Some(conversation_id) = self.conversation_id.as_deref() {
            ensure_id("conversation", conversation_id)?;
        }
        Ok(())
    }
}

/// 存储连接状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreStatus {
    /// 可继续读写。
    Ready,
    /// 回滚/提交失败后进入 fail-closed，只能关闭后重新打开。
    Faulted,
}

/// 打开时检查到的未完成恢复项（不自动处理，必须显式调用 recover_incomplete_writes）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecoveryPending {
    /// 待处理的恢复标记数。
    pub markers: u64,
    /// 已过期但未清除的 outbox 租约数。
    pub expired_leases: u64,
}

/// 成功提交后的唯一发布凭证；只能由 commit_transaction 产生。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitToken {
    /// 提交后的库版本。
    pub committed_version: u64,
    /// 本次事务内计数到的写入行数。
    pub changed_rows: u64,
}

/// 用户消息内容。P0-04 只实现 Text，附件/扩展在后续模块补齐。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageContent {
    /// 文本正文（UTF-8，非空且不超过线协议上限）。
    Text(String),
}

impl MessageContent {
    /// 构造并校验文本正文。
    pub fn text(value: impl Into<String>) -> Result<Self> {
        let content = Self::Text(value.into());
        content.validate()?;
        Ok(content)
    }

    /// 校验正文（非空、UTF-8 字节上限）。
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Text(text) => chat_protocol::wire::validate_text(text)
                .map_err(|error| StorageError::InvalidInput(describe(error))),
        }
    }

    fn tag(&self) -> u8 {
        match self {
            Self::Text(_) => 1,
        }
    }

    fn canonical_bytes(&self) -> &[u8] {
        match self {
            Self::Text(text) => text.as_bytes(),
        }
    }

    fn from_canonical(bytes: &[u8]) -> Result<Self> {
        let text = String::from_utf8(bytes.to_vec()).map_err(|_| {
            StorageError::StateCorrupted("sealed draft content is not valid UTF-8".to_owned())
        })?;
        let content = Self::Text(text);
        content.validate().map_err(|_| {
            StorageError::StateCorrupted(
                "sealed draft content violates the message contract".to_owned(),
            )
        })?;
        Ok(content)
    }
}

/// 消息来源（由可信调用路径设置，UI 不能自选）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageOrigin {
    /// 用户在编辑区发起。
    UserInput,
    /// 本地重建（例如被拒绝后保留草稿重新构造）。
    LocalRebuild,
}

impl MessageOrigin {
    fn tag(self) -> u8 {
        match self {
            Self::UserInput => 1,
            Self::LocalRebuild => 2,
        }
    }
}

/// 已通过外层校验的发送请求（STORAGE-006 输入）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedSendRequest {
    /// 会话 ID。
    pub conversation_id: String,
    /// 用户操作幂等键（本账户内唯一）。
    pub client_operation_id: String,
    /// 消息内容。
    pub content: MessageContent,
    /// 回复目标消息 ID。
    pub reply_to: Option<String>,
    /// 来源。
    pub origin: MessageOrigin,
    /// 本地创建时间（Unix 毫秒，只用于本地展示/排序，不参与请求摘要）。
    pub created_at: i64,
}

impl ValidatedSendRequest {
    /// 结构校验：ID 格式、内容、时间。
    pub fn validate(&self) -> Result<()> {
        ensure_id("conversation", &self.conversation_id)?;
        ensure_id("operation", &self.client_operation_id)?;
        if let Some(reply_to) = self.reply_to.as_deref() {
            ensure_id("reply message", reply_to)?;
        }
        if self.created_at < 0 {
            return Err(StorageError::InvalidInput(
                "created_at must be a non-negative Unix millisecond timestamp".to_owned(),
            ));
        }
        self.content.validate()
    }

    /// 请求摘要：决定同一 operation ID 是否同一请求。不包含 created_at（重试时钟可变）。
    pub fn request_digest(&self) -> [u8; 32] {
        let content_tag = [self.content.tag()];
        let has_reply = [u8::from(self.reply_to.is_some())];
        let origin_tag = [self.origin.tag()];
        let reply = self.reply_to.as_deref().unwrap_or("");
        digest_parts(
            REQUEST_DOMAIN,
            &[
                self.conversation_id.as_bytes(),
                self.client_operation_id.as_bytes(),
                &content_tag,
                self.content.canonical_bytes(),
                &has_reply,
                reply.as_bytes(),
                &origin_tag,
            ],
        )
    }
}

/// T1 结果：已提交的草稿投影（content 为已解密正文，只用于可信核心路径）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftRecord {
    /// 草稿 ID（由会话 + operation ID 确定性派生）。
    pub draft_id: String,
    /// 会话 ID。
    pub conversation_id: String,
    /// 用户操作幂等键。
    pub operation_id: String,
    /// 请求摘要。
    pub request_hash: [u8; 32],
    /// 正文。
    pub content: MessageContent,
    /// 草稿状态（P0-04 只在 queued 上工作）。
    pub state: String,
    /// 创建时间。
    pub created_at: i64,
    /// 本次调用是否命中已存在草稿（幂等重放）。
    pub already_existed: bool,
}

/// 单条 MLS provider 记录变化（T2 的 crypto 部分）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CryptoRecordChange {
    /// provider 记录键（原始字节）。
    pub record_key: Vec<u8>,
    /// 目标记录版本；新记录直接插入，更新必须等于旧版本 + 1。
    pub record_version: u64,
    /// 未封装的秘密值；写入前用主密钥封装，Drop 时清零。
    pub value: Zeroizing<Vec<u8>>,
}

impl CryptoRecordChange {
    /// 构造一条记录变化。
    pub fn new(
        record_key: impl Into<Vec<u8>>,
        record_version: u64,
        value: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            record_key: record_key.into(),
            record_version,
            value: Zeroizing::new(value.into()),
        }
    }
}

/// T2 输入：crypto 在同事务中准备好的不可变尝试。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedWrite {
    /// 关联草稿。
    pub draft_id: String,
    /// 会话 ID；必须与草稿一致。
    pub conversation_id: String,
    /// 本次尝试的 SubmissionId；网络重试必须复用同 ID 同字节。
    pub submission_id: String,
    /// 加密时使用的 MLS epoch。
    pub mls_epoch: u64,
    /// 不可变密文（外层已签名候选的 payload）。
    pub immutable_ciphertext: Vec<u8>,
    /// 创建该尝试时的草稿请求摘要；必须与库内草稿一致。
    pub request_hash: [u8; 32],
    /// 同事务产生的 MLS provider 记录变化。
    pub crypto_changes: Vec<CryptoRecordChange>,
}

impl PreparedWrite {
    /// 有界校验。
    pub fn validate(&self) -> Result<()> {
        ensure_id("draft", &self.draft_id)?;
        ensure_id("conversation", &self.conversation_id)?;
        ensure_id("submission", &self.submission_id)?;
        if self.immutable_ciphertext.is_empty()
            || self.immutable_ciphertext.len() > MAX_CIPHERTEXT_BYTES
        {
            return Err(StorageError::InvalidInput(
                "immutable ciphertext length is outside the protocol bounds".to_owned(),
            ));
        }
        if self.crypto_changes.len() > MAX_CRYPTO_CHANGES {
            return Err(StorageError::InvalidInput(
                "too many MLS record changes in one attempt".to_owned(),
            ));
        }
        let mut total = 0usize;
        for change in &self.crypto_changes {
            if change.record_key.is_empty() || change.record_key.len() > MAX_RECORD_KEY_BYTES {
                return Err(StorageError::InvalidInput(
                    "MLS record key length is outside the supported bounds".to_owned(),
                ));
            }
            if change.value.len() > MAX_RECORD_VALUE_BYTES {
                return Err(StorageError::InvalidInput(
                    "MLS record value exceeds the supported size".to_owned(),
                ));
            }
            total = total.saturating_add(change.value.len());
        }
        if total > MAX_CRYPTO_CHANGE_BYTES {
            return Err(StorageError::InvalidInput(
                "MLS record changes exceed the transaction byte bound".to_owned(),
            ));
        }
        Ok(())
    }
}

/// T2 结果：已提交的尝试投影。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptRecord {
    /// SubmissionId。
    pub submission_id: String,
    /// 关联草稿。
    pub draft_id: String,
    /// 会话 ID。
    pub conversation_id: String,
    /// MLS epoch。
    pub mls_epoch: u64,
    /// 不可变密文摘要。
    pub candidate_hash: [u8; 32],
    /// 不可变密文字节（重试只读这份字节，不重新加密）。
    pub immutable_ciphertext: Vec<u8>,
    /// 尝试状态。
    pub state: String,
    /// 本次调用是否命中已存在尝试。
    pub already_persisted: bool,
}

/// outbox 任务投影。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxJobRecord {
    /// 任务 ID。
    pub job_id: String,
    /// 去重键。
    pub dedup_key: String,
    /// 任务种类（prepare / send）。
    pub kind: String,
    /// 载荷引用。
    pub payload_ref: String,
    /// 状态。
    pub state: String,
    /// 下次可尝试时间。
    pub retry_at: i64,
    /// 租约到期时间。
    pub lease_until: Option<i64>,
    /// 租约版本。
    pub lease_version: u64,
}

/// 恢复标记投影。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryMarkerRecord {
    /// 标记主键。
    pub marker_id: i64,
    /// 阶段。
    pub stage: String,
    /// 关联草稿。
    pub draft_id: String,
    /// 关联尝试（草稿阶段为空）。
    pub submission_id: Option<String>,
    /// 标记摘要（草稿阶段为请求摘要，尝试阶段为候选摘要）。
    pub mark_hash: Vec<u8>,
    /// 创建时间。
    pub created_at: i64,
}

/// 隔离记录投影。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuarantineRecord {
    /// 主键。
    pub quarantine_id: i64,
    /// 类别。
    pub kind: String,
    /// 被隔离对象的引用。
    pub ref_id: String,
    /// 已解密的诊断（不含正文/密钥）。
    pub detail: String,
    /// 创建时间。
    pub created_at: i64,
}

/// 恢复结果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// 处理的标记数。
    pub markers_processed: u64,
    /// 清除的标记数。
    pub markers_cleared: u64,
    /// 重置的过期租约数。
    pub leases_reset: u64,
    /// 重建的准备任务数。
    pub prepare_jobs_requeued: u64,
    /// 重建的发送任务数。
    pub send_jobs_requeued: u64,
    /// 隔离的记录数。
    pub quarantined: u64,
    /// 是否仍有需要人工处理的未决项。
    pub recovery_required: bool,
    /// 是否发现无法证明一致的已提交数据。
    pub state_corrupted: bool,
}

impl RecoveryReport {
    fn summary(&self) -> String {
        format!(
            "markers processed={} cleared={} leases_reset={} prepare_requeued={} send_requeued={} quarantined={}",
            self.markers_processed,
            self.markers_cleared,
            self.leases_reset,
            self.prepare_jobs_requeued,
            self.send_jobs_requeued,
            self.quarantined
        )
    }
}

/// 故障注入点（用于崩溃/回滚测试；生产路径不传自定义回调）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteCheckpoint {
    /// T1：草稿行已写，准备任务未写。
    AfterDraft,
    /// T1：准备任务已写，恢复标记未写。
    AfterPrepareJob,
    /// T2：attempt 行已写，MLS 记录未写。
    AfterAttempt,
    /// T2：MLS 记录已写，发送任务未写。
    AfterMlsRecords,
    /// T2：发送任务已写，恢复标记未写。
    AfterOutbox,
}

/// 正式存储句柄：单写者、独占打开，持有主密钥与恢复状态。
pub struct Store {
    path: PathBuf,
    connection: Connection,
    key: ProtectedStorageKey,
    key_provenance: KeyProvenance,
    schema_version: u32,
    fail_closed: bool,
    fault_reason: Option<String>,
    recovery_pending: RecoveryPending,
}

impl fmt::Debug for Store {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Store")
            .field("path", &self.path)
            .field("schema_version", &self.schema_version)
            .field("status", &self.status())
            .field("recovery_pending", &self.recovery_pending)
            .finish_non_exhaustive()
    }
}

/// 打开正式加密存储：独占锁 -> PRAGMA -> 密钥校验 -> 迁移 -> 恢复标记检查。
///
/// 错误密钥在任何迁移/写入前被拒绝，绝不重建同名空库。
pub fn open_store(options: StoreOptions, key: ProtectedStorageKey) -> Result<Store> {
    options.key_provenance.validate()?;
    if let Some(parent) = options.path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.is_dir()
    {
        return Err(StorageError::InvalidInput(
            "storage directory does not exist".to_owned(),
        ));
    }

    let mut connection = Connection::open(&options.path)?;

    // 1. 独占迁移/写锁：等锁超时固定为 0，重复打开立即失败而不是排队。
    connection.busy_timeout(Duration::ZERO)?;
    let locking: String =
        connection.query_row("PRAGMA locking_mode = EXCLUSIVE", [], |row| row.get(0))?;
    if !locking.eq_ignore_ascii_case("exclusive") {
        return Err(StorageError::Failure(
            "cannot enable SQLite exclusive locking mode".to_owned(),
        ));
    }
    // 探测真实可写性；被其它连接持有时 SQLite 返回 busy/locked。
    connection.execute_batch("BEGIN IMMEDIATE; ROLLBACK;")?;
    connection.busy_timeout(options.busy_timeout)?;

    // 2. 耐久与约束 PRAGMA：WAL + FULL fsync + 外键。
    let journal: String = connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !journal.eq_ignore_ascii_case("wal") {
        return Err(StorageError::Failure(format!(
            "journal_mode WAL is unavailable (got {journal})"
        )));
    }
    connection.execute_batch("PRAGMA synchronous = FULL; PRAGMA foreign_keys = ON;")?;

    // 3. 已有库必须先通过密钥校验，才允许执行迁移或任何写操作。
    //    只有“表存在且校验记录行存在”才算已初始化；表存在但行缺失按未初始化处理。
    let had_key_check = migrations::table_exists(&connection, "store_key_check")?
        && connection.query_row(
            "SELECT COUNT(*) FROM store_key_check WHERE id = 1",
            [],
            |row| row.get::<_, i64>(0),
        )? == 1;
    if had_key_check {
        verify_stored_key(&connection, &key, &options.key_provenance)?;
    }
    let previously_applied = migrations::applied_migrations(&connection)?;

    // 4. 迁移到本程序支持的版本。
    let report = migrations::migrate(&mut connection, migrations::target_version())?;

    // 5. 新库写入密钥校验记录。已有 schema 且已写入受保护数据、却没有校验记录时拒绝，
    //    避免用新密钥静默接管别人的库；只有 schema 没有业务行（首次打开中途崩溃）才允许补写。
    if !had_key_check {
        if !previously_applied.is_empty() && has_protected_rows(&connection)? {
            return Err(StorageError::StateCorrupted(
                "existing store has no key verification record".to_owned(),
            ));
        }
        initialise_store(&mut connection, &key, &options.key_provenance)?;
    } else {
        let meta_rows: i64 =
            connection.query_row("SELECT COUNT(*) FROM store_meta WHERE id = 1", [], |row| {
                row.get(0)
            })?;
        if meta_rows != 1 {
            return Err(StorageError::StateCorrupted(
                "store_meta row is missing".to_owned(),
            ));
        }
    }

    // 6. 检查（不自动执行）未完成恢复标记与过期租约。
    let recovery_pending = compute_recovery_pending(&connection, now_ms())?;

    Ok(Store {
        path: options.path.clone(),
        connection,
        key_provenance: options.key_provenance,
        key,
        schema_version: report.to_version,
        fail_closed: false,
        fault_reason: None,
        recovery_pending,
    })
}

fn verify_stored_key(
    connection: &Connection,
    key: &ProtectedStorageKey,
    expected: &KeyProvenance,
) -> Result<()> {
    let row: Option<(u16, Vec<u8>, Vec<u8>)> = connection
        .query_row(
            "SELECT kdf_version, params, verifier FROM store_key_check WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((kdf_version, provenance_bytes, verifier)) = row else {
        return Err(StorageError::StateCorrupted(
            "store key verification record is missing".to_owned(),
        ));
    };
    let stored: KeyProvenance = serde_json::from_slice(&provenance_bytes).map_err(|_| {
        StorageError::StateCorrupted("stored key provenance is unreadable".to_owned())
    })?;
    stored
        .validate()
        .map_err(|_| StorageError::StateCorrupted("stored key provenance is invalid".to_owned()))?;
    if kdf_version != expected.kdf_version() || stored != *expected {
        return Err(StorageError::StateCorrupted(
            "storage key provenance mismatch".to_owned(),
        ));
    }
    let sealer = RecordSealer::new(key, SealPurpose::StoreVerifier);
    let opened = sealer.open(&[VERIFIER_CONTEXT, &kdf_version.to_be_bytes()], &verifier)?;
    if opened.as_slice() != VERIFIER_PLAINTEXT {
        return Err(StorageError::StateCorrupted(
            "storage key verifier mismatch".to_owned(),
        ));
    }
    Ok(())
}

fn initialise_store(
    connection: &mut Connection,
    key: &ProtectedStorageKey,
    provenance: &KeyProvenance,
) -> Result<()> {
    let kdf_version = provenance.kdf_version();
    let sealer = RecordSealer::new(key, SealPurpose::StoreVerifier);
    let verifier = sealer.seal(
        &[VERIFIER_CONTEXT, &kdf_version.to_be_bytes()],
        VERIFIER_PLAINTEXT,
    )?;
    let provenance_bytes = provenance.encode()?;
    let created_at = now_ms();
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute(
        "INSERT INTO store_key_check (id, kdf_version, params, verifier, created_at) VALUES (1, ?1, ?2, ?3, ?4)",
        params![kdf_version, provenance_bytes, verifier, created_at],
    )?;
    // 首次打开中途崩溃后重试时 store_meta 可能已存在；保留既有版本，不重置。
    transaction.execute(
        "INSERT INTO store_meta (id, store_version, created_at) VALUES (1, 0, ?1)
         ON CONFLICT(id) DO NOTHING",
        params![created_at],
    )?;
    transaction.commit()?;
    Ok(())
}

/// 是否已经写入任何受保护/业务行（用于判断能否安全补写密钥校验记录）。
fn has_protected_rows(connection: &Connection) -> Result<bool> {
    let count: i64 = connection.query_row(
        "SELECT (SELECT COUNT(*) FROM conversations)
              + (SELECT COUNT(*) FROM drafts)
              + (SELECT COUNT(*) FROM message_attempts)
              + (SELECT COUNT(*) FROM mls_state_records)
              + (SELECT COUNT(*) FROM messages)
              + (SELECT COUNT(*) FROM outbox_jobs)
              + (SELECT COUNT(*) FROM group_log_entries)
              + (SELECT COUNT(*) FROM inbox_dedup)",
        [],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn compute_recovery_pending(connection: &Connection, now: i64) -> Result<RecoveryPending> {
    let markers: i64 =
        connection.query_row("SELECT COUNT(*) FROM store_recovery_markers", [], |row| {
            row.get(0)
        })?;
    let expired_leases: i64 = connection.query_row(
        "SELECT COUNT(*) FROM outbox_jobs WHERE state = 'leased' AND lease_until IS NOT NULL AND lease_until <= ?1",
        [now],
        |row| row.get(0),
    )?;
    Ok(RecoveryPending {
        markers: u64::try_from(markers).unwrap_or(0),
        expired_leases: u64::try_from(expired_leases).unwrap_or(0),
    })
}

impl Store {
    /// 数据库文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 已应用的 schema 版本。
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 主密钥来源记录。
    pub fn key_provenance(&self) -> &KeyProvenance {
        &self.key_provenance
    }

    /// 连接状态；Faulted 时所有读写入口都显式失败。
    pub fn status(&self) -> StoreStatus {
        if self.fail_closed {
            StoreStatus::Faulted
        } else {
            StoreStatus::Ready
        }
    }

    /// 进入 fail-closed 的原因。
    pub fn fault_reason(&self) -> Option<&str> {
        self.fault_reason.as_deref()
    }

    /// 打开时检查到的未完成恢复项。
    pub fn recovery_pending(&self) -> RecoveryPending {
        self.recovery_pending
    }

    /// 主动把连接标记为故障（上层无法确认回滚语义时使用）。之后所有入口显式失败。
    pub fn mark_faulted(&mut self, reason: impl Into<String>) {
        self.fail_closed = true;
        self.fault_reason = Some(reason.into());
    }

    /// 只读连接访问：仅用于迁移自检、诊断与测试夹具；业务写入必须走 CoreTransaction。
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// 当前库版本（每次从库读取，不缓存）。
    pub fn store_version(&self) -> Result<u64> {
        self.ensure_ready()?;
        let version: u64 = self.connection.query_row(
            "SELECT store_version FROM store_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(version)
    }

    /// 重新统计未完成恢复项。
    pub fn refresh_recovery_pending(&mut self) -> Result<RecoveryPending> {
        self.ensure_ready()?;
        self.recovery_pending = compute_recovery_pending(&self.connection, now_ms())?;
        Ok(self.recovery_pending)
    }

    /// 开启受限写事务（BEGIN IMMEDIATE）。同一连接同时只能有一个事务。
    pub fn begin_transaction(&mut self, scope: WriteScope) -> Result<CoreTransaction<'_>> {
        self.ensure_ready()?;
        scope.validate()?;
        let Store {
            connection,
            key,
            fail_closed,
            ..
        } = self;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let store_version: u64 = match transaction.query_row(
            "SELECT store_version FROM store_meta WHERE id = 1",
            [],
            |row| row.get(0),
        ) {
            Ok(version) => version,
            Err(error) => {
                let _ = transaction.rollback();
                return Err(error.into());
            }
        };
        if let Some(expected) = scope.expected_version
            && expected != store_version
        {
            let _ = transaction.rollback();
            return Err(StorageError::VersionConflict {
                expected,
                found: store_version,
            });
        }
        Ok(CoreTransaction {
            transaction,
            key,
            fail_closed,
            scope,
            store_version,
            changes: 0,
            poisoned: false,
        })
    }

    /// 读取草稿并按主密钥解密正文；密文被篡改时返回 STATE_CORRUPTED。
    pub fn read_draft(&self, draft_id: &str) -> Result<Option<DraftRecord>> {
        self.ensure_ready()?;
        load_draft(&self.connection, &self.key, draft_id)
    }

    /// 读取尝试记录。
    pub fn read_attempt(&self, submission_id: &str) -> Result<Option<AttemptRecord>> {
        self.ensure_ready()?;
        load_attempt(&self.connection, submission_id)
    }

    /// 读取并解密一条 MLS provider 记录。
    pub fn read_mls_record(
        &self,
        conversation_id: &str,
        record_key: &[u8],
    ) -> Result<Option<Zeroizing<Vec<u8>>>> {
        self.ensure_ready()?;
        load_mls_record(&self.connection, &self.key, conversation_id, record_key)
    }

    /// 全部 outbox 任务（按 job_id 升序）。
    pub fn outbox_jobs(&self) -> Result<Vec<OutboxJobRecord>> {
        self.ensure_ready()?;
        load_outbox_jobs(&self.connection)
    }

    /// 未处理的恢复标记（最多返回 MAX_RECOVERY_MARKERS + 1 条，调用方据此判断是否超界）。
    pub fn recovery_markers(&self) -> Result<Vec<RecoveryMarkerRecord>> {
        self.ensure_ready()?;
        load_markers(&self.connection, MAX_RECOVERY_MARKERS as i64 + 1)
    }

    /// 已隔离记录。
    pub fn quarantine_records(&self) -> Result<Vec<QuarantineRecord>> {
        self.ensure_ready()?;
        load_quarantine(&self.connection, &self.key)
    }

    /// 等待准备的草稿（T1 已提交但 T2 尚未执行）。
    pub fn waiting_drafts(&self) -> Result<Vec<String>> {
        self.ensure_ready()?;
        let mut statement = self.connection.prepare(
            "SELECT d.draft_id FROM drafts d
             JOIN outbox_jobs j ON j.dedup_key = 'prepare:' || d.draft_id
             WHERE d.state = 'queued' AND j.state IN ('pending','leased')
             ORDER BY d.draft_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut drafts = Vec::new();
        for row in rows {
            drafts.push(row?);
        }
        Ok(drafts)
    }

    fn ensure_ready(&self) -> Result<()> {
        if self.fail_closed {
            return Err(StorageError::Failure(
                self.fault_reason
                    .clone()
                    .unwrap_or_else(|| "store is fail-closed".to_owned()),
            ));
        }
        Ok(())
    }
}

/// 受限写事务句柄。只能通过 commit_transaction 或 rollback_transaction 结束；
/// Drop 未提交时由 SQLite 回滚。出错后句柄被标记 poisoned，拒绝提交。
pub struct CoreTransaction<'a> {
    transaction: Transaction<'a>,
    key: &'a ProtectedStorageKey,
    fail_closed: &'a mut bool,
    scope: WriteScope,
    store_version: u64,
    changes: u64,
    poisoned: bool,
}

impl fmt::Debug for CoreTransaction<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CoreTransaction")
            .field("scope", &self.scope)
            .field("store_version", &self.store_version)
            .field("changes", &self.changes)
            .field("poisoned", &self.poisoned)
            .finish()
    }
}

impl<'a> CoreTransaction<'a> {
    /// 事务作用域。
    pub fn scope(&self) -> &WriteScope {
        &self.scope
    }

    /// 事务开始时的库版本。
    pub fn store_version(&self) -> u64 {
        self.store_version
    }

    /// 本次事务计数到的写入行数。
    pub fn changes(&self) -> u64 {
        self.changes
    }

    /// 是否已因写入失败被标记 poisoned。
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// 只读连接访问（诊断/测试夹具）。
    pub fn connection(&self) -> &Connection {
        &self.transaction
    }

    /// 读取本事务刚写入（未提交）的 MLS 记录；用于验证同事务读一致。
    pub fn read_mls_record(
        &self,
        conversation_id: &str,
        record_key: &[u8],
    ) -> Result<Option<Zeroizing<Vec<u8>>>> {
        load_mls_record(&self.transaction, self.key, conversation_id, record_key)
    }

    fn note(&mut self, rows: usize) {
        self.changes = self.changes.saturating_add(rows as u64);
    }

    fn poison(&mut self) {
        self.poisoned = true;
    }
}

fn sealer_for(transaction: &CoreTransaction<'_>, purpose: SealPurpose) -> RecordSealer {
    RecordSealer::new(transaction.key, purpose)
}

/// 耐久提交：最终版本比较 -> 外键检查 -> COMMIT。成功才返回 CommitToken。
pub fn commit_transaction(transaction: CoreTransaction<'_>) -> Result<CommitToken> {
    let CoreTransaction {
        transaction,
        fail_closed,
        store_version,
        changes,
        poisoned,
        ..
    } = transaction;
    if poisoned {
        if transaction.rollback().is_err() {
            *fail_closed = true;
        }
        return Err(StorageError::Failure(
            "transaction was poisoned by a failed write; refusing to commit".to_owned(),
        ));
    }
    let prepared = commit_inner(&transaction, store_version, changes);
    match prepared {
        Ok(token) => match transaction.commit() {
            Ok(()) => Ok(token),
            Err(error) => {
                *fail_closed = true;
                Err(error.into())
            }
        },
        Err(error) => match transaction.rollback() {
            Ok(()) => Err(error),
            Err(rollback) => {
                *fail_closed = true;
                Err(StorageError::RollbackFailed(rollback.to_string()))
            }
        },
    }
}

fn commit_inner(
    transaction: &Transaction<'_>,
    expected_version: u64,
    changes: u64,
) -> Result<CommitToken> {
    let current: u64 = transaction.query_row(
        "SELECT store_version FROM store_meta WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    if current != expected_version {
        return Err(StorageError::VersionConflict {
            expected: expected_version,
            found: current,
        });
    }
    {
        let mut statement = transaction.prepare("PRAGMA foreign_key_check")?;
        let mut rows = statement.query([])?;
        if rows.next()?.is_some() {
            return Err(StorageError::StateCorrupted(
                "foreign key violation detected before commit".to_owned(),
            ));
        }
    }
    let updated = transaction.execute(
        "UPDATE store_meta SET store_version = store_version + 1 WHERE id = 1",
        [],
    )?;
    if updated != 1 {
        return Err(StorageError::StateCorrupted(
            "store_meta row is missing".to_owned(),
        ));
    }
    Ok(CommitToken {
        committed_version: expected_version.saturating_add(1),
        changed_rows: changes,
    })
}

/// 显式回滚。回滚本身失败时把连接标记为 fail-closed 并返回 ROLLBACK_FAILED。
pub fn rollback_transaction(transaction: CoreTransaction<'_>) -> Result<()> {
    let CoreTransaction {
        transaction,
        fail_closed,
        ..
    } = transaction;
    match transaction.rollback() {
        Ok(()) => Ok(()),
        Err(error) => {
            *fail_closed = true;
            Err(StorageError::RollbackFailed(error.to_string()))
        }
    }
}

/// STORAGE-006：以 operation ID 建立唯一草稿与初始排队任务（T1）。
pub fn persist_send_intent(
    transaction: &mut CoreTransaction<'_>,
    request: ValidatedSendRequest,
) -> Result<DraftRecord> {
    persist_send_intent_with_checkpoint(transaction, request, &mut |_| Ok(()))
}

/// 带故障注入点的 persist_send_intent：checkpoint 返回错误即整体回滚。
pub fn persist_send_intent_with_checkpoint(
    transaction: &mut CoreTransaction<'_>,
    request: ValidatedSendRequest,
    checkpoint: &mut dyn FnMut(WriteCheckpoint) -> Result<()>,
) -> Result<DraftRecord> {
    let outcome = write_send_intent(transaction, &request, checkpoint);
    if outcome.is_err() {
        transaction.poison();
    }
    outcome
}

fn write_send_intent(
    transaction: &mut CoreTransaction<'_>,
    request: &ValidatedSendRequest,
    checkpoint: &mut dyn FnMut(WriteCheckpoint) -> Result<()>,
) -> Result<DraftRecord> {
    request.validate()?;
    if transaction.scope.kind != WriteScopeKind::Intent {
        return Err(StorageError::InvalidInput(
            "persist_send_intent requires an intent write scope".to_owned(),
        ));
    }
    if let Some(scope_conversation) = transaction.scope.conversation_id.as_deref()
        && scope_conversation != request.conversation_id
    {
        return Err(StorageError::InvalidInput(
            "write scope covers a different conversation".to_owned(),
        ));
    }
    ensure_conversation(&transaction.transaction, &request.conversation_id)?;

    let request_hash = request.request_digest();
    let draft_id = draft_id_for(&request.conversation_id, &request.client_operation_id);

    // 步骤 1-3：唯一索引查询、同请求返回旧草稿、不同摘要同 ID 拒绝。
    if let Some(existing) = load_draft(&transaction.transaction, transaction.key, &draft_id)? {
        let same_request = existing.conversation_id == request.conversation_id
            && existing.operation_id == request.client_operation_id
            && existing.request_hash == request_hash
            && existing.content == request.content;
        if !same_request {
            return Err(StorageError::OperationIdConflict(format!(
                "operation {} maps to a different draft request",
                request.client_operation_id
            )));
        }
        if prepare_job(&transaction.transaction, &draft_id)?.is_none() {
            return Err(StorageError::RecoveryRequired(format!(
                "draft {draft_id} lost its prepare job"
            )));
        }
        let mut record = existing;
        record.already_existed = true;
        return Ok(record);
    }

    // 步骤 4：保存加密正文。
    let sealed = sealer_for(transaction, SealPurpose::DraftContent).seal(
        &[
            request.conversation_id.as_bytes(),
            draft_id.as_bytes(),
            request.client_operation_id.as_bytes(),
        ],
        request.content.canonical_bytes(),
    )?;
    let inserted = transaction.transaction.execute(
        "INSERT INTO drafts (draft_id, conversation_id, operation_id, request_hash, protected_content, state, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6)",
        params![
            draft_id,
            request.conversation_id,
            request.client_operation_id,
            request_hash.as_slice(),
            sealed,
            request.created_at
        ],
    );
    match inserted {
        Ok(rows) => transaction.note(rows),
        Err(error) => return Err(conflict_or_failure(error)),
    }
    checkpoint(WriteCheckpoint::AfterDraft)?;

    // 步骤 5：插入待准备任务（与草稿同事务）。
    let job_id = prepare_job_id_for(&draft_id);
    transaction.transaction.execute(
        "INSERT INTO outbox_jobs (job_id, dedup_key, kind, payload_ref, conversation_id, depends_on, state, retry_count, retry_at, lease_owner, lease_until, lease_version, cancel_requested)
         VALUES (?1, ?2, 'prepare', ?3, ?4, NULL, 'pending', 0, ?5, NULL, NULL, 0, 0)",
        params![
            job_id,
            format!("prepare:{draft_id}"),
            draft_id,
            request.conversation_id,
            request.created_at
        ],
    )?;
    checkpoint(WriteCheckpoint::AfterPrepareJob)?;

    // 恢复标记只记录已写入什么，恢复流程核对后清除，不会据此重建任何数据。
    transaction.transaction.execute(
        "INSERT OR IGNORE INTO store_recovery_markers (stage, draft_id, submission_id, mark_hash, created_at)
         VALUES ('draft_prepared', ?1, NULL, ?2, ?3)",
        params![draft_id, request_hash.as_slice(), request.created_at],
    )?;
    transaction.note(2);

    Ok(DraftRecord {
        draft_id,
        conversation_id: request.conversation_id.clone(),
        operation_id: request.client_operation_id.clone(),
        request_hash,
        content: request.content.clone(),
        state: "queued".to_owned(),
        created_at: request.created_at,
        already_existed: false,
    })
}

/// STORAGE-007：保存不可变密文、摘要、SubmissionId 与草稿关联（T2）。
pub fn persist_prepared_attempt(
    transaction: &mut CoreTransaction<'_>,
    prepared: PreparedWrite,
) -> Result<AttemptRecord> {
    persist_prepared_attempt_with_checkpoint(transaction, prepared, &mut |_| Ok(()))
}

/// 带故障注入点的 persist_prepared_attempt：checkpoint 返回错误即整体回滚。
pub fn persist_prepared_attempt_with_checkpoint(
    transaction: &mut CoreTransaction<'_>,
    prepared: PreparedWrite,
    checkpoint: &mut dyn FnMut(WriteCheckpoint) -> Result<()>,
) -> Result<AttemptRecord> {
    let outcome = write_prepared_attempt(transaction, &prepared, checkpoint);
    if outcome.is_err() {
        transaction.poison();
    }
    outcome
}

fn write_prepared_attempt(
    transaction: &mut CoreTransaction<'_>,
    prepared: &PreparedWrite,
    checkpoint: &mut dyn FnMut(WriteCheckpoint) -> Result<()>,
) -> Result<AttemptRecord> {
    prepared.validate()?;
    if transaction.scope.kind != WriteScopeKind::Attempt {
        return Err(StorageError::InvalidInput(
            "persist_prepared_attempt requires an attempt write scope".to_owned(),
        ));
    }
    if let Some(scope_conversation) = transaction.scope.conversation_id.as_deref()
        && scope_conversation != prepared.conversation_id
    {
        return Err(StorageError::InvalidInput(
            "write scope covers a different conversation".to_owned(),
        ));
    }

    // 步骤 1：核对草稿与 epoch 上下文。
    let draft = load_draft(&transaction.transaction, transaction.key, &prepared.draft_id)?
        .ok_or_else(|| {
            StorageError::SubmissionIdConflict(format!(
                "draft {} does not exist",
                prepared.draft_id
            ))
        })?;
    if draft.conversation_id != prepared.conversation_id {
        return Err(StorageError::InvalidInput(
            "attempt conversation does not match the persisted draft".to_owned(),
        ));
    }
    if draft.request_hash != prepared.request_hash {
        return Err(StorageError::InvalidInput(
            "attempt request digest does not match the persisted draft".to_owned(),
        ));
    }
    let candidate = candidate_hash(
        &prepared.conversation_id,
        &prepared.submission_id,
        prepared.mls_epoch,
        &prepared.immutable_ciphertext,
    );

    // 步骤 3：SubmissionId 唯一性与幂等重放。
    if let Some(existing) = load_attempt(&transaction.transaction, &prepared.submission_id)? {
        let same_attempt = existing.draft_id == prepared.draft_id
            && existing.conversation_id == prepared.conversation_id
            && existing.candidate_hash == candidate;
        if !same_attempt {
            return Err(StorageError::SubmissionIdConflict(format!(
                "submission {} is bound to a different attempt",
                prepared.submission_id
            )));
        }
        if send_job(&transaction.transaction, &prepared.submission_id)?.is_none() {
            return Err(StorageError::RecoveryRequired(format!(
                "attempt {} lost its send job",
                prepared.submission_id
            )));
        }
        let mut record = existing;
        record.already_persisted = true;
        return Ok(record);
    }
    if let Some(active) = active_attempt_for_draft(&transaction.transaction, &prepared.draft_id)? {
        return Err(StorageError::SubmissionIdConflict(format!(
            "draft {} already has attempt {} in state {}",
            prepared.draft_id, active.0, active.1
        )));
    }
    if prepare_job(&transaction.transaction, &prepared.draft_id)?.is_none() {
        return Err(StorageError::RecoveryRequired(format!(
            "draft {} lost its prepare job before preparing an attempt",
            prepared.draft_id
        )));
    }

    // 步骤 2：写入不可变尝试。
    transaction.transaction.execute(
        "INSERT INTO message_attempts (submission_id, origin, draft_id, operation_id, conversation_id, mls_epoch, immutable_ciphertext, candidate_hash, state)
         VALUES (?1, 'local_application', ?2, NULL, ?3, ?4, ?5, ?6, 'prepared')",
        params![
            prepared.submission_id,
            prepared.draft_id,
            prepared.conversation_id,
            prepared.mls_epoch,
            prepared.immutable_ciphertext,
            candidate.as_slice()
        ],
    )?;
    checkpoint(WriteCheckpoint::AfterAttempt)?;

    // 步骤 4：写 crypto 变化（同事务内加密写入，绝不跨事务）。
    write_crypto_changes(transaction, prepared)?;
    checkpoint(WriteCheckpoint::AfterMlsRecords)?;

    // 步骤 5：排入发送 outbox，并把准备任务标记完成。
    let job_id = send_job_id_for(&prepared.submission_id);
    transaction.transaction.execute(
        "INSERT INTO outbox_jobs (job_id, dedup_key, kind, payload_ref, conversation_id, depends_on, state, retry_count, retry_at, lease_owner, lease_until, lease_version, cancel_requested)
         VALUES (?1, ?2, 'send', ?3, ?4, ?5, 'pending', 0, ?6, NULL, NULL, 0, 0)",
        params![
            job_id,
            format!("send:{}", prepared.submission_id),
            prepared.submission_id,
            prepared.conversation_id,
            prepare_job_id_for(&prepared.draft_id),
            draft.created_at
        ],
    )?;
    transaction.transaction.execute(
        "UPDATE outbox_jobs SET state = 'complete' WHERE dedup_key = ?1",
        params![format!("prepare:{}", prepared.draft_id)],
    )?;
    checkpoint(WriteCheckpoint::AfterOutbox)?;

    transaction.transaction.execute(
        "INSERT OR IGNORE INTO store_recovery_markers (stage, draft_id, submission_id, mark_hash, created_at)
         VALUES ('attempt_prepared', ?1, ?2, ?3, ?4)",
        params![
            prepared.draft_id,
            prepared.submission_id,
            candidate.as_slice(),
            draft.created_at
        ],
    )?;
    transaction.note(3 + prepared.crypto_changes.len());

    Ok(AttemptRecord {
        submission_id: prepared.submission_id.clone(),
        draft_id: prepared.draft_id.clone(),
        conversation_id: prepared.conversation_id.clone(),
        mls_epoch: prepared.mls_epoch,
        candidate_hash: candidate,
        immutable_ciphertext: prepared.immutable_ciphertext.clone(),
        state: "prepared".to_owned(),
        already_persisted: false,
    })
}

fn write_crypto_changes(
    transaction: &mut CoreTransaction<'_>,
    prepared: &PreparedWrite,
) -> Result<()> {
    let sealer = sealer_for(transaction, SealPurpose::MlsStateRecord);
    for change in &prepared.crypto_changes {
        let version_bytes = change.record_version.to_be_bytes();
        let sealed = sealer.seal(
            &[
                prepared.conversation_id.as_bytes(),
                change.record_key.as_slice(),
                &version_bytes,
            ],
            change.value.as_slice(),
        )?;
        let affected = transaction.transaction.execute(
            "INSERT INTO mls_state_records (conversation_id, record_key, protected_value, record_version)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(conversation_id, record_key) DO UPDATE SET
                 protected_value = excluded.protected_value,
                 record_version = excluded.record_version
             WHERE excluded.record_version = mls_state_records.record_version + 1",
            params![
                prepared.conversation_id,
                change.record_key,
                sealed,
                change.record_version
            ],
        )?;
        if affected != 1 {
            let current: Option<u64> = transaction
                .transaction
                .query_row(
                    "SELECT record_version FROM mls_state_records WHERE conversation_id = ?1 AND record_key = ?2",
                    params![prepared.conversation_id, change.record_key],
                    |row| row.get(0),
                )
                .optional()?;
            return Err(StorageError::VersionConflict {
                expected: change.record_version,
                found: current.unwrap_or(0),
            });
        }
    }
    Ok(())
}

/// STORAGE-011：按恢复标记核对已提交状态，重置过期租约，隔离无法证明一致的记录。
///
/// 不重建任何密文：只核对既有字节、重建可推导的 outbox 任务，并把无法证明一致的数据
/// 写入 store_quarantine，同时把对应 attempt 置为 unknown。
pub fn recover_incomplete_writes(store: &mut Store) -> Result<RecoveryReport> {
    store.ensure_ready()?;
    let mut transaction = store.begin_transaction(WriteScope::recovery())?;
    let outcome = recover_inner(&mut transaction);
    match outcome {
        Ok(report) => {
            commit_transaction(transaction)?;
            store.refresh_recovery_pending()?;
            if report.state_corrupted {
                return Err(StorageError::StateCorrupted(format!(
                    "recovery quarantined {} record(s)",
                    report.quarantined
                )));
            }
            if report.recovery_required {
                return Err(StorageError::RecoveryRequired(report.summary()));
            }
            Ok(report)
        }
        Err(error) => {
            let _ = rollback_transaction(transaction);
            Err(error)
        }
    }
}

fn recover_inner(transaction: &mut CoreTransaction<'_>) -> Result<RecoveryReport> {
    let mut report = RecoveryReport::default();

    // 步骤 4：重置过期任务租约（只处理已过期，不猜测即将到期）。
    let reset = transaction.transaction.execute(
        "UPDATE outbox_jobs SET state = 'pending', lease_owner = NULL, lease_until = NULL
         WHERE state = 'leased' AND lease_until IS NOT NULL AND lease_until <= ?1",
        [now_ms()],
    )?;
    report.leases_reset = reset as u64;

    // 步骤 1：读取恢复记录（有界：只多读一条用于判断超界）。
    let markers = load_markers(&transaction.transaction, MAX_RECOVERY_MARKERS as i64 + 1)?;
    if markers.len() > MAX_RECOVERY_MARKERS {
        return Err(StorageError::RecoveryRequired(format!(
            "{} recovery markers exceed the bounded limit",
            markers.len()
        )));
    }

    for marker in markers {
        report.markers_processed += 1;
        match marker.stage.as_str() {
            "draft_prepared" => {
                let draft = load_draft(&transaction.transaction, transaction.key, &marker.draft_id)?;
                if let Some(draft) = draft {
                    if draft.request_hash.as_slice() != marker.mark_hash.as_slice() {
                        quarantine(
                            transaction,
                            "draft_request_hash_mismatch",
                            &marker.draft_id,
                            "recovery marker digest differs from the committed draft",
                            &mut report,
                        )?;
                        report.state_corrupted = true;
                    } else if prepare_job(&transaction.transaction, &marker.draft_id)?.is_none() {
                        let job_id = prepare_job_id_for(&marker.draft_id);
                        transaction.transaction.execute(
                            "INSERT INTO outbox_jobs (job_id, dedup_key, kind, payload_ref, conversation_id, depends_on, state, retry_count, retry_at, lease_owner, lease_until, lease_version, cancel_requested)
                             VALUES (?1, ?2, 'prepare', ?3, ?4, NULL, 'pending', 0, ?5, NULL, NULL, 0, 0)",
                            params![
                                job_id,
                                format!("prepare:{}", marker.draft_id),
                                marker.draft_id,
                                draft.conversation_id,
                                draft.created_at
                            ],
                        )?;
                        report.prepare_jobs_requeued += 1;
                    }
                }
            }
            "attempt_prepared" => {
                let Some(submission_id) = marker.submission_id.clone() else {
                    return Err(StorageError::StateCorrupted(
                        "attempt recovery marker has no submission ID".to_owned(),
                    ));
                };
                let attempt = load_attempt(&transaction.transaction, &submission_id)?;
                if let Some(attempt) = attempt {
                    let expected = candidate_hash(
                        &attempt.conversation_id,
                        &submission_id,
                        attempt.mls_epoch,
                        &attempt.immutable_ciphertext,
                    );
                    if expected != attempt.candidate_hash {
                        // 已提交密文被改动：无法证明一致，隔离并置为 unknown，绝不重造。
                        transaction.transaction.execute(
                            "UPDATE message_attempts SET state = 'unknown' WHERE submission_id = ?1",
                            [&submission_id],
                        )?;
                        quarantine(
                            transaction,
                            "attempt_digest_mismatch",
                            &submission_id,
                            "immutable ciphertext digest does not match the committed attempt",
                            &mut report,
                        )?;
                        report.state_corrupted = true;
                    } else if send_job(&transaction.transaction, &submission_id)?.is_none() {
                        let job_id = send_job_id_for(&submission_id);
                        transaction.transaction.execute(
                            "INSERT INTO outbox_jobs (job_id, dedup_key, kind, payload_ref, conversation_id, depends_on, state, retry_count, retry_at, lease_owner, lease_until, lease_version, cancel_requested)
                             VALUES (?1, ?2, 'send', ?3, ?4, ?5, 'pending', 0, ?6, NULL, NULL, 0, 0)",
                            params![
                                job_id,
                                format!("send:{submission_id}"),
                                submission_id,
                                attempt.conversation_id,
                                prepare_job_id_for(&attempt.draft_id),
                                now_ms()
                            ],
                        )?;
                        report.send_jobs_requeued += 1;
                    }
                }
            }
            other => {
                return Err(StorageError::StateCorrupted(format!(
                    "unknown recovery marker stage {other}"
                )));
            }
        }
        // 步骤 2/3/5/6：标记核对结束（无论是否已提交）后清除，绝不保留可用半成品。
        transaction.transaction.execute(
            "DELETE FROM store_recovery_markers WHERE marker_id = ?1",
            [marker.marker_id],
        )?;
        report.markers_cleared += 1;
    }
    Ok(report)
}

fn quarantine(
    transaction: &mut CoreTransaction<'_>,
    kind: &str,
    ref_id: &str,
    detail: &str,
    report: &mut RecoveryReport,
) -> Result<()> {
    let sealed = sealer_for(transaction, SealPurpose::QuarantineDetail).seal(
        &[kind.as_bytes(), ref_id.as_bytes()],
        detail.as_bytes(),
    )?;
    let inserted = transaction.transaction.execute(
        "INSERT OR IGNORE INTO store_quarantine (kind, ref_id, protected_detail, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![kind, ref_id, sealed, now_ms()],
    )?;
    report.quarantined += inserted as u64;
    Ok(())
}

fn conflict_or_failure(error: rusqlite::Error) -> StorageError {
    if matches!(
        &error,
        rusqlite::Error::SqliteFailure(code, _) if code.code == rusqlite::ErrorCode::ConstraintViolation
    ) {
        return StorageError::OperationIdConflict(
            "operation ID is already bound to another draft".to_owned(),
        );
    }
    error.into()
}

fn ensure_conversation(connection: &Connection, conversation_id: &str) -> Result<()> {
    let found: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM conversations WHERE conversation_id = ?1",
            [conversation_id],
            |row| row.get(0),
        )
        .optional()?;
    if found.is_none() {
        return Err(StorageError::InvalidInput(format!(
            "conversation {conversation_id} does not exist"
        )));
    }
    Ok(())
}

fn load_draft(
    connection: &Connection,
    key: &ProtectedStorageKey,
    draft_id: &str,
) -> Result<Option<DraftRecord>> {
    let row: Option<(String, String, Vec<u8>, Vec<u8>, String, i64)> = connection
        .query_row(
            "SELECT conversation_id, operation_id, request_hash, protected_content, state, created_at
             FROM drafts WHERE draft_id = ?1",
            [draft_id],
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
    let Some((conversation_id, operation_id, request_hash, protected, state, created_at)) = row
    else {
        return Ok(None);
    };
    let sealer = RecordSealer::new(key, SealPurpose::DraftContent);
    let content = sealer.open(
        &[
            conversation_id.as_bytes(),
            draft_id.as_bytes(),
            operation_id.as_bytes(),
        ],
        &protected,
    )?;
    Ok(Some(DraftRecord {
        draft_id: draft_id.to_owned(),
        conversation_id,
        operation_id,
        request_hash: digest_bytes(request_hash)?,
        content: MessageContent::from_canonical(content.as_slice())?,
        state,
        created_at,
        already_existed: false,
    }))
}

fn load_attempt(connection: &Connection, submission_id: &str) -> Result<Option<AttemptRecord>> {
    let row: Option<(String, String, u64, Vec<u8>, Vec<u8>, String)> = connection
        .query_row(
            "SELECT draft_id, conversation_id, mls_epoch, immutable_ciphertext, candidate_hash, state
             FROM message_attempts WHERE submission_id = ?1",
            [submission_id],
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
    let Some((draft_id, conversation_id, mls_epoch, ciphertext, candidate, state)) = row else {
        return Ok(None);
    };
    Ok(Some(AttemptRecord {
        submission_id: submission_id.to_owned(),
        draft_id,
        conversation_id,
        mls_epoch,
        candidate_hash: digest_bytes(candidate)?,
        immutable_ciphertext: ciphertext,
        state,
        already_persisted: false,
    }))
}

fn load_mls_record(
    connection: &Connection,
    key: &ProtectedStorageKey,
    conversation_id: &str,
    record_key: &[u8],
) -> Result<Option<Zeroizing<Vec<u8>>>> {
    let row: Option<(u64, Vec<u8>)> = connection
        .query_row(
            "SELECT record_version, protected_value FROM mls_state_records
             WHERE conversation_id = ?1 AND record_key = ?2",
            params![conversation_id, record_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((record_version, protected)) = row else {
        return Ok(None);
    };
    let sealer = RecordSealer::new(key, SealPurpose::MlsStateRecord);
    let value = sealer.open(
        &[
            conversation_id.as_bytes(),
            record_key,
            &record_version.to_be_bytes(),
        ],
        &protected,
    )?;
    Ok(Some(value))
}

fn load_outbox_jobs(connection: &Connection) -> Result<Vec<OutboxJobRecord>> {
    let mut statement = connection.prepare(
        "SELECT job_id, dedup_key, kind, payload_ref, state, retry_at, lease_until, lease_version
         FROM outbox_jobs ORDER BY job_id ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(OutboxJobRecord {
            job_id: row.get(0)?,
            dedup_key: row.get(1)?,
            kind: row.get(2)?,
            payload_ref: row.get(3)?,
            state: row.get(4)?,
            retry_at: row.get(5)?,
            lease_until: row.get(6)?,
            lease_version: row.get(7)?,
        })
    })?;
    let mut jobs = Vec::new();
    for row in rows {
        jobs.push(row?);
    }
    Ok(jobs)
}

fn job_by_dedup(connection: &Connection, dedup_key: &str) -> Result<Option<OutboxJobRecord>> {
    let row: Option<(String, String, String, String, String, i64, Option<i64>, u64)> = connection
        .query_row(
            "SELECT job_id, dedup_key, kind, payload_ref, state, retry_at, lease_until, lease_version
             FROM outbox_jobs WHERE dedup_key = ?1",
            [dedup_key],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .optional()?;
    Ok(row.map(
        |(job_id, dedup_key, kind, payload_ref, state, retry_at, lease_until, lease_version)| {
            OutboxJobRecord {
                job_id,
                dedup_key,
                kind,
                payload_ref,
                state,
                retry_at,
                lease_until,
                lease_version,
            }
        },
    ))
}

fn prepare_job(connection: &Connection, draft_id: &str) -> Result<Option<OutboxJobRecord>> {
    job_by_dedup(connection, &format!("prepare:{draft_id}"))
}

fn send_job(connection: &Connection, submission_id: &str) -> Result<Option<OutboxJobRecord>> {
    job_by_dedup(connection, &format!("send:{submission_id}"))
}

fn active_attempt_for_draft(
    connection: &Connection,
    draft_id: &str,
) -> Result<Option<(String, String)>> {
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT submission_id, state FROM message_attempts
             WHERE draft_id = ?1 AND state IN ('prepared','submitted')
             ORDER BY submission_id ASC LIMIT 1",
            [draft_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(row)
}

fn load_markers(connection: &Connection, limit: i64) -> Result<Vec<RecoveryMarkerRecord>> {
    let mut statement = connection.prepare(
        "SELECT marker_id, stage, draft_id, submission_id, mark_hash, created_at
         FROM store_recovery_markers ORDER BY marker_id ASC LIMIT ?1",
    )?;
    let rows = statement.query_map([limit], |row| {
        Ok(RecoveryMarkerRecord {
            marker_id: row.get(0)?,
            stage: row.get(1)?,
            draft_id: row.get(2)?,
            submission_id: row.get(3)?,
            mark_hash: row.get(4)?,
            created_at: row.get(5)?,
        })
    })?;
    let mut markers = Vec::new();
    for row in rows {
        markers.push(row?);
    }
    Ok(markers)
}

fn load_quarantine(
    connection: &Connection,
    key: &ProtectedStorageKey,
) -> Result<Vec<QuarantineRecord>> {
    let mut statement = connection.prepare(
        "SELECT quarantine_id, kind, ref_id, protected_detail, created_at
         FROM store_quarantine ORDER BY quarantine_id ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Vec<u8>>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    let sealer = RecordSealer::new(key, SealPurpose::QuarantineDetail);
    let mut records = Vec::new();
    for row in rows {
        let (quarantine_id, kind, ref_id, protected, created_at) = row?;
        let detail = sealer.open(&[kind.as_bytes(), ref_id.as_bytes()], &protected)?;
        records.push(QuarantineRecord {
            quarantine_id,
            kind,
            ref_id,
            detail: String::from_utf8_lossy(detail.as_slice()).into_owned(),
            created_at,
        });
    }
    Ok(records)
}
