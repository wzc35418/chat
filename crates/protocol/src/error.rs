//! 稳定错误码、脱敏公共错误与内部错误映射（PROTOCOL-014）。
//!
//! 三条硬约束：
//!
//! 1. 错误码是冻结的 ASCII 常量，同名同义；
//! 2. 公共错误只携带码表里的静态 `detail_key`，内部诊断（可能包含路径、明文或密钥）
//!    绝不进入 `PublicError`；
//! 3. 重试提示有上限，未知错误默认不可重试。

use core::fmt;

use crate::cursor::CursorError;
use crate::decode::MalformedFrame;
use crate::extension::ExtensionError;
use crate::ids::{IdKind, OperationId};

/// 脱敏后回给 UI/节点的稳定错误码。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ErrorCode {
    NetworkUnavailable,
    NodeUnavailable,
    RateLimited,
    QuotaExceeded,
    PermissionDenied,
    DeviceRevoked,
    ContactNotAccepted,
    StaleControlHead,
    StaleEpoch,
    MissingLogEntries,
    InvalidCredential,
    InvalidCiphertext,
    LogConflict,
    ProtocolUnsupported,
    ContentTooLarge,
    StorageLocked,
    StorageFailure,
    StateCorrupted,
    InvalidId,
    MalformedFrame,
    InvalidCandidate,
    InvalidSignature,
    InvalidSequence,
    InvalidExtension,
    InvalidCursor,
    CursorExpired,
    InvalidCursorScope,
    InvalidTarget,
    GroupLimitExceeded,
    DeviceLimitExceeded,
    OwnerTransferRequired,
    NotMember,
    InvalidStateTransition,
    InvalidReceiptScope,
    InvalidReply,
    MessageNotVisible,
    HistoryNotAuthorized,
    InvalidEventBinding,
    RequiredCapabilityMissing,
    UnknownResult,
    InternalFailure,
}

