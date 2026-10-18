//! 版本化 HTTP DTO。
//!
//! 请求与响应都带 schema_version；未知版本显式 400 拒绝，不做“尽力兼容”。
//! 传输字段名固定为 snake_case，与签名原文分开（签名原文见 node-services 的规范编码）。
use axum::http::HeaderMap;
use chat_node_services::auth::{AuthChallenge, ChallengePurpose, SignedAuth};
use chat_node_services::capability::{CapabilityGrant, CapabilityScope};
use chat_node_services::group::CandidateDomainOperation;
use chat_node_services::blob::{
    BlobReceipt, ChunkReceipt, CipherManifest, PublicBlobDescriptor, UploadSession,
};
use chat_node_services::canonical::validate_bounded_id;
use chat_node_services::group::{
    AppendResult, GroupCandidate, GroupCreation, GroupRegistration, LogPage, SignedCandidate,
    SignedGroupCreation, SignedLogHead,
};
use chat_node_services::limits::{MAX_CHALLENGE_ID_BYTES, MAX_DEVICE_ID_BYTES};
use chat_node_services::mailbox::{AckSummary, MailboxItem, MailboxPage, MailboxReceipt};
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

/// POST /v1/groups 请求（NODE-005）。
#[derive(Clone, Debug, Deserialize)]
pub struct RegisterGroupRequestDto {
    #[serde(default = "default_schema_version")]
    pub schema_version: u16,
    pub creation: GroupCreation,
    /// 创建设备对创建原文的 Ed25519 签名。
    pub device_signature: String,
}

impl RegisterGroupRequestDto {
    /// 校验版本并转换为服务层请求。
    pub fn into_signed(self) -> Result<SignedGroupCreation, ApiError> {
        check_schema_version(self.schema_version)?;
        Ok(SignedGroupCreation {
            creation: self.creation,
            device_signature: self.device_signature,
        })
    }
}

/// POST /v1/groups 响应。
#[derive(Clone, Debug, Serialize)]
pub struct GroupRegistrationResponseDto {
    pub schema_version: u16,
    pub registration: GroupRegistration,
}

impl GroupRegistrationResponseDto {
    /// 包装节点签名登记回执。
    pub fn new(registration: GroupRegistration) -> Self {
        Self {
            schema_version: DTO_SCHEMA_VERSION,
            registration,
        }
    }
}

/// POST /v1/groups/{id}/entries 请求（NODE-006）。
#[derive(Clone, Debug, Deserialize)]
pub struct AppendGroupEntryRequestDto {
    #[serde(default = "default_schema_version")]
    pub schema_version: u16,
    pub candidate: GroupCandidate,
    /// 作者设备对 PROTOCOL-004 签名原文的 Ed25519 签名。
    pub author_signature: String,
    /// 可选的领域操作声明（PROTOCOL-004 字段 55/56）。
    ///
    /// 声明参与作者签名原文：客户端必须**先按含声明的形态**算签名再提交，否则 `INVALID_SIGNATURE`。
    /// 不传 = 旧客户端行为（字节不变）。
    #[serde(default, alias = "domainOperation")]
    pub domain_operation: Option<CandidateDomainOperation>,
}

impl AppendGroupEntryRequestDto {
    /// 校验版本并转换为服务层候选（含可选的领域操作声明）。
    pub fn into_request(self) -> Result<(SignedCandidate, Option<CandidateDomainOperation>), ApiError> {
        check_schema_version(self.schema_version)?;
        Ok((
            SignedCandidate {
                candidate: self.candidate,
                author_signature: self.author_signature,
            },
            self.domain_operation,
        ))
    }
}

/// POST /v1/groups/{id}/entries 响应。
#[derive(Clone, Debug, Serialize)]
pub struct AppendGroupEntryResponseDto {
    pub schema_version: u16,
    pub result: AppendResult,
}

impl AppendGroupEntryResponseDto {
    /// 包装节点签名回执。
    pub fn new(result: AppendResult) -> Self {
        Self {
            schema_version: DTO_SCHEMA_VERSION,
            result,
        }
    }
}

/// GET /v1/groups/{id}/log 响应（NODE-007）。
#[derive(Clone, Debug, Serialize)]
pub struct GroupLogResponseDto {
    pub schema_version: u16,
    pub page: LogPage,
}

impl GroupLogResponseDto {
    /// 包装一页日志。
    pub fn new(page: LogPage) -> Self {
        Self {
            schema_version: DTO_SCHEMA_VERSION,
            page,
        }
    }
}

