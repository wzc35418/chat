//! sync 层稳定错误。
//!
//! 码表**一律取自 PROTOCOL-014**（[`chat_protocol::ErrorCode`]），本层不新增错误码：
//! 计划书里的 `JOB_KEY_CONFLICT` / `PERMANENT_FAILURE` / `RETRY_BUDGET_EXHAUSTED`
//! 都不是码表条目，按下面方式表达（映射表见 docs/implementation/sync.md 的实现记录）：
//!
//! - 业务键/状态机冲突 → `INVALID_STATE_TRANSITION`（结果未知时要求重发 → `UNKNOWN_RESULT`）；
//! - `PERMANENT_FAILURE` / `RETRY_BUDGET_EXHAUSTED` 是**重试策略结果**
//!   （[`crate::retry::RetryAction`]），同时保留根因冻结码，不伪装成新码。
//!
//! 错误只携带静态 `detail`，绝不包含消息正文、密钥或用户内容。

use core::fmt;

use chat_protocol::{ErrorCode as ProtocolErrorCode, InternalError, PublicError, map_error};

/// 冻结错误码（PROTOCOL-014）。
pub type ErrorCode = ProtocolErrorCode;

/// sync 错误：冻结码 + 静态诊断键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncError {
    code: ErrorCode,
    detail: &'static str,
}

impl SyncError {
    /// 由冻结码与静态诊断构造。
    pub const fn new(code: ErrorCode, detail: &'static str) -> Self {
        Self { code, detail }
    }

    /// 冻结错误码。
    pub const fn code(self) -> ErrorCode {
        self.code
    }

    /// 静态诊断键（不含用户内容）。
    pub const fn detail(self) -> &'static str {
        self.detail
    }

    /// 该码是否允许自动重试（由 PROTOCOL-014 码表决定，本层不另行判断）。
    pub const fn retryable(self) -> bool {
        self.code.retryable()
    }

    /// 交给桥接层/UI 的脱敏公共错误。
    pub fn to_public_error(self) -> PublicError {
        map_error(InternalError::Classified(self.code), None)
    }
}

impl fmt::Display for SyncError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} [{}]", self.code.as_str(), self.detail)
    }
}

impl std::error::Error for SyncError {}

/// sync 结果类型。
pub type Result<T> = core::result::Result<T, SyncError>;

pub(crate) const fn invalid_target(detail: &'static str) -> SyncError {
    SyncError::new(ErrorCode::InvalidTarget, detail)
}

pub(crate) const fn invalid_id(detail: &'static str) -> SyncError {
    SyncError::new(ErrorCode::InvalidId, detail)
}

pub(crate) const fn content_too_large(detail: &'static str) -> SyncError {
    SyncError::new(ErrorCode::ContentTooLarge, detail)
}

pub(crate) const fn invalid_cursor(detail: &'static str) -> SyncError {
    SyncError::new(ErrorCode::InvalidCursor, detail)
}

pub(crate) const fn invalid_state(detail: &'static str) -> SyncError {
    SyncError::new(ErrorCode::InvalidStateTransition, detail)
}

pub(crate) const fn corrupted(detail: &'static str) -> SyncError {
    SyncError::new(ErrorCode::StateCorrupted, detail)
}

pub(crate) const fn invalid_sequence(detail: &'static str) -> SyncError {
    SyncError::new(ErrorCode::InvalidSequence, detail)
}

pub(crate) const fn invalid_candidate(detail: &'static str) -> SyncError {
    SyncError::new(ErrorCode::InvalidCandidate, detail)
}

pub(crate) const fn missing_log_entries(detail: &'static str) -> SyncError {
    SyncError::new(ErrorCode::MissingLogEntries, detail)
}

pub(crate) const fn log_conflict(detail: &'static str) -> SyncError {
    SyncError::new(ErrorCode::LogConflict, detail)
}

pub(crate) const fn unknown_result(detail: &'static str) -> SyncError {
    SyncError::new(ErrorCode::UnknownResult, detail)
}