impl ErrorCode {
    /// 冻结的线/UI 码字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::NetworkUnavailable => "NETWORK_UNAVAILABLE",
            ErrorCode::NodeUnavailable => "NODE_UNAVAILABLE",
            ErrorCode::RateLimited => "RATE_LIMITED",
            ErrorCode::QuotaExceeded => "QUOTA_EXCEEDED",
            ErrorCode::PermissionDenied => "PERMISSION_DENIED",
            ErrorCode::DeviceRevoked => "DEVICE_REVOKED",
            ErrorCode::ContactNotAccepted => "CONTACT_NOT_ACCEPTED",
            ErrorCode::StaleControlHead => "STALE_CONTROL_HEAD",
            ErrorCode::StaleEpoch => "STALE_EPOCH",
            ErrorCode::MissingLogEntries => "MISSING_LOG_ENTRIES",
            ErrorCode::InvalidCredential => "INVALID_CREDENTIAL",
            ErrorCode::InvalidCiphertext => "INVALID_CIPHERTEXT",
            ErrorCode::LogConflict => "LOG_CONFLICT",
            ErrorCode::ProtocolUnsupported => "PROTOCOL_UNSUPPORTED",
            ErrorCode::ContentTooLarge => "CONTENT_TOO_LARGE",
            ErrorCode::StorageLocked => "STORAGE_LOCKED",
            ErrorCode::StorageFailure => "STORAGE_FAILURE",
            ErrorCode::StateCorrupted => "STATE_CORRUPTED",
            ErrorCode::InvalidId => "INVALID_ID",
            ErrorCode::MalformedFrame => "MALFORMED_FRAME",
            ErrorCode::InvalidCandidate => "INVALID_CANDIDATE",
            ErrorCode::InvalidSignature => "INVALID_SIGNATURE",
            ErrorCode::InvalidSequence => "INVALID_SEQUENCE",
            ErrorCode::InvalidExtension => "INVALID_EXTENSION",
            ErrorCode::InvalidCursor => "INVALID_CURSOR",
            ErrorCode::CursorExpired => "CURSOR_EXPIRED",
            ErrorCode::InvalidCursorScope => "INVALID_CURSOR_SCOPE",
            ErrorCode::InvalidTarget => "INVALID_TARGET",
            ErrorCode::GroupLimitExceeded => "GROUP_LIMIT_EXCEEDED",
            ErrorCode::DeviceLimitExceeded => "DEVICE_LIMIT_EXCEEDED",
            ErrorCode::OwnerTransferRequired => "OWNER_TRANSFER_REQUIRED",
            ErrorCode::NotMember => "NOT_MEMBER",
            ErrorCode::InvalidStateTransition => "INVALID_STATE_TRANSITION",
            ErrorCode::InvalidReceiptScope => "INVALID_RECEIPT_SCOPE",
            ErrorCode::InvalidReply => "INVALID_REPLY",
            ErrorCode::MessageNotVisible => "MESSAGE_NOT_VISIBLE",
            ErrorCode::HistoryNotAuthorized => "HISTORY_NOT_AUTHORIZED",
            ErrorCode::InvalidEventBinding => "INVALID_EVENT_BINDING",
            ErrorCode::RequiredCapabilityMissing => "REQUIRED_CAPABILITY_MISSING",
            ErrorCode::UnknownResult => "UNKNOWN_RESULT",
            ErrorCode::InternalFailure => "INTERNAL_FAILURE",
        }
    }

    /// 码表全量，供唯一性测试与对外文档使用。
    pub const ALL: [ErrorCode; 41] = [
        ErrorCode::NetworkUnavailable,
        ErrorCode::NodeUnavailable,
        ErrorCode::RateLimited,
        ErrorCode::QuotaExceeded,
        ErrorCode::PermissionDenied,
        ErrorCode::DeviceRevoked,
        ErrorCode::ContactNotAccepted,
        ErrorCode::StaleControlHead,
        ErrorCode::StaleEpoch,
        ErrorCode::MissingLogEntries,
        ErrorCode::InvalidCredential,
        ErrorCode::InvalidCiphertext,
        ErrorCode::LogConflict,
        ErrorCode::ProtocolUnsupported,
        ErrorCode::ContentTooLarge,
        ErrorCode::StorageLocked,
        ErrorCode::StorageFailure,
        ErrorCode::StateCorrupted,
        ErrorCode::InvalidId,
        ErrorCode::MalformedFrame,
        ErrorCode::InvalidCandidate,
        ErrorCode::InvalidSignature,
        ErrorCode::InvalidSequence,
        ErrorCode::InvalidExtension,
        ErrorCode::InvalidCursor,
        ErrorCode::CursorExpired,
        ErrorCode::InvalidCursorScope,
        ErrorCode::InvalidTarget,
        ErrorCode::GroupLimitExceeded,
        ErrorCode::DeviceLimitExceeded,
        ErrorCode::OwnerTransferRequired,
        ErrorCode::NotMember,
        ErrorCode::InvalidStateTransition,
        ErrorCode::InvalidReceiptScope,
        ErrorCode::InvalidReply,
        ErrorCode::MessageNotVisible,
        ErrorCode::HistoryNotAuthorized,
        ErrorCode::InvalidEventBinding,
        ErrorCode::RequiredCapabilityMissing,
        ErrorCode::UnknownResult,
        ErrorCode::InternalFailure,
    ];

    /// 该码是否允许自动重试。
    ///
    /// 只有「网络/节点/限流/存储忙/需要补齐或同步后重试」是可自动重试的；
    /// 验证、损坏、权限与未知错误一律不可无限重试。
    pub const fn retryable(self) -> bool {
        matches!(
            self,
            ErrorCode::NetworkUnavailable
                | ErrorCode::NodeUnavailable
                | ErrorCode::RateLimited
                | ErrorCode::StaleControlHead
                | ErrorCode::StaleEpoch
                | ErrorCode::MissingLogEntries
                | ErrorCode::StorageLocked
        )
    }
}

/// `retryAfter` 的冻结上限：5 分钟。
pub const MAX_RETRY_AFTER_MS: u64 = 300_000;
/// 限流但没有明确建议时的默认退避。
pub const DEFAULT_RATE_LIMIT_RETRY_MS: u64 = 1_000;