/// GET /v1/groups/{id}/head 响应（NODE-008）。
#[derive(Clone, Debug, Serialize)]
pub struct GroupHeadResponseDto {
    pub schema_version: u16,
    pub head: SignedLogHead,
}

impl GroupHeadResponseDto {
    /// 包装签名 head。
    pub fn new(head: SignedLogHead) -> Self {
        Self {
            schema_version: DTO_SCHEMA_VERSION,
            head,
        }
    }
}

/// POST /v1/mailbox/items 请求（NODE-009）。
#[derive(Clone, Debug, Deserialize)]
pub struct StoreMailboxItemRequestDto {
    #[serde(default = "default_schema_version")]
    pub schema_version: u16,
    pub item: MailboxItem,
}

impl StoreMailboxItemRequestDto {
    /// 校验版本并取出条目。
    pub fn into_item(self) -> Result<MailboxItem, ApiError> {
        check_schema_version(self.schema_version)?;
        Ok(self.item)
    }
}

/// POST /v1/mailbox/items 响应。
#[derive(Clone, Debug, Serialize)]
pub struct MailboxReceiptResponseDto {
    pub schema_version: u16,
    pub receipt: MailboxReceipt,
}

impl MailboxReceiptResponseDto {
    /// 包装节点签名存储回执。
    pub fn new(receipt: MailboxReceipt) -> Self {
        Self {
            schema_version: DTO_SCHEMA_VERSION,
            receipt,
        }
    }
}

/// GET /v1/mailbox 响应（NODE-010）。
#[derive(Clone, Debug, Serialize)]
pub struct MailboxPageResponseDto {
    pub schema_version: u16,
    pub page: MailboxPage,
}

impl MailboxPageResponseDto {
    /// 包装一页信箱数据。
    pub fn new(page: MailboxPage) -> Self {
        Self {
            schema_version: DTO_SCHEMA_VERSION,
            page,
        }
    }
}

/// POST /v1/mailbox/ack 请求（NODE-011）。
#[derive(Clone, Debug, Deserialize)]
pub struct AckMailboxItemsRequestDto {
    #[serde(default = "default_schema_version")]
    pub schema_version: u16,
    pub item_ids: Vec<String>,
}

impl AckMailboxItemsRequestDto {
    /// 校验版本并取出批量标识。
    pub fn into_ids(self) -> Result<Vec<String>, ApiError> {
        check_schema_version(self.schema_version)?;
        Ok(self.item_ids)
    }
}

/// POST /v1/mailbox/ack 响应。
#[derive(Clone, Debug, Serialize)]
pub struct AckMailboxItemsResponseDto {
    pub schema_version: u16,
    pub summary: AckSummary,
}

impl AckMailboxItemsResponseDto {
    /// 包装 ACK 结果。
    pub fn new(summary: AckSummary) -> Self {
        Self {
            schema_version: DTO_SCHEMA_VERSION,
            summary,
        }
    }
}

/// POST /v1/blobs 请求（NODE-012）。
#[derive(Clone, Debug, Deserialize)]
pub struct BeginBlobUploadRequestDto {
    #[serde(default = "default_schema_version")]
    pub schema_version: u16,
    /// 客户端业务幂等键（PROTOCOL-001 operation 文本）。
    pub operation_id: String,
    pub descriptor: PublicBlobDescriptor,
}

impl BeginBlobUploadRequestDto {
    /// 校验版本并取出 (operation, descriptor)。
    pub fn into_request(self) -> Result<(String, PublicBlobDescriptor), ApiError> {
        check_schema_version(self.schema_version)?;
        Ok((self.operation_id, self.descriptor))
    }
}

/// POST /v1/blobs 响应：节点签名的上传会话。
#[derive(Clone, Debug, Serialize)]
pub struct UploadSessionResponseDto {
    pub schema_version: u16,
    pub session: UploadSession,
}

impl UploadSessionResponseDto {
    /// 包装上传会话。
    pub fn new(session: UploadSession) -> Self {
        Self {
            schema_version: DTO_SCHEMA_VERSION,
            session,
        }
    }
}

/// PUT /v1/blobs/{id}/chunks/{index} 响应：分块回执。
#[derive(Clone, Debug, Serialize)]
pub struct ChunkReceiptResponseDto {
    pub schema_version: u16,
    pub receipt: ChunkReceipt,
}

impl ChunkReceiptResponseDto {
    /// 包装分块回执。
    pub fn new(receipt: ChunkReceipt) -> Self {
        Self {
            schema_version: DTO_SCHEMA_VERSION,
            receipt,
        }
    }
}

