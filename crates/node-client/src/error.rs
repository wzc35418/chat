//! 客户端稳定错误：PROTOCOL-014 码表 + 传输层分类。
//!
//! 两条硬约束：
//!
//! 1. `code` 只能取 PROTOCOL-014 的 [`ErrorCode`]，上层据此做统一处理；
//! 2. `detail` 只能是本文件里的静态短语 —— 节点响应正文、密钥、URL 明文都不进错误，
//!    避免把服务端内容泄漏到 UI/日志。
use std::fmt;

use chat_protocol::ErrorCode;

/// 错误分类：调用方据它决定“重试 / 重新鉴权 / 提示用户 / 放弃”。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeClientErrorKind {
    /// 连接失败、连接中断、地址不可达。
    Network,
    /// 请求超时（总超时）。
    Timeout,
    /// 节点返回的结构不符合协议：非 JSON、缺字段、schema 版本未知、错误码未知、响应过大、
    /// 或节点返回的签名/回执/head 验签失败。
    Protocol,
    /// 鉴权失败：挑战无效/过期/重放、设备签名不匹配、凭据无效、设备已撤销。
    Authentication,
    /// 已鉴权但没有该资源的权限（非成员、历史窗口外、能力不足）。
    Permission,
    /// 冲突：同 submission/item 相异字节、群 ID 冲突、前置 head/epoch 过期。
    Conflict,
    /// 目标不存在（群未知、节点不匹配）。
    NotFound,
    /// 请求形状或本地参数不合法（调用方输入越界）。
    InvalidRequest,
    /// 触及上限：请求过大、响应过大、配额、容量、限流。
    Limit,
    /// 节点内部错误（迁移、存储、配置）。
    Internal,
    /// 客户端自身配置不合法（URL、超时、上限、设备与密钥不匹配）。
    Config,
}

impl NodeClientErrorKind {
    /// 稳定小写标签，便于日志与指标。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Timeout => "timeout",
            Self::Protocol => "protocol",
            Self::Authentication => "authentication",
            Self::Permission => "permission",
            Self::Conflict => "conflict",
            Self::NotFound => "not_found",
            Self::InvalidRequest => "invalid_request",
            Self::Limit => "limit",
            Self::Internal => "internal",
            Self::Config => "config",
        }
    }
}

/// 节点客户端错误。
#[derive(Clone, Debug)]
pub struct NodeClientError {
    code: ErrorCode,
    kind: NodeClientErrorKind,
    status: Option<u16>,
    node_code: Option<&'static str>,
    detail: &'static str,
}

impl NodeClientError {
    pub(crate) fn new(
        code: ErrorCode,
        kind: NodeClientErrorKind,
        status: Option<u16>,
        node_code: Option<&'static str>,
        detail: &'static str,
    ) -> Self {
        Self {
            code,
            kind,
            status,
            node_code,
            detail,
        }
    }

    /// PROTOCOL-014 稳定错误码。
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    /// 分类。
    pub fn kind(&self) -> NodeClientErrorKind {
        self.kind
    }

    /// HTTP 状态码（若是节点返回的响应）。
    pub fn http_status(&self) -> Option<u16> {
        self.status
    }