/// ID 解析失败的具体原因（PROTOCOL-001）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdError {
    Empty,
    TooLong {
        limit: usize,
    },
    NonAscii,
    Malformed,
    WrongKind {
        expected: IdKind,
        found: Option<IdKind>,
    },
    UnsupportedVersion,
    NonCanonicalHex,
    WrongLength {
        expected: usize,
        found: usize,
    },
}

impl fmt::Display for IdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdError::Empty => formatter.write_str("id is empty"),
            IdError::TooLong { limit } => write!(formatter, "id exceeds {limit} bytes"),
            IdError::NonAscii => formatter.write_str("id must be ASCII"),
            IdError::Malformed => formatter.write_str("id must be prefix:version:hex"),
            IdError::WrongKind { expected, found } => match found {
                Some(found) => write!(
                    formatter,
                    "expected {} id, found {} id",
                    expected.prefix(),
                    found.prefix()
                ),
                None => write!(
                    formatter,
                    "unknown id prefix (expected {})",
                    expected.prefix()
                ),
            },
            IdError::UnsupportedVersion => formatter.write_str("unsupported id version"),
            IdError::NonCanonicalHex => formatter.write_str("id payload is not lowercase hex"),
            IdError::WrongLength { expected, found } => {
                write!(
                    formatter,
                    "id payload must be {expected} hex chars, found {found}"
                )
            }
        }
    }
}

/// 协议层错误。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    /// PROTOCOL-001。
    InvalidId(IdError),
    /// 版本不支持（canonical/帧编码）。
    UnsupportedProtocolVersion,
    /// 规范编码拒绝未知安全字段。
    UnknownSafetyField(u16),
    /// 规范编码拒绝重复安全字段。
    DuplicateSafetyField(u16),
    /// 嵌套超过预算。
    TooDeep { limit: usize },
    /// 长度超过预算。
    ContentTooLarge { limit: usize },
    /// PROTOCOL-003。
    MalformedFrame(MalformedFrame),
    /// PROTOCOL-004。
    InvalidCandidate,
    /// PROTOCOL-006。
    InvalidSequence,
    /// PROTOCOL-010。
    InvalidExtension(ExtensionError),
    /// PROTOCOL-012/013。
    InvalidCursor(CursorError),
    /// PROTOCOL-012/013：过期。
    CursorExpired,
    /// PROTOCOL-012/013：跨范围复用。
    InvalidCursorScope,
    /// 验签失败（helper）。
    InvalidSignature,
    /// 系统随机源失败，调用方必须显式失败而不是退化为可预测值。
    RandomUnavailable,
}

impl ProtocolError {
    /// 稳定错误码。
    pub const fn code(&self) -> ErrorCode {
        match self {
            ProtocolError::InvalidId(_) => ErrorCode::InvalidId,
            ProtocolError::UnsupportedProtocolVersion => ErrorCode::ProtocolUnsupported,
            ProtocolError::UnknownSafetyField(_) | ProtocolError::DuplicateSafetyField(_) => {
                ErrorCode::ProtocolUnsupported
            }
            ProtocolError::TooDeep { .. } | ProtocolError::ContentTooLarge { .. } => {
                ErrorCode::ContentTooLarge
            }
            ProtocolError::MalformedFrame(_) => ErrorCode::MalformedFrame,
            ProtocolError::InvalidCandidate => ErrorCode::InvalidCandidate,
            ProtocolError::InvalidSequence => ErrorCode::InvalidSequence,
            ProtocolError::InvalidExtension(_) => ErrorCode::InvalidExtension,
            ProtocolError::InvalidCursor(_) => ErrorCode::InvalidCursor,
            ProtocolError::CursorExpired => ErrorCode::CursorExpired,
            ProtocolError::InvalidCursorScope => ErrorCode::InvalidCursorScope,
            ProtocolError::InvalidSignature => ErrorCode::InvalidSignature,
            ProtocolError::RandomUnavailable => ErrorCode::InternalFailure,
        }
    }