/// POST /v1/blobs/{id}/complete 请求（NODE-014）。
#[derive(Clone, Debug, Deserialize)]
pub struct CompleteBlobUploadRequestDto {
    #[serde(default = "default_schema_version")]
    pub schema_version: u16,
    pub manifest: CipherManifest,
}

impl CompleteBlobUploadRequestDto {
    /// 校验版本并取出清单。
    pub fn into_manifest(self) -> Result<CipherManifest, ApiError> {
        check_schema_version(self.schema_version)?;
        Ok(self.manifest)
    }
}

/// POST /v1/blobs/{id}/complete 响应：发布回执。
#[derive(Clone, Debug, Serialize)]
pub struct BlobReceiptResponseDto {
    pub schema_version: u16,
    pub receipt: BlobReceipt,
}

impl BlobReceiptResponseDto {
    /// 包装发布回执。
    pub fn new(receipt: BlobReceipt) -> Self {
        Self {
            schema_version: DTO_SCHEMA_VERSION,
            receipt,
        }
    }
}

/// 把路径里的块序号解析成 u32；非数字一律按请求错误拒绝（不走 axum 默认错误体）。
pub fn parse_chunk_index(raw: &str) -> Result<u32, ApiError> {
    raw.parse::<u32>()
        .map_err(|_| ApiError::invalid_request("chunk index must be an unsigned integer"))
}
/// GET /v1/health 响应：只含公开身份与 API schema 版本。
#[derive(Clone, Debug, Serialize)]
pub struct HealthResponseDto {
    pub schema_version: u16,
    /// 固定 `ok`：能返回该结构就说明服务进程可应答。
    pub status: &'static str,
    pub node_id: String,
    /// 节点签名公钥（公开材料，客户端用它固定节点身份）。
    pub node_public_key: String,
}

impl HealthResponseDto {
    /// 包装节点公开身份。
    pub fn new(node_id: String, node_public_key: String) -> Self {
        Self {
            schema_version: DTO_SCHEMA_VERSION,
            status: "ok",
            node_id,
            node_public_key,
        }
    }
}
/// POST /v1/capabilities 请求（API-004）。
#[derive(Clone, Debug, Deserialize)]
pub struct IssueCapabilityRequestDto {
    #[serde(default = "default_schema_version")]
    pub schema_version: u16,
    /// 被授权的对象（上传会话 id 或已发布 blob id）。
    pub object_id: String,
    /// 用途文本（`blob:upload` / `blob:download`）。
    pub scope: String,
    /// 有效期（秒）；缺省用节点默认值。
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
}

impl IssueCapabilityRequestDto {
    /// 校验版本并解出 (对象, 用途, 有效期)。
    pub fn into_parts(self) -> Result<(String, CapabilityScope, u64), ApiError> {
        check_schema_version(self.schema_version)?;
        let scope = CapabilityScope::parse(&self.scope).ok_or_else(|| {
            ApiError::invalid_request("capability scope must be blob:upload or blob:download")
        })?;
        let ttl_seconds = self
            .ttl_seconds
            .unwrap_or(chat_node_services::capability::DEFAULT_CAPABILITY_TTL_SECONDS);
        if ttl_seconds == 0 || ttl_seconds > chat_node_services::capability::MAX_CAPABILITY_TTL_SECONDS
        {
            return Err(ApiError::invalid_request("capability ttl is out of range"));
        }
        Ok((self.object_id, scope, ttl_seconds))
    }
}

/// POST /v1/capabilities 响应：节点签名的能力令牌（公开材料，不含密钥）。
#[derive(Clone, Debug, Serialize)]
pub struct CapabilityResponseDto {
    pub schema_version: u16,
    pub capability: CapabilityDto,
}

/// 令牌与其公开绑定字段。
#[derive(Clone, Debug, Serialize)]
pub struct CapabilityDto {
    pub token: String,
    pub object_id: String,
    pub device_id: String,
    pub scope: &'static str,
    pub issued_at: u64,
    pub expires_at: u64,
}

impl CapabilityResponseDto {
    /// 包装已签发的令牌。
    pub fn new(token: String, grant: CapabilityGrant) -> Self {
        Self {
            schema_version: DTO_SCHEMA_VERSION,
            capability: CapabilityDto {
                token,
                object_id: grant.object_id,
                device_id: grant.device_id,
                scope: grant.scope.as_str(),
                issued_at: grant.issued_at,
                expires_at: grant.expires_at,
            },
        }
    }
}
