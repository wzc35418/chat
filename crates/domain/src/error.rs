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
    /// 候选/操作基于的控制状态已陈旧。
    StaleControlHead { detail: &'static str },
    /// 策略迁移不合法或操作与授权结果不一致。
    InvalidPolicyTransition { detail: &'static str },
    /// 控制结果（MLS 实际增删/epoch）与业务预期不一致。
    InvalidControlResult { detail: &'static str },
    /// 日志冲突：群/epoch/序号与本地已验证状态矛盾。
    LogConflict { detail: &'static str },
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
            DomainError::StaleControlHead { .. } => ErrorCode::StaleControlHead,
            DomainError::InvalidPolicyTransition { .. } => ErrorCode::InvalidPolicyTransition,
            DomainError::InvalidControlResult { .. } => ErrorCode::InvalidControlResult,
            DomainError::LogConflict { .. } => ErrorCode::LogConflict,
        }
    }

    /// 是否允许自动重试：完全遵循冻结码表。
    ///
    /// 只有 `STALE_CONTROL_HEAD` 这类「先同步再重试」的条件可重试；权限、验证、
    /// 损坏与策略迁移失败都不可重试。
    pub const fn retryable(&self) -> bool {
        self.code().retryable()
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
            DomainError::StaleControlHead { detail } => {
                write!(formatter, "stale control head: {detail}")
            }
            DomainError::InvalidPolicyTransition { detail } => {
                write!(formatter, "invalid policy transition: {detail}")
            }
            DomainError::InvalidControlResult { detail } => {
                write!(formatter, "invalid control result: {detail}")
            }
            DomainError::LogConflict { detail } => write!(formatter, "log conflict: {detail}"),
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
            ErrorCode::StaleControlHead => InternalError::StaleControlHead,
            ErrorCode::LogConflict => InternalError::LogConflict,
            code => InternalError::Classified(code),
        }
    }
}

/// 协议层错误（规范编码/版本）只能以策略迁移失败的形式进入领域层。
impl From<chat_protocol::ProtocolError> for DomainError {
    fn from(error: chat_protocol::ProtocolError) -> Self {
        match error {
            chat_protocol::ProtocolError::UnsupportedProtocolVersion => {
                DomainError::InvalidPolicyTransition {
                    detail: "unsupported protocol version for policy encoding",
                }
            }
            chat_protocol::ProtocolError::ContentTooLarge { .. } => {
                DomainError::InvalidPolicyTransition {
                    detail: "policy encoding exceeds protocol limits",
                }
            }
            _ => DomainError::InvalidPolicyTransition {
                detail: "policy canonical encoding failed",
            },
        }
    }
}

/// 领域层 `Result`。
pub type Result<T> = core::result::Result<T, DomainError>;