    /// 该错误默认是否可重试。协议层永远不可重试。
    pub const fn retryable(&self) -> bool {
        self.code().retryable()
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolError::InvalidId(reason) => write!(formatter, "invalid id: {reason}"),
            ProtocolError::UnsupportedProtocolVersion => {
                formatter.write_str("unsupported protocol version")
            }
            ProtocolError::UnknownSafetyField(id) => {
                write!(formatter, "unknown safety field {id}")
            }
            ProtocolError::DuplicateSafetyField(id) => {
                write!(formatter, "duplicate safety field {id}")
            }
            ProtocolError::TooDeep { limit } => write!(formatter, "nesting exceeds {limit}"),
            ProtocolError::ContentTooLarge { limit } => {
                write!(formatter, "content exceeds {limit}")
            }
            ProtocolError::MalformedFrame(reason) => write!(formatter, "malformed frame: {reason}"),
            ProtocolError::InvalidCandidate => formatter.write_str("invalid candidate"),
            ProtocolError::InvalidSequence => formatter.write_str("sequence must start at 1"),
            ProtocolError::InvalidExtension(reason) => {
                write!(formatter, "invalid extension: {reason}")
            }
            ProtocolError::InvalidCursor(reason) => write!(formatter, "invalid cursor: {reason}"),
            ProtocolError::CursorExpired => formatter.write_str("cursor expired"),
            ProtocolError::InvalidCursorScope => formatter.write_str("cursor scope mismatch"),
            ProtocolError::InvalidSignature => formatter.write_str("invalid signature"),
            ProtocolError::RandomUnavailable => {
                formatter.write_str("secure randomness unavailable")
            }
        }
    }
}

impl std::error::Error for ProtocolError {}

/// 协议层 `Result`。
pub type Result<T> = core::result::Result<T, ProtocolError>;

/// 内部错误分类（PROTOCOL-014 输入）。可能来自任意层，因此这里只保留分类，
/// 详细诊断不得回显。
#[derive(Clone, PartialEq, Eq)]
pub enum InternalError {
    NetworkUnavailable,
    NodeUnavailable,
    RateLimited {
        retry_after_ms: Option<u64>,
    },
    QuotaExceeded,
    PermissionDenied,
    DeviceRevoked,
    ContactNotAccepted,
    StaleControlHead,
    StaleEpoch,
    MissingLogEntries,
    InvalidCredential,
    InvalidCiphertext,
    LogConflict,
    ProtocolUnsupported,
    ContentTooLarge,
    StorageLocked,
    StorageFailure,
    StateCorrupted,
    SignatureInvalid,
    /// 畸形外部输入。
    MalformedInput,
    /// 未知结果：必须先按原 SubmissionId 对账。
    UnknownResult,
    /// 已经分类到冻结码的错误（例如领域层错误）。码本身即稳定身份。
    Classified(ErrorCode),
    /// 未分类错误；`detail` 只用于内部诊断，永远不会进入 `PublicError`。
    Unknown {
        detail: String,
    },
}

impl InternalError {
    /// 该分类的稳定错误码。
    pub fn code(&self) -> ErrorCode {
        match self {
            InternalError::NetworkUnavailable => ErrorCode::NetworkUnavailable,
            InternalError::NodeUnavailable => ErrorCode::NodeUnavailable,
            InternalError::RateLimited { .. } => ErrorCode::RateLimited,
            InternalError::QuotaExceeded => ErrorCode::QuotaExceeded,
            InternalError::PermissionDenied => ErrorCode::PermissionDenied,
            InternalError::DeviceRevoked => ErrorCode::DeviceRevoked,
            InternalError::ContactNotAccepted => ErrorCode::ContactNotAccepted,
            InternalError::StaleControlHead => ErrorCode::StaleControlHead,
            InternalError::StaleEpoch => ErrorCode::StaleEpoch,
            InternalError::MissingLogEntries => ErrorCode::MissingLogEntries,
            InternalError::InvalidCredential => ErrorCode::InvalidCredential,
            InternalError::InvalidCiphertext => ErrorCode::InvalidCiphertext,
            InternalError::LogConflict => ErrorCode::LogConflict,
            InternalError::ProtocolUnsupported => ErrorCode::ProtocolUnsupported,
            InternalError::ContentTooLarge => ErrorCode::ContentTooLarge,
            InternalError::StorageLocked => ErrorCode::StorageLocked,
            InternalError::StorageFailure => ErrorCode::StorageFailure,
            InternalError::StateCorrupted => ErrorCode::StateCorrupted,
            InternalError::SignatureInvalid => ErrorCode::InvalidSignature,
            InternalError::MalformedInput => ErrorCode::MalformedFrame,
            InternalError::UnknownResult => ErrorCode::UnknownResult,
            InternalError::Classified(code) => *code,
            InternalError::Unknown { .. } => ErrorCode::InternalFailure,
        }
    }