    /// 节点侧的稳定错误码字符串（若节点明确返回且可识别）。
    pub fn node_code(&self) -> Option<&'static str> {
        self.node_code
    }

    /// 固定短说明（不含节点正文/密钥/URL）。
    pub fn detail(&self) -> &'static str {
        self.detail
    }

    /// 是否值得自动重试（沿用 PROTOCOL-014 的 retryable 定义）。
    pub fn is_retryable(&self) -> bool {
        self.code.retryable()
    }

    /// 客户端配置错误。
    pub(crate) fn config(detail: &'static str) -> Self {
        Self::new(ErrorCode::InvalidId, NodeClientErrorKind::Config, None, None, detail)
    }

    /// 网络失败。
    pub(crate) fn network(detail: &'static str) -> Self {
        Self::new(
            ErrorCode::NetworkUnavailable,
            NodeClientErrorKind::Network,
            None,
            None,
            detail,
        )
    }

    /// 超时。
    pub(crate) fn timeout(detail: &'static str) -> Self {
        Self::new(
            ErrorCode::NetworkUnavailable,
            NodeClientErrorKind::Timeout,
            None,
            None,
            detail,
        )
    }

    /// 节点响应不符合协议（含本地无法验证的节点产物）。
    pub(crate) fn protocol(code: ErrorCode, detail: &'static str) -> Self {
        Self::new(code, NodeClientErrorKind::Protocol, None, None, detail)
    }

    /// 响应体超过客户端上限。
    pub(crate) fn response_too_large() -> Self {
        Self::new(
            ErrorCode::ContentTooLarge,
            NodeClientErrorKind::Limit,
            None,
            None,
            "node response exceeds the client limit",
        )
    }

    /// 请求体超过客户端上限。
    pub(crate) fn request_too_large() -> Self {
        Self::new(
            ErrorCode::ContentTooLarge,
            NodeClientErrorKind::Limit,
            None,
            None,
            "request body exceeds the client limit",
        )
    }

    /// 本地输入/参数不合法（不来自节点）。
    pub(crate) fn local_invalid(detail: &'static str) -> Self {
        Self::new(
            ErrorCode::MalformedFrame,
            NodeClientErrorKind::InvalidRequest,
            None,
            None,
            detail,
        )
    }

    /// reqwest 传输错误 → 分类。
    pub(crate) fn from_transport(error: &reqwest::Error) -> Self {
        if error.is_timeout() {
            return Self::timeout("node request timed out");
        }
        if error.is_connect() {
            return Self::network("node connection failed");
        }
        if error.is_body() || error.is_decode() {
            return Self::protocol(
                ErrorCode::MalformedFrame,
                "node response body is not readable",
            );
        }
        Self::network("node transport failed")
    }

    /// 节点返回的错误码字符串 → PROTOCOL-014 码表（未知值显式落到 UNKNOWN_RESULT）。
    pub fn code_for_node_code(node_code: &str) -> ErrorCode {
        match node_code {
            "INVALID_REQUEST" | "UNSUPPORTED_SCHEMA_VERSION" | "INVALID_ACK_BATCH" => {
                ErrorCode::MalformedFrame
            }
            "PAYLOAD_TOO_LARGE" | "CONTENT_TOO_LARGE" => ErrorCode::ContentTooLarge,
            "INVALID_CHALLENGE" | "AUTH_EXPIRED" | "AUTH_REPLAY" | "INVALID_CREDENTIAL" => {
                ErrorCode::InvalidCredential
            }
            "INVALID_SIGNATURE" => ErrorCode::InvalidSignature,
            "NODE_ID_MISMATCH" => ErrorCode::InvalidTarget,
            "DEVICE_REVOKED" => ErrorCode::DeviceRevoked,
            "DEVICE_LIMIT_EXCEEDED" => ErrorCode::DeviceLimitExceeded,
            "INVALID_CAPABILITY" | "AUTH_REQUIRED" => ErrorCode::RequiredCapabilityMissing,
            "PERMISSION_DENIED" => ErrorCode::PermissionDenied,
            "HISTORY_NOT_AUTHORIZED" => ErrorCode::HistoryNotAuthorized,
            "CHALLENGE_CAPACITY_EXCEEDED" => ErrorCode::NodeUnavailable,
            "RATE_LIMITED" => ErrorCode::RateLimited,
            "GROUP_NOT_FOUND" => ErrorCode::InvalidTarget,
            "GROUP_ID_CONFLICT" | "SUBMISSION_ID_CONFLICT" | "ITEM_ID_CONFLICT" => {
                ErrorCode::LogConflict
            }
            "GROUP_CAPACITY_EXCEEDED" => ErrorCode::GroupLimitExceeded,
            "STALE_CONTROL_HEAD" => ErrorCode::StaleControlHead,
            "STALE_EPOCH" => ErrorCode::StaleEpoch,
            "INVALID_CURSOR" | "INVALID_CURSOR_SCOPE" => ErrorCode::InvalidCursor,
            "QUOTA_EXCEEDED" => ErrorCode::QuotaExceeded,
            "BLOB_UPLOAD_CONFLICT" | "CHUNK_CONFLICT" => ErrorCode::LogConflict,
            "UPLOAD_NOT_FOUND" | "BLOB_NOT_FOUND" => ErrorCode::InvalidTarget,
            "UPLOAD_EXPIRED" => ErrorCode::InvalidStateTransition,
            "CHUNK_OUT_OF_ORDER" => ErrorCode::InvalidSequence,
            "MISSING_CHUNKS" => ErrorCode::MissingLogEntries,
            "MANIFEST_MISMATCH" => ErrorCode::InvalidCiphertext,
            "STORAGE_FAILURE" => ErrorCode::StorageFailure,
            "MIGRATION_FAILED" | "NODE_IDENTITY_MISMATCH" | "INVALID_CONFIG" | "ENCODING_FAILURE" => {
                ErrorCode::InternalFailure
            }
            "NETWORK_UNAVAILABLE" => ErrorCode::NetworkUnavailable,
            "NODE_UNAVAILABLE" => ErrorCode::NodeUnavailable,
            _ => ErrorCode::UnknownResult,
        }
    }

    /// 节点错误码 → 分类。
    fn kind_for_node_code(node_code: &str) -> NodeClientErrorKind {
        match node_code {
            "INVALID_CHALLENGE" | "AUTH_EXPIRED" | "AUTH_REPLAY" | "INVALID_SIGNATURE"
            | "INVALID_CREDENTIAL" | "DEVICE_REVOKED" => NodeClientErrorKind::Authentication,
            "INVALID_CAPABILITY" | "AUTH_REQUIRED" | "PERMISSION_DENIED"
            | "HISTORY_NOT_AUTHORIZED" => NodeClientErrorKind::Permission,
            "GROUP_ID_CONFLICT" | "SUBMISSION_ID_CONFLICT" | "ITEM_ID_CONFLICT"
            | "STALE_CONTROL_HEAD" | "STALE_EPOCH" => NodeClientErrorKind::Conflict,
            "GROUP_NOT_FOUND" | "UPLOAD_NOT_FOUND" | "BLOB_NOT_FOUND" => {
                NodeClientErrorKind::NotFound
            }
            "BLOB_UPLOAD_CONFLICT" | "CHUNK_CONFLICT" | "UPLOAD_EXPIRED" | "CHUNK_OUT_OF_ORDER"
            | "MISSING_CHUNKS" | "MANIFEST_MISMATCH" => NodeClientErrorKind::Conflict,
            "DEVICE_LIMIT_EXCEEDED" | "GROUP_CAPACITY_EXCEEDED" | "QUOTA_EXCEEDED"
            | "RATE_LIMITED" | "CHALLENGE_CAPACITY_EXCEEDED" | "PAYLOAD_TOO_LARGE"
            | "CONTENT_TOO_LARGE" => NodeClientErrorKind::Limit,
            "INVALID_REQUEST" | "UNSUPPORTED_SCHEMA_VERSION" | "INVALID_ACK_BATCH"
            | "INVALID_CURSOR" => NodeClientErrorKind::InvalidRequest,
            "NODE_ID_MISMATCH" => NodeClientErrorKind::Protocol,
            _ => NodeClientErrorKind::Internal,
        }
    }

    fn detail_for_kind(kind: NodeClientErrorKind) -> &'static str {
        match kind {
            NodeClientErrorKind::Network => "node connection failed",
            NodeClientErrorKind::Timeout => "node request timed out",
            NodeClientErrorKind::Protocol => "node response does not match the protocol",
            NodeClientErrorKind::Authentication => "node rejected the authentication",
            NodeClientErrorKind::Permission => "node denied the operation",
            NodeClientErrorKind::Conflict => "node reported a conflict",
            NodeClientErrorKind::NotFound => "node does not know the target",
            NodeClientErrorKind::InvalidRequest => "node rejected the request shape",
            NodeClientErrorKind::Limit => "node limit or quota reached",
            NodeClientErrorKind::Internal => "node internal failure",
            NodeClientErrorKind::Config => "client configuration is not valid",
        }
    }

    /// 节点返回的稳定错误码 → 客户端错误。
    pub(crate) fn from_node_code(status: u16, node_code: &str) -> Self {
        let kind = Self::kind_for_node_code(node_code);
        Self {
            code: Self::code_for_node_code(node_code),
            kind,
            status: Some(status),
            node_code: Some(NODE_CODES
                .iter()
                .copied()
                .find(|known| *known == node_code)
                .unwrap_or("UNKNOWN_NODE_CODE")),
            detail: Self::detail_for_kind(kind),
        }
    }

    /// 响应没有可识别错误体时，按 HTTP 状态归类。
    pub(crate) fn from_status(status: u16) -> Self {
        let (code, kind) = match status {
            400 => (ErrorCode::MalformedFrame, NodeClientErrorKind::InvalidRequest),
            401 => (ErrorCode::InvalidCredential, NodeClientErrorKind::Authentication),
            403 => (ErrorCode::PermissionDenied, NodeClientErrorKind::Permission),
            404 => (ErrorCode::InvalidTarget, NodeClientErrorKind::NotFound),
            409 => (ErrorCode::LogConflict, NodeClientErrorKind::Conflict),
            413 => (ErrorCode::ContentTooLarge, NodeClientErrorKind::Limit),
            429 => (ErrorCode::RateLimited, NodeClientErrorKind::Limit),
            500 => (ErrorCode::InternalFailure, NodeClientErrorKind::Internal),
            502 | 503 | 504 => (ErrorCode::NodeUnavailable, NodeClientErrorKind::Network),
            _ => (ErrorCode::UnknownResult, NodeClientErrorKind::Protocol),
        };
        Self {
            code,
            kind,
            status: Some(status),
            node_code: None,
            detail: Self::detail_for_kind(kind),
        }
    }

    /// 本地校验失败（节点侧 NodeError 复用同一套码串）。
    pub(crate) fn from_local_node_error(error: &chat_node_services::error::NodeError) -> Self {
        let node_code = error.code();
        let kind = Self::kind_for_node_code(node_code);
        Self {
            code: Self::code_for_node_code(node_code),
            kind,
            status: None,
            node_code: None,
            detail: "local request validation failed",
        }
    }
}

