//! 领域层稳定错误码。
//!
//! 错误码复用 [`chat_protocol::ErrorCode`] 的冻结码表；本层只携带静态
//! `detail` 字符串，绝不把用户内容或密钥写进错误。

use core::fmt;

use chat_protocol::{ErrorCode, InternalError};

/// 领域规则错误。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomainError {
    /// 角色不允许该操作。
    PermissionDenied { detail: &'static str },
    /// 设备不在成员设备集合内（已撤销或从未授权）。
    DeviceRevoked,
    /// 操作者不是当前成员。
    NotMember,
    /// 内容超过上限。
    ContentTooLarge { size: usize, limit: usize },
    /// 目标非法（不存在、已离开、重复设备等）。
    InvalidTarget { detail: &'static str },
    /// 账户数超过群上限。
    GroupLimitExceeded { detail: &'static str },
    /// 设备数超过上限。
    DeviceLimitExceeded { detail: &'static str },
    /// 群主必须先转让或显式解散。
    OwnerTransferRequired,
    /// 发送状态机不接受该证据。
    InvalidStateTransition { detail: &'static str },
    /// 回执超出消息时点的收件人范围。
    InvalidReceiptScope { detail: &'static str },
    /// 回复引用不合法。
    InvalidReply { detail: &'static str },
    /// 被引用消息对调用方不可见。
    MessageNotVisible,
    /// 请求的序号窗口不在成员可见窗口内。
    HistoryNotAuthorized { detail: &'static str },
    /// 快照本身违反不变量（例如群主不唯一），必须停止该群分支。
    InvalidSnapshot { detail: &'static str },
}

impl DomainError {
    /// 冻结错误码。
    pub const fn code(&self) -> ErrorCode {
        match self {
            DomainError::PermissionDenied { .. } => ErrorCode::PermissionDenied,
            DomainError::DeviceRevoked => ErrorCode::DeviceRevoked,
            DomainError::NotMember => ErrorCode::NotMember,
            DomainError::ContentTooLarge { .. } => ErrorCode::ContentTooLarge,
            DomainError::InvalidTarget { .. } => ErrorCode::InvalidTarget,
            DomainError::GroupLimitExceeded { .. } => ErrorCode::GroupLimitExceeded,
            DomainError::DeviceLimitExceeded { .. } => ErrorCode::DeviceLimitExceeded,
            DomainError::OwnerTransferRequired => ErrorCode::OwnerTransferRequired,
            DomainError::InvalidStateTransition { .. } => ErrorCode::InvalidStateTransition,
            DomainError::InvalidReceiptScope { .. } => ErrorCode::InvalidReceiptScope,
            DomainError::InvalidReply { .. } => ErrorCode::InvalidReply,
            DomainError::MessageNotVisible => ErrorCode::MessageNotVisible,
            DomainError::HistoryNotAuthorized { .. } => ErrorCode::HistoryNotAuthorized,
            DomainError::InvalidSnapshot { .. } => ErrorCode::StateCorrupted,
        }
    }

    /// 领域层错误一律不可自动重试（需要先改变正式状态）。
    pub const fn retryable(&self) -> bool {
        false
    }
}

impl fmt::Display for DomainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DomainError::PermissionDenied { detail } => {
                write!(formatter, "permission denied: {detail}")
            }
            DomainError::DeviceRevoked => formatter.write_str("device revoked"),
            DomainError::NotMember => formatter.write_str("not a member"),
            DomainError::ContentTooLarge { size, limit } => {
                write!(formatter, "content {size} bytes exceeds {limit}")
            }
            DomainError::InvalidTarget { detail } => write!(formatter, "invalid target: {detail}"),
            DomainError::GroupLimitExceeded { detail } => {
                write!(formatter, "group limit exceeded: {detail}")
            }
            DomainError::DeviceLimitExceeded { detail } => {
                write!(formatter, "device limit exceeded: {detail}")
            }
            DomainError::OwnerTransferRequired => {
                formatter.write_str("owner must transfer ownership or dissolve")
            }
            DomainError::InvalidStateTransition { detail } => {
                write!(formatter, "invalid send state transition: {detail}")
            }
            DomainError::InvalidReceiptScope { detail } => {
                write!(formatter, "invalid receipt scope: {detail}")
            }
            DomainError::InvalidReply { detail } => write!(formatter, "invalid reply: {detail}"),
            DomainError::MessageNotVisible => formatter.write_str("message is not visible"),
            DomainError::HistoryNotAuthorized { detail } => {
                write!(formatter, "history not authorized: {detail}")
            }
            DomainError::InvalidSnapshot { detail } => {
                write!(formatter, "invalid group snapshot: {detail}")
            }
        }
    }
}

impl std::error::Error for DomainError {}

impl From<DomainError> for InternalError {
    fn from(error: DomainError) -> Self {
        // 领域错误自带冻结码，直接保留具体分类（权限/容量/可见性等），
        // 不降级成通用的畸形输入。
        match error.code() {
            ErrorCode::PermissionDenied => InternalError::PermissionDenied,
            ErrorCode::DeviceRevoked => InternalError::DeviceRevoked,
            ErrorCode::ContentTooLarge => InternalError::ContentTooLarge,
            ErrorCode::StateCorrupted => InternalError::StateCorrupted,
            code => InternalError::Classified(code),
        }
    }
}

/// 领域层 `Result`。
pub type Result<T> = core::result::Result<T, DomainError>;
