//! 稳定错误结构与 HTTP 状态映射。
//!
//! 每个 NodeError 变体只有一个映射，响应体只含稳定错误码与固定短说明，
//! 不回显正文、密钥、SQL 或内部细节。
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chat_node_services::error::NodeError;
use serde::Serialize;

use crate::dto::DTO_SCHEMA_VERSION;

/// 版本化错误响应体。
#[derive(Clone, Debug, Serialize)]
pub struct ErrorResponseDto {
    pub schema_version: u16,
    pub error: ErrorDetailDto,
}

/// 错误详情。
#[derive(Clone, Debug, Serialize)]
pub struct ErrorDetailDto {
    pub code: &'static str,
    pub message: &'static str,
    /// 建议的重试等待（毫秒）；只有限流等“稍后再来”的错误会带这个字段。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

/// HTTP 适配错误。
#[derive(Clone, Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    retry_after_ms: Option<u64>,
}

impl ApiError {
    /// 构造一个固定错误。
    pub fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
            retry_after_ms: None,
        }
    }

    /// 建议的重试等待（毫秒），只用于 429 之类的“稍后再来”错误。
    pub fn retry_after_ms(&self) -> Option<u64> {
        self.retry_after_ms
    }

    /// 请求形状不合法。
    pub fn invalid_request(message: &'static str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "INVALID_REQUEST", message)
    }

    /// 请求正文超过节点上限。
    pub fn payload_too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "PAYLOAD_TOO_LARGE",
            "request body exceeds the node limit",
        )
    }

    /// schema 版本不被接受。
    pub fn unsupported_schema_version() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "UNSUPPORTED_SCHEMA_VERSION",
            "unsupported dto schema version",
        )
    }

    /// 缺失或非法鉴权头。
    pub fn missing_auth_header() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "missing or invalid authentication header",
        )
    }

    /// 把服务错误映射为 HTTP 错误。
    pub fn from_node(error: NodeError) -> Self {
        let status = match &error {
            NodeError::InvalidRequest(_)
            | NodeError::InvalidCandidate
            | NodeError::InvalidCursor
            | NodeError::InvalidAckBatch => StatusCode::BAD_REQUEST,
            NodeError::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            NodeError::InvalidChallenge
            | NodeError::NodeIdMismatch
            | NodeError::AuthExpired
            | NodeError::AuthReplay
            | NodeError::AuthRequired
            | NodeError::InvalidSignature
            | NodeError::InvalidCredential => StatusCode::UNAUTHORIZED,
            NodeError::DeviceRevoked
            | NodeError::InvalidCapability
            // CAP-REV：撤销后的令牌立即失效（403，与其它凭据类拒绝一致）。
            | NodeError::CapabilityRevoked
            | NodeError::PermissionDenied
            | NodeError::HistoryNotAuthorized => StatusCode::FORBIDDEN,
            NodeError::GroupNotFound
            | NodeError::UploadNotFound
            | NodeError::CapabilityTokenNotFound
            | NodeError::BlobNotFound => StatusCode::NOT_FOUND,
            NodeError::UploadExpired => StatusCode::GONE,
            NodeError::ContentTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            NodeError::DeviceLimitExceeded
            | NodeError::GroupIdConflict
            | NodeError::GroupCapacityExceeded
            | NodeError::SubmissionIdConflict
            | NodeError::StaleControlHead
            | NodeError::StaleEpoch
            | NodeError::QuotaExceeded
            | NodeError::ItemIdConflict
            | NodeError::BlobUploadConflict
            | NodeError::ChunkConflict
            | NodeError::ChunkOutOfOrder
            | NodeError::MissingChunks
            | NodeError::ManifestMismatch => StatusCode::CONFLICT,
            NodeError::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            NodeError::ChallengeCapacityExceeded
            | NodeError::StorageUnavailable
            | NodeError::Storage(_) => StatusCode::SERVICE_UNAVAILABLE,
            NodeError::MigrationFailed(_)
            | NodeError::NodeIdentityMismatch
            | NodeError::InvalidConfig(_)
            | NodeError::Encoding(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let retry_after_ms = match &error {
            NodeError::RateLimited { retry_after_ms } => Some(*retry_after_ms),
            _ => None,
        };
        Self {
            status,
            code: error.code(),
            message: error.safe_message(),
            retry_after_ms,
        }
    }

    /// 稳定错误码。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// HTTP 状态。
    pub fn status(&self) -> StatusCode {
        self.status
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorResponseDto {
            schema_version: DTO_SCHEMA_VERSION,
            error: ErrorDetailDto {
                code: self.code,
                message: self.message,
                retry_after_ms: self.retry_after_ms,
            },
        };
        let mut response = (self.status, Json(body)).into_response();
        // HTTP 语义里的 Retry-After 以秒为单位（向上取整，至少 1 秒）。
        if let Some(retry_after_ms) = self.retry_after_ms {
            let seconds = retry_after_ms.div_ceil(1000).max(1);
            if let Ok(value) = axum::http::HeaderValue::from_str(&seconds.to_string()) {
                response
                    .headers_mut()
                    .insert(axum::http::header::RETRY_AFTER, value);
            }
        }
        response
    }
}