/// 节点侧可能的稳定码字符串（用于把未知码显式标记为 UNKNOWN_NODE_CODE）。
const NODE_CODES: [&str; 40] = [
    "INVALID_REQUEST",
    "PAYLOAD_TOO_LARGE",
    "INVALID_CHALLENGE",
    "NODE_ID_MISMATCH",
    "AUTH_EXPIRED",
    "AUTH_REQUIRED",
    "AUTH_REPLAY",
    "INVALID_SIGNATURE",
    "INVALID_CREDENTIAL",
    "DEVICE_REVOKED",
    "DEVICE_LIMIT_EXCEEDED",
    "INVALID_CAPABILITY",
    "PERMISSION_DENIED",
    "CHALLENGE_CAPACITY_EXCEEDED",
    "RATE_LIMITED",
    "MIGRATION_FAILED",
    "NODE_IDENTITY_MISMATCH",
    "INVALID_CONFIG",
    "STORAGE_FAILURE",
    "ENCODING_FAILURE",
    "GROUP_NOT_FOUND",
    "GROUP_ID_CONFLICT",
    "GROUP_CAPACITY_EXCEEDED",
    "SUBMISSION_ID_CONFLICT",
    "STALE_CONTROL_HEAD",
    "STALE_EPOCH",
    "HISTORY_NOT_AUTHORIZED",
    "INVALID_CURSOR",
    "QUOTA_EXCEEDED",
    "ITEM_ID_CONFLICT",
    "INVALID_ACK_BATCH",
    "BLOB_UPLOAD_CONFLICT",
    "UPLOAD_NOT_FOUND",
    "UPLOAD_EXPIRED",
    "CHUNK_CONFLICT",
    "CHUNK_OUT_OF_ORDER",
    "MISSING_CHUNKS",
    "MANIFEST_MISMATCH",
    "BLOB_NOT_FOUND",
    "CONTENT_TOO_LARGE",
];