    fn detail_key(&self) -> &'static str {
        match self {
            InternalError::NetworkUnavailable => "network.unavailable",
            InternalError::NodeUnavailable => "node.unavailable",
            InternalError::RateLimited { .. } => "network.rate_limited",
            InternalError::QuotaExceeded => "node.quota_exceeded",
            InternalError::PermissionDenied => "domain.permission_denied",
            InternalError::DeviceRevoked => "identity.device_revoked",
            InternalError::ContactNotAccepted => "identity.contact_not_accepted",
            InternalError::StaleControlHead => "group.stale_control_head",
            InternalError::StaleEpoch => "group.stale_epoch",
            InternalError::MissingLogEntries => "sync.missing_log_entries",
            InternalError::InvalidCredential => "identity.invalid_credential",
            InternalError::InvalidCiphertext => "crypto.invalid_ciphertext",
            InternalError::LogConflict => "sync.log_conflict",
            InternalError::ProtocolUnsupported => "protocol.unsupported",
            InternalError::ContentTooLarge => "content.too_large",
            InternalError::StorageLocked => "storage.locked",
            InternalError::StorageFailure => "storage.failure",
            InternalError::StateCorrupted => "state.corrupted",
            InternalError::SignatureInvalid => "protocol.invalid_signature",
            InternalError::MalformedInput => "protocol.malformed_input",
            InternalError::UnknownResult => "sync.unknown_result",
            InternalError::Classified(_) => "error.classified",
            InternalError::Unknown { .. } => "internal.failure",
        }
    }
}

impl fmt::Debug for InternalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 不打印 detail，避免诊断信息进入日志。
        write!(formatter, "InternalError({})", self.code().as_str())
    }
}

impl fmt::Display for InternalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.detail_key())
    }
}

/// 字段级作用域绑定：成员字段与实现之间的绑定错误不可互换。
impl std::error::Error for InternalError {}

impl From<ProtocolError> for InternalError {
    fn from(error: ProtocolError) -> Self {
        match error.code() {
            ErrorCode::ContentTooLarge => InternalError::ContentTooLarge,
            ErrorCode::InvalidSignature => InternalError::SignatureInvalid,
            ErrorCode::ProtocolUnsupported => InternalError::ProtocolUnsupported,
            // 协议层每个失败都有冻结码，直接保留具体码，避免降级成 MALFORMED_FRAME。
            code => InternalError::Classified(code),
        }
    }
}

/// 对外错误。字段全部来自冻结码表，不含内部 cause。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicError {
    code: ErrorCode,
    retryable: bool,
    retry_after_ms: Option<u64>,
    operation_id: Option<OperationId>,
    detail_key: &'static str,
}

impl PublicError {
    /// 稳定错误码。
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    /// 是否允许自动重试。
    pub const fn retryable(&self) -> bool {
        self.retryable
    }

    /// 建议退避（毫秒），已限幅。
    pub const fn retry_after_ms(&self) -> Option<u64> {
        self.retry_after_ms
    }

    /// 关联业务幂等键（如调用方提供）。
    pub const fn operation_id(&self) -> Option<OperationId> {
        self.operation_id
    }

    /// 静态诊断键，用于 UI 文案映射；不含任何用户内容。
    pub const fn detail_key(&self) -> &'static str {
        self.detail_key
    }
}

