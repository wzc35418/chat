//! 传输层稳定错误码。
//!
//! 与 PROTOCOL-014 的关系：
//!
//! - 帧/尺寸类失败直接复用冻结码：`MALFORMED_FRAME`、`CONTENT_TOO_LARGE`；
//! - 能力协商不兼容复用冻结码 `PROTOCOL_UNSUPPORTED`（PROTOCOL-011 的协商失败语义）；
//! - 其余 `TRANSPORT_*` 与分片/重试/迁移码是本 crate 新增的稳定码，尚未登记到
//!   `chat_protocol::ErrorCode` 冻结表（新增协议码需要协议模块版本化变更）。
//!   跨 FFI 暴露时应在协议层登记前映射为 `INTERNAL_FAILURE` 并保留本层码做诊断，
//!   不得伪装成网络类可重试错误。
//! - 本层错误都不可自动重试；重试节奏由 [`crate::retry`] 的显式策略决定。
//!
//! 新增稳定码必须同时登记到 `docs/implementation/transport.md` 的码表。

use core::fmt;

use chat_protocol::ErrorCode;

/// 传输层错误。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// 会话/端点/配置非法。
    InvalidConfig { detail: &'static str },
    /// 连接状态迁移非法。
    InvalidTransition,
    /// 协商结果与本地硬约束不兼容（版本/必需能力/限额）。
    CapabilityMismatch { detail: &'static str },
    /// 尚未完成能力协商就尝试连接。
    CapabilityNotNegotiated,
    /// 帧结构非法（长度头、字段、分片尺寸不一致）。
    MalformedFrame { detail: &'static str },
    /// 帧/分片超过上限。
    ContentTooLarge { size: usize, limit: usize },
    /// 帧预算配置非法。
    InvalidFrameLimits { detail: &'static str },
    /// 同一分片重复到达。
    DuplicateFragment,
    /// 分片乱序（索引跳跃或新帧先于旧帧完成）。
    OutOfOrderFragment,
    /// 迟到的旧帧（完成号或索引更小）。
    StaleFrame,
    /// 当前帧尚未收齐就切换到下一帧，或收齐后总长不一致。
    IncompleteFrame,
    /// 重试策略参数非法。
    InvalidRetryPolicy { detail: &'static str },
    /// 会话当前状态不可收发（未连接/已关闭）。
    SessionNotUsable,
    /// 对端会话/端点绑定不一致（跨会话复用或未认证来源）。
    SessionMismatch { detail: &'static str },
    /// 发送队列/分片预算超限。
    SendBudgetExceeded { detail: &'static str },
    /// Pull 计划参数非法（范围/分页/请求数）。
    PullPlanInvalid { detail: &'static str },
    /// 同一请求索引的响应重复到达。
    DuplicateResponse,
    /// 响应与请求计划不一致（索引/范围/条目数）。
    ResponseMismatch { detail: &'static str },
}

impl TransportError {
    /// 本层稳定码字符串。
    pub const fn code(&self) -> &'static str {
        match self {
            TransportError::InvalidConfig { .. } => "INVALID_TRANSPORT_CONFIG",
            TransportError::InvalidTransition => "INVALID_TRANSITION",
            TransportError::CapabilityMismatch { .. } => "CAPABILITY_MISMATCH",
            TransportError::CapabilityNotNegotiated => "CAPABILITY_NOT_NEGOTIATED",
            TransportError::MalformedFrame { .. } => "MALFORMED_FRAME",
            TransportError::ContentTooLarge { .. } => "CONTENT_TOO_LARGE",
            TransportError::InvalidFrameLimits { .. } => "INVALID_FRAME_LIMITS",
            TransportError::DuplicateFragment => "DUPLICATE_FRAGMENT",
            TransportError::OutOfOrderFragment => "OUT_OF_ORDER_FRAGMENT",
            TransportError::StaleFrame => "STALE_FRAME",
            TransportError::IncompleteFrame => "INCOMPLETE_FRAME",
            TransportError::InvalidRetryPolicy { .. } => "INVALID_RETRY_POLICY",
            TransportError::SessionNotUsable => "SESSION_NOT_USABLE",
            TransportError::SessionMismatch { .. } => "SESSION_MISMATCH",
            TransportError::SendBudgetExceeded { .. } => "SEND_BUDGET_EXCEEDED",
            TransportError::PullPlanInvalid { .. } => "PULL_PLAN_INVALID",
            TransportError::DuplicateResponse => "DUPLICATE_RESPONSE",
            TransportError::ResponseMismatch { .. } => "RESPONSE_MISMATCH",
        }
    }

    /// 本层不允许自动重试（重试由显式策略驱动）。
    pub const fn retryable(&self) -> bool {
        false
    }

    /// 已在 PROTOCOL-014 冻结码表中登记的同义码；没有则为 `None`。
    pub const fn frozen_code(&self) -> Option<ErrorCode> {
        match self {
            TransportError::MalformedFrame { .. } => Some(ErrorCode::MalformedFrame),
            TransportError::ContentTooLarge { .. } => Some(ErrorCode::ContentTooLarge),
            TransportError::CapabilityMismatch { .. } => Some(ErrorCode::ProtocolUnsupported),
            _ => None,
        }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::InvalidConfig { detail } => {
                write!(formatter, "invalid transport config: {detail}")
            }
            TransportError::InvalidTransition => {
                formatter.write_str("invalid connection state transition")
            }
            TransportError::CapabilityMismatch { detail } => {
                write!(formatter, "capability mismatch: {detail}")
            }
            TransportError::CapabilityNotNegotiated => {
                formatter.write_str("capabilities are not negotiated")
            }
            TransportError::MalformedFrame { detail } => {
                write!(formatter, "malformed frame: {detail}")
            }
            TransportError::ContentTooLarge { size, limit } => {
                write!(formatter, "content {size} bytes exceeds {limit}")
            }
            TransportError::InvalidFrameLimits { detail } => {
                write!(formatter, "invalid frame limits: {detail}")
            }
            TransportError::DuplicateFragment => formatter.write_str("duplicate frame fragment"),
            TransportError::OutOfOrderFragment => {
                formatter.write_str("out-of-order frame fragment")
            }
            TransportError::StaleFrame => formatter.write_str("stale frame"),
            TransportError::IncompleteFrame => formatter.write_str("incomplete frame"),
            TransportError::InvalidRetryPolicy { detail } => {
                write!(formatter, "invalid retry policy: {detail}")
            }
            TransportError::SessionNotUsable => {
                formatter.write_str("session is not usable for peer traffic")
            }
            TransportError::SessionMismatch { detail } => {
                write!(formatter, "session mismatch: {detail}")
            }
            TransportError::SendBudgetExceeded { detail } => {
                write!(formatter, "send budget exceeded: {detail}")
            }
            TransportError::PullPlanInvalid { detail } => {
                write!(formatter, "invalid pull plan: {detail}")
            }
            TransportError::DuplicateResponse => formatter.write_str("duplicate pull response"),
            TransportError::ResponseMismatch { detail } => {
                write!(formatter, "pull response mismatch: {detail}")
            }
        }
    }
}

impl std::error::Error for TransportError {}

/// 传输层 `Result`。
pub type Result<T> = core::result::Result<T, TransportError>;

/// 传输层稳定码全量，供唯一性测试与文档码表使用。
pub const CODE_TABLE: [&str; 18] = [
    "INVALID_TRANSPORT_CONFIG",
    "INVALID_TRANSITION",
    "CAPABILITY_MISMATCH",
    "CAPABILITY_NOT_NEGOTIATED",
    "MALFORMED_FRAME",
    "CONTENT_TOO_LARGE",
    "INVALID_FRAME_LIMITS",
    "DUPLICATE_FRAGMENT",
    "OUT_OF_ORDER_FRAGMENT",
    "STALE_FRAME",
    "INCOMPLETE_FRAME",
    "INVALID_RETRY_POLICY",
    "SESSION_NOT_USABLE",
    "SESSION_MISMATCH",
    "SEND_BUDGET_EXCEEDED",
    "PULL_PLAN_INVALID",
    "DUPLICATE_RESPONSE",
    "RESPONSE_MISMATCH",
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn codes_are_unique_and_stable() {
        let mut seen = BTreeSet::new();
        for code in CODE_TABLE {
            assert!(code.is_ascii());
            assert!(!code.is_empty());
            assert!(seen.insert(code), "duplicate code {code}");
        }
        assert_eq!(seen.len(), CODE_TABLE.len());
    }

    #[test]
    fn frozen_codes_only_for_shared_semantics() {
        assert_eq!(
            TransportError::MalformedFrame { detail: "x" }.frozen_code(),
            Some(ErrorCode::MalformedFrame)
        );
        assert_eq!(
            TransportError::ContentTooLarge { size: 9, limit: 8 }.frozen_code(),
            Some(ErrorCode::ContentTooLarge)
        );
        assert_eq!(
            TransportError::CapabilityMismatch { detail: "x" }.frozen_code(),
            Some(ErrorCode::ProtocolUnsupported)
        );
        assert_eq!(TransportError::StaleFrame.frozen_code(), None);
        assert!(!TransportError::StaleFrame.retryable());
        assert!(!TransportError::InvalidTransition.retryable());
        for error in [
            TransportError::SessionNotUsable,
            TransportError::SessionMismatch { detail: "x" },
            TransportError::SendBudgetExceeded { detail: "x" },
            TransportError::PullPlanInvalid { detail: "x" },
            TransportError::DuplicateResponse,
            TransportError::ResponseMismatch { detail: "x" },
        ] {
            assert_eq!(error.frozen_code(), None);
            assert!(!error.retryable());
            assert!(!error.code().is_empty());
        }
    }
}