impl fmt::Display for NodeClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} ({}): {}",
            self.code.as_str(),
            self.kind.as_str(),
            self.detail
        )?;
        if let Some(status) = self.status {
            write!(formatter, " [http {status}]")?;
        }
        if let Some(node_code) = self.node_code {
            write!(formatter, " [node code {node_code}]")?;
        }
        Ok(())
    }
}

impl std::error::Error for NodeClientError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_known_node_code_maps_to_a_frozen_code() {
        for node_code in NODE_CODES {
            let mapped = NodeClientError::code_for_node_code(node_code);
            assert_ne!(
                mapped,
                ErrorCode::UnknownResult,
                "node code {node_code} must map to a PROTOCOL-014 code"
            );
            // 码表里的字符串必须与 PROTOCOL-014 的冻结字符串风格一致（大写 + 下划线）。
            assert!(node_code.bytes().all(|byte| byte.is_ascii_uppercase() || byte == b'_'));
        }
    }

    #[test]
    fn unknown_node_code_is_explicit_and_not_retryable() {
        let error = NodeClientError::from_node_code(500, "SOMETHING_NEW");
        assert_eq!(error.code(), ErrorCode::UnknownResult);
        assert_eq!(error.node_code(), Some("UNKNOWN_NODE_CODE"));
        assert!(!error.is_retryable());
        assert_eq!(error.http_status(), Some(500));
    }

    #[test]
    fn classification_and_retryability_are_stable() {
        let auth = NodeClientError::from_node_code(401, "INVALID_SIGNATURE");
        assert_eq!(auth.kind(), NodeClientErrorKind::Authentication);
        assert_eq!(auth.code(), ErrorCode::InvalidSignature);
        assert!(!auth.is_retryable());

        let stale = NodeClientError::from_node_code(409, "STALE_CONTROL_HEAD");
        assert_eq!(stale.kind(), NodeClientErrorKind::Conflict);
        assert!(stale.is_retryable(), "stale head tells the client to resync");

        let denied = NodeClientError::from_node_code(403, "PERMISSION_DENIED");
        assert_eq!(denied.kind(), NodeClientErrorKind::Permission);
        assert!(!denied.is_retryable());

        // 没有可识别错误体时按状态归类，仍然是显式失败。
        assert_eq!(
            NodeClientError::from_status(413).code(),
            ErrorCode::ContentTooLarge
        );
        assert_eq!(
            NodeClientError::from_status(503).kind(),
            NodeClientErrorKind::Network
        );
        assert_eq!(
            NodeClientError::from_status(418).code(),
            ErrorCode::UnknownResult
        );
    }

    #[test]
    fn display_has_no_body_or_secret_material() {
        let error = NodeClientError::from_node_code(401, "AUTH_REPLAY");
        let text = error.to_string();
        assert!(text.contains("INVALID_CREDENTIAL"));
        assert!(text.contains("authentication"));
        assert!(text.contains("AUTH_REPLAY"));
        assert!(text.contains("http 401"));
    }
}