impl fmt::Display for PublicError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} [{}]", self.code.as_str(), self.detail_key)
    }
}

impl std::error::Error for PublicError {}

/// PROTOCOL-014：把内部错误映射为稳定脱敏结构。
///
/// - 先按分类选码；
/// - 再按码表决定是否可重试（未知错误一律 `INTERNAL_FAILURE` 且不可无限重试）；
/// - 限流退避被限幅到 `MAX_RETRY_AFTER_MS`；
/// - 仅保留调用方显式提供的 `operationId`；
/// - 内部 `cause`/`detail` 被丢弃，公共结构只能携带静态 `detail_key`。
pub fn map_error(error: InternalError, operation: Option<OperationId>) -> PublicError {
    let code = error.code();
    let retryable = code.retryable();
    let retry_after_ms = match (&error, retryable) {
        (InternalError::RateLimited { retry_after_ms }, _) => {
            let raw = retry_after_ms.unwrap_or(DEFAULT_RATE_LIMIT_RETRY_MS);
            Some(raw.clamp(1, MAX_RETRY_AFTER_MS))
        }
        _ => None,
    };
    PublicError {
        code,
        retryable,
        retry_after_ms,
        operation_id: operation,
        detail_key: error.detail_key(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_is_unique_and_non_empty() {
        let mut seen = std::collections::BTreeSet::new();
        for code in ErrorCode::ALL {
            let text = code.as_str();
            assert!(!text.is_empty());
            assert!(text.is_ascii());
            assert!(text.chars().all(|c| c.is_ascii_uppercase() || c == '_'));
            assert!(seen.insert(text), "duplicate code {text}");
        }
        assert_eq!(seen.len(), ErrorCode::ALL.len());
    }

    #[test]
    fn unknown_error_maps_to_internal_failure_without_leaking_detail() {
        let public = map_error(
            InternalError::Unknown {
                detail: "private key seed 0xdeadbeef / body plaintext".to_owned(),
            },
            None,
        );
        assert_eq!(public.code(), ErrorCode::InternalFailure);
        assert!(!public.retryable());
        assert_eq!(public.detail_key(), "internal.failure");
        let rendered = format!("{public} {public:?}");
        assert!(!rendered.contains("deadbeef"));
        assert!(!rendered.contains("plaintext"));
    }

    #[test]
    fn storage_corruption_is_not_a_network_retry() {
        let public = map_error(InternalError::StateCorrupted, None);
        assert_eq!(public.code(), ErrorCode::StateCorrupted);
        assert!(!public.retryable());
        let storage = map_error(InternalError::StorageFailure, None);
        assert!(!storage.retryable());
        let locked = map_error(InternalError::StorageLocked, None);
        assert!(locked.retryable());
    }

    #[test]
    fn retry_after_is_clamped_and_operation_id_is_kept() {
        let operation = OperationId::from_bytes([3; 32]);
        let public = map_error(
            InternalError::RateLimited {
                retry_after_ms: Some(u64::MAX),
            },
            Some(operation),
        );
        assert_eq!(public.code(), ErrorCode::RateLimited);
        assert!(public.retryable());
        assert_eq!(public.retry_after_ms(), Some(MAX_RETRY_AFTER_MS));
        assert_eq!(public.operation_id(), Some(operation));

        let defaulted = map_error(
            InternalError::RateLimited {
                retry_after_ms: None,
            },
            None,
        );
        assert_eq!(
            defaulted.retry_after_ms(),
            Some(DEFAULT_RATE_LIMIT_RETRY_MS)
        );

        let zero = map_error(
            InternalError::RateLimited {
                retry_after_ms: Some(0),
            },
            None,
        );
        assert_eq!(zero.retry_after_ms(), Some(1));
    }

    #[test]
    fn network_error_is_retryable() {
        assert!(map_error(InternalError::NetworkUnavailable, None).retryable());
        assert!(map_error(InternalError::NodeUnavailable, None).retryable());
        assert!(!map_error(InternalError::UnknownResult, None).retryable());
        assert!(!map_error(InternalError::SignatureInvalid, None).retryable());
    }
}
