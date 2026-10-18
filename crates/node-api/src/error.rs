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
}

/// HTTP 适配错误。
#[derive(Clone, Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

impl ApiError {
    /// 构造一个固定错误。
    pub fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
        }
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
            NodeError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            NodeError::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            NodeError::InvalidChallenge
            | NodeError::NodeIdMismatch
            | NodeError::AuthExpired
            | NodeError::AuthReplay
            | NodeError::InvalidSignature
            | NodeError::InvalidCredential => StatusCode::UNAUTHORIZED,
            NodeError::DeviceRevoked
            | NodeError::InvalidCapability
            | NodeError::PermissionDenied => StatusCode::FORBIDDEN,
            NodeError::DeviceLimitExceeded => StatusCode::CONFLICT,
            NodeError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            NodeError::ChallengeCapacityExceeded
            | NodeError::StorageUnavailable
            | NodeError::Storage(_) => StatusCode::SERVICE_UNAVAILABLE,
            NodeError::MigrationFailed(_)
            | NodeError::NodeIdentityMismatch
            | NodeError::InvalidConfig(_)
            | NodeError::Encoding(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            code: error.code(),
            message: error.safe_message(),
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
            },
        };
        (self.status, Json(body)).into_response()
    }
}
