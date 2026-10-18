//! 正式存储路径的稳定错误码。
//!
//! 每个变体对应文档中的稳定错误码（见 `docs/implementation/storage.md` 的失败处理）。
//! 安全校验失败必须显式返回错误，绝不降级继续写；错误消息只包含表名/约束名与 ID，
//! 不包含消息正文或密钥字节。

use rusqlite::ErrorCode;

/// 存储层错误。`code()` 返回文档约定的稳定错误码字符串。
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// 另一个写者（其它进程或已故障连接）持有该库的独占写锁。
    #[error("storage is locked: {0}")]
    Locked(String),
    /// 密钥校验、密文认证或库结构校验失败。不区分“错误密钥”和“被篡改的密文”。
    #[error("state corrupted: {0}")]
    StateCorrupted(String),
    /// 迁移脚本执行失败或校验和不匹配。
    #[error("migration failed: {0}")]
    MigrationFailed(String),
    /// 磁盘上的 schema 版本比本程序支持的更新，禁止降级。
    #[error("schema too new: found {found}, supported {supported}")]
    SchemaTooNew { found: u32, supported: u32 },
    /// 同一 operation ID 已绑定到不同的请求摘要。
    #[error("operation ID conflict: {0}")]
    OperationIdConflict(String),
    /// 同一 SubmissionId 已绑定到不同的密文摘要或不同的活跃尝试。
    #[error("submission ID conflict: {0}")]
    SubmissionIdConflict(String),
    /// 期望版本与库内版本不一致。
    #[error("version conflict: expected {expected}, found {found}")]
    VersionConflict { expected: u64, found: u64 },
    /// 已提交状态无法证明一致，调用方必须暂停该分支并显式恢复。
    #[error("recovery required: {0}")]
    RecoveryRequired(String),
    /// 回滚失败，连接进入 fail-closed。
    #[error("rollback failed: {0}")]
    RollbackFailed(String),
    /// 磁盘空间不足。
    #[error("disk full")]
    DiskFull,
    /// SQLite 或底层 I/O 失败。
    #[error("storage failure: {0}")]
    Failure(String),
    /// 调用方输入不合法（ID 格式、长度上限、作用域不匹配等）。
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// 分页游标格式/认证/范围/过期校验失败；绝不退化为“跳到最新页”。
    #[error("invalid cursor: {0}")]
    InvalidCursor(String),
    /// 会话不可见（不存在或已离开/解散），查询与写入都必须显式拒绝。
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// 另一个写者短暂持有写锁，可重试；与永久故障区分。
    #[error("storage busy: {0}")]
    Busy(String),
    /// 租约版本/持有者不匹配：旧 worker 不能更新新 lease。
    #[error("job lease conflict: {0}")]
    JobLeaseConflict(String),
    /// 目标状态与当前状态冲突（例如把终态结果改成另一种结果）。
    #[error("invalid state transition: {0}")]
    InvalidStateTransition(String),
}

impl StorageError {
    /// 稳定错误码字符串（FFI/UI 只依赖这些常量，不解析错误消息）。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Locked(_) => "STORAGE_LOCKED",
            Self::StateCorrupted(_) => "STATE_CORRUPTED",
            Self::MigrationFailed(_) => "MIGRATION_FAILED",
            Self::SchemaTooNew { .. } => "SCHEMA_TOO_NEW",
            Self::OperationIdConflict(_) => "OPERATION_ID_CONFLICT",
            Self::SubmissionIdConflict(_) => "SUBMISSION_ID_CONFLICT",
            Self::VersionConflict { .. } => "VERSION_CONFLICT",
            Self::RecoveryRequired(_) => "RECOVERY_REQUIRED",
            Self::RollbackFailed(_) => "ROLLBACK_FAILED",
            Self::DiskFull => "DISK_FULL",
            Self::Failure(_) => "STORAGE_FAILURE",
            Self::InvalidInput(_) => "INVALID_INPUT",
            Self::InvalidCursor(_) => "INVALID_CURSOR",
            Self::PermissionDenied(_) => "PERMISSION_DENIED",
            Self::Busy(_) => "STORAGE_BUSY",
            Self::JobLeaseConflict(_) => "JOB_LEASE_CONFLICT",
            Self::InvalidStateTransition(_) => "INVALID_STATE_TRANSITION",
        }
    }
}

impl From<rusqlite::Error> for StorageError {
    fn from(error: rusqlite::Error) -> Self {
        match &error {
            rusqlite::Error::SqliteFailure(code, message) => match code.code {
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => {
                    Self::Locked("another writer holds the store".to_owned())
                }
                ErrorCode::DiskFull => Self::DiskFull,
                _ => Self::Failure(describe(message.clone().unwrap_or_default())),
            },
            other => Self::Failure(describe(other.to_string())),
        }
    }
}

/// 截断并去掉潜在的分隔符，避免把整段 SQL 或用户数据塞进错误消息。
fn describe(message: String) -> String {
    let trimmed: String = message.chars().take(200).collect();
    trimmed.replace('\n', " ")
}
