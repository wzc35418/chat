//! 版本化 HTTP DTO。
//!
//! 请求与响应都带 schema_version；未知版本显式 400 拒绝，不做“尽力兼容”。
//! 传输字段名固定为 snake_case，与签名原文分开（签名原文见 node-services 的规范编码）。
use axum::http::HeaderMap;
use chat_node_services::auth::{AuthChallenge, ChallengePurpose, SignedAuth};
use chat_node_services::canonical::validate_bounded_id;
use chat_node_services::limits::{
    MAX_CHALLENGE_ID_BYTES, MAX_DEVICE_ID_BYTES,
};
use chat_node_services::registration::{
    DeviceCredential, DeviceRegistration, RegisterDeviceRequest, RegistrationCapability,
};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;

/// 本 crate 唯一接受的 DTO 版本。
pub const DTO_SCHEMA_VERSION: u16 = 1;

/// 设备标识请求头。
pub const HEADER_DEVICE_ID: &str = "x-chat-device-id";
/// 挑战标识请求头。
pub const HEADER_CHALLENGE_ID: &str = "x-chat-challenge-id";
/// 挑战 nonce 请求头（小写十六进制 32 字节）。
pub const HEADER_NONCE: &str = "x-chat-challenge-nonce";
/// 设备签名请求头（小写十六进制 64 字节）。
pub const HEADER_SIGNATURE: &str = "x-chat-signature";

fn default_schema_version() -> u16 {
    DTO_SCHEMA_VERSION
}

fn check_schema_version(version: u16) -> Result<(), ApiError> {
    if version == DTO_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(ApiError::unsupported_schema_version())
    }
}

/// POST /v1/auth/challenge 请求。
#[derive(Clone, Debug, Deserialize)]
pub struct ChallengeRequestDto {
    #[serde(default = "default_schema_version")]
    pub schema_version: u16,
    /// request_auth 或 device_registration。
    pub purpose: String,
}

impl ChallengeRequestDto {
    /// 校验版本并解析用途。
    pub fn purpose(&self) -> Result<ChallengePurpose, ApiError> {
        check_schema_version(self.schema_version)?;
        ChallengePurpose::parse(&self.purpose)
            .map_err(|_| ApiError::invalid_request("unknown challenge purpose"))
    }
}

/// POST /v1/auth/challenge 响应。
#[derive(Clone, Debug, Serialize)]
pub struct ChallengeResponseDto {
    pub schema_version: u16,
    pub challenge: AuthChallenge,
}

impl ChallengeResponseDto {
    /// 包装节点签发的挑战。
    pub fn new(challenge: AuthChallenge) -> Self {
        Self {
            schema_version: DTO_SCHEMA_VERSION,
            challenge,
        }
    }
}

/// POST /v1/devices 请求。
#[derive(Clone, Debug, Deserialize)]
pub struct RegisterDeviceRequestDto {
    #[serde(default = "default_schema_version")]
    pub schema_version: u16,
    pub account_id: String,
    pub credential: DeviceCredential,
    pub capability: RegistrationCapability,
}

impl RegisterDeviceRequestDto {
    /// 校验版本并转换为服务层请求。
    pub fn into_request(self) -> Result<RegisterDeviceRequest, ApiError> {
        check_schema_version(self.schema_version)?;
        Ok(RegisterDeviceRequest {
            account_id: self.account_id,
            credential: self.credential,
            capability: self.capability,
        })
    }
}

/// POST /v1/devices 响应。
#[derive(Clone, Debug, Serialize)]
pub struct DeviceRegistrationResponseDto {
    pub schema_version: u16,
    pub registration: DeviceRegistration,
}

impl DeviceRegistrationResponseDto {
    /// 包装节点注册回执。
    pub fn new(registration: DeviceRegistration) -> Self {
        Self {
            schema_version: DTO_SCHEMA_VERSION,
            registration,
        }
    }
}

/// 从请求头解析出的签名鉴权材料。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedAuthHeaders {
    pub device_id: String,
    pub challenge_id: String,
    pub nonce: String,
    pub signature: String,
}

impl SignedAuthHeaders {
    /// 读取四个鉴权头；缺失或非 ASCII 一律拒绝。
    pub fn from_headers(headers: &HeaderMap) -> Result<Self, ApiError> {
        let read = |name: &'static str| -> Result<String, ApiError> {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned)
                .ok_or_else(ApiError::missing_auth_header)
        };
        let device_id = read(HEADER_DEVICE_ID)?;
        let challenge_id = read(HEADER_CHALLENGE_ID)?;
        let nonce = read(HEADER_NONCE)?;
        let signature = read(HEADER_SIGNATURE)?;
        validate_bounded_id("device id", &device_id, MAX_DEVICE_ID_BYTES)
            .map_err(ApiError::from_node)?;
        validate_bounded_id("challenge id", &challenge_id, MAX_CHALLENGE_ID_BYTES)
            .map_err(ApiError::from_node)?;
        Ok(Self {
            device_id,
            challenge_id,
            nonce,
            signature,
        })
    }

    /// 转换为服务层签名材料。
    pub fn to_signed_auth(&self) -> SignedAuth {
        SignedAuth {
            challenge_id: self.challenge_id.clone(),
            device_id: self.device_id.clone(),
            nonce: self.nonce.clone(),
            signature: self.signature.clone(),
        }
    }
}
