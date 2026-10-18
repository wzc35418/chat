//! axum 路由与端点适配（API-006 风格的单一适配层）。
//!
//! 每个 handler 只做：限制正文、解码 DTO、鉴权、调用服务、映射结果。
//! 领域规则与事务全部在 crates/node-services；本层不重复实现，也不放宽任何安全条件。
use axum::body::Bytes;
use axum::{Json, Router};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use chat_node_services::auth::{ChallengePurpose, RequestMeta};
use chat_node_services::canonical::sha256;
use chat_node_services::limits::MAX_HTTP_BODY_BYTES;
use serde::de::DeserializeOwned;

use crate::dto::{
    ChallengeRequestDto, ChallengeResponseDto, DeviceRegistrationResponseDto,
    RegisterDeviceRequestDto, SignedAuthHeaders,
};
use crate::error::ApiError;
use crate::state::AppState;

/// 构造节点 HTTP 路由。
///
/// 公开端点只有挑战签发；authenticated 端点目前是设备注册。
/// 逐 IP 限流（API-005）与事件套接字（API-009）尚未在本阶段实现。
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/v1/auth/challenge", post(issue_challenge))
        .route("/v1/devices", post(register_device))
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .with_state(state)
}

fn ensure_body_bounded(state: &AppState, length: usize) -> Result<(), ApiError> {
    if length > state.max_request_body_bytes() {
        return Err(ApiError::payload_too_large());
    }
    Ok(())
}

fn parse_body<T: DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body)
        .map_err(|_| ApiError::invalid_request("request body is not valid json for this endpoint"))
}

/// POST /v1/auth/challenge：签发一次性短期挑战。
pub async fn issue_challenge(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, ApiError> {
    ensure_body_bounded(&state, body.len())?;
    let request: ChallengeRequestDto = parse_body(&body)?;
    let purpose = request.purpose()?;
    let now = state.now();
    let challenge =
        state.with_service(|service| service.issue_auth_challenge(purpose, now))?;
    Ok(Json(ChallengeResponseDto::new(challenge)).into_response())
}

/// POST /v1/devices：设备证明持有私钥后幂等注册。
///
/// 顺序：限长 -> 解码 DTO -> 用凭据中的设备公钥验证请求签名（消费挑战）-> 注册。
/// 请求签名绑定实际请求的 method/path/query 与正文摘要，换方法、换路径、换正文、
/// 换节点或重放同一挑战都会在鉴权阶段失败。
pub async fn register_device(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    ensure_body_bounded(&state, body.len())?;
    let request: RegisterDeviceRequestDto = parse_body(&body)?;
    let request = request.into_request()?;
    let auth_headers = SignedAuthHeaders::from_headers(&headers)?;
    let device_public_key = request
        .credential
        .device_key()
        .map_err(ApiError::from_node)?;
    let meta = RequestMeta::canonical("POST", uri.path(), uri.query().unwrap_or(""))
        .map_err(ApiError::from_node)?;
    let body_hash = sha256(&body);
    let signed_auth = auth_headers.to_signed_auth();
    let now = state.now();
    let proof = state.with_service(|service| {
        service.verify_device_signature(
            &meta,
            &body_hash,
            &signed_auth,
            &device_public_key,
            ChallengePurpose::DeviceRegistration,
            now,
        )
    })?;
    let registration = state.with_service(|service| service.register_device(&request, &proof, now))?;
    Ok(Json(DeviceRegistrationResponseDto::new(registration)).into_response())
}
