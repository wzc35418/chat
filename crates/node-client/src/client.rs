//! 节点 HTTP 客户端：一次性挑战 + 设备签名 + 有界收发 + 稳定错误映射。
//!
//! 分层：
//!
//! - 匿名层：[`NodeClient::fetch_challenge`]（POST /v1/auth/challenge）；
//! - 签名层：[`NodeClient::send_with_challenge`]（调用方自带挑战，只发一次）；
//! - 业务层：register_device / create_group / append_entry / fetch_log / get_head /
//!   store_mailbox_item / read_mailbox_page / ack_mailbox_items。
//!
//! 业务层每次调用都**重新取一个一次性挑战**（用后即弃），只在“挑战本身不可用”（过期/未知/
//! 重放）或传输失败时重取一次再试；不会无限重试，也不会把一次失败包装成成功。
//!
//! 签名原文完全复用节点侧实现（[`request_signing_bytes`] + [`RequestMeta::canonical`]），
//! 因此客户端与服务端不存在第二套“canonical 规则”。
use std::time::Duration;

use chat_node_services::auth::{
    AuthChallenge, ChallengePurpose, RequestMeta, request_signing_bytes, verify_auth_challenge,
};
use chat_node_services::canonical::{
    decode_hex_bytes, hex_decode, hex_encode, sha256, validate_bounded_id,
};
use chat_node_services::blob::{
    BlobReceipt, ChunkReceipt, CipherManifest, PublicBlobDescriptor, UploadSession,
};
use chat_node_services::error::NodeError;
use chat_node_services::group::{
    AppendResult, GroupCandidate, GroupCreation, GroupRegistration, LogPage, SignedCandidate,
    SignedGroupCreation, SignedLogHead,
};
use chat_node_services::limits::{
    MAX_BLOB_ID_BYTES, MAX_BLOB_UPLOAD_ID_BYTES, MAX_DEVICE_ID_BYTES, MAX_GROUP_ID_BYTES,
    MAX_HTTP_BODY_BYTES,
};
use chat_node_services::mailbox::{
    AckSummary, MailboxItem, MailboxPage, MailboxReceipt,
};
use chat_node_services::registration::{DeviceRegistration, RegisterDeviceRequest};
use chat_protocol::{DeviceId, ErrorCode, ObjectId, candidate_signing_bytes};
use ed25519_dalek::{Signature, Signer, SigningKey};
use reqwest::header::CONTENT_TYPE;
use reqwest::{Method, Response};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::endpoint::NodeEndpoint;
use crate::error::{NodeClientError, NodeClientErrorKind};

/// DTO schema 版本；必须与节点 [crates/node-api/src/dto.rs] 的 DTO_SCHEMA_VERSION 一致。
pub const CLIENT_SCHEMA_VERSION: u16 = 1;
/// 默认响应体上限：1 MiB（超过即显式失败，不静默截断）。
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// 默认单请求超时。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// 默认总尝试次数：首次 + 重取挑战一次。
pub const DEFAULT_CHALLENGE_ATTEMPTS: usize = 2;

/// JSON 请求正文的 content-type。
const JSON_CONTENT_TYPE: &'static str = "application/json";
/// 附件密文块的 content-type（原始字节，不做 hex/JSON 二次编码）。
const OCTET_STREAM_CONTENT_TYPE: &'static str = "application/octet-stream";

/// 鉴权头（与节点 API-003 的传输字段一致）。
pub const HEADER_DEVICE_ID: &str = "x-chat-device-id";
pub const HEADER_CHALLENGE_ID: &str = "x-chat-challenge-id";
pub const HEADER_NONCE: &str = "x-chat-challenge-nonce";
pub const HEADER_SIGNATURE: &str = "x-chat-signature";

const CHALLENGE_PATH: &str = "/v1/auth/challenge";
const DEVICES_PATH: &str = "/v1/devices";
const GROUPS_PATH: &str = "/v1/groups";
const MAILBOX_ITEMS_PATH: &str = "/v1/mailbox/items";
const MAILBOX_PATH: &str = "/v1/mailbox";
const MAILBOX_ACK_PATH: &str = "/v1/mailbox/ack";
const BLOBS_PATH: &str = "/v1/blobs";

/// 客户端上限与超时。全部在构造时校验，越界配置显式拒绝。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientLimits {
    /// 单次响应体上限（字节）。
    pub max_response_bytes: usize,
    /// 单次请求正文上限（字节）。
    pub max_request_bytes: usize,
    /// 单请求总超时。
    pub timeout: Duration,
    /// 总尝试次数（含首次）；上限 2，即“失败后最多重取一次挑战”。
    pub challenge_attempts: usize,
}

impl Default for ClientLimits {
    fn default() -> Self {
        Self {
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_request_bytes: MAX_HTTP_BODY_BYTES,
            timeout: DEFAULT_TIMEOUT,
            challenge_attempts: DEFAULT_CHALLENGE_ATTEMPTS,
        }
    }
}

impl ClientLimits {
    /// 校验上限本身；不接受 0、越界或无限重试配置。
    pub fn validate(&self) -> Result<(), NodeClientError> {
        if self.max_response_bytes == 0 || self.max_response_bytes > 64 * 1024 * 1024 {
            return Err(NodeClientError::config("max response bytes is out of range"));
        }
        if self.max_request_bytes == 0 || self.max_request_bytes > 16 * 1024 * 1024 {
            return Err(NodeClientError::config("max request bytes is out of range"));
        }
        if self.timeout < Duration::from_millis(1) || self.timeout > Duration::from_secs(300) {
            return Err(NodeClientError::config("request timeout is out of range"));
        }
        if self.challenge_attempts == 0 || self.challenge_attempts > 2 {
            // 契约要求“失败重取一次后返回稳定错误”，因此总尝试次数硬上限是 2。
            return Err(NodeClientError::config("challenge attempt budget must be 1 or 2"));
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct ChallengeBody<'a> {
    schema_version: u16,
    purpose: &'a str,
}

#[derive(Deserialize)]
struct ChallengeEnvelope {
    schema_version: u16,
    challenge: AuthChallenge,
}

#[derive(Serialize)]
struct RegisterDeviceBody<'a> {
    schema_version: u16,
    account_id: &'a str,
    credential: &'a chat_node_services::registration::DeviceCredential,
    capability: &'a chat_node_services::registration::RegistrationCapability,
}

#[derive(Deserialize)]
struct RegisterDeviceEnvelope {
    schema_version: u16,
    registration: DeviceRegistration,
}

#[derive(Serialize)]
struct GroupBody<'a> {
    schema_version: u16,
    creation: &'a GroupCreation,
    device_signature: &'a str,
}

#[derive(Deserialize)]
struct GroupEnvelope {
    schema_version: u16,
    registration: GroupRegistration,
}

#[derive(Serialize)]
struct EntryBody<'a> {
    schema_version: u16,
    candidate: &'a GroupCandidate,
    author_signature: &'a str,
}

#[derive(Deserialize)]
struct EntryEnvelope {
    schema_version: u16,
    result: AppendResult,
}

#[derive(Deserialize)]
struct LogEnvelope {
    schema_version: u16,
    page: LogPage,
}

#[derive(Deserialize)]
struct HeadEnvelope {
    schema_version: u16,
    head: SignedLogHead,
}

#[derive(Serialize)]
struct MailboxItemBody<'a> {
    schema_version: u16,
    item: &'a MailboxItem,
}

#[derive(Deserialize)]
struct MailboxReceiptEnvelope {
    schema_version: u16,
    receipt: MailboxReceipt,
}

#[derive(Deserialize)]
struct MailboxPageEnvelope {
    schema_version: u16,
    page: MailboxPage,
}

#[derive(Serialize)]
struct AckBody<'a> {
    schema_version: u16,
    item_ids: &'a [String],
}

#[derive(Deserialize)]
struct AckEnvelope {
    schema_version: u16,
    summary: AckSummary,
}

#[derive(Serialize)]
struct BeginBlobUploadBody<'a> {
    schema_version: u16,
    operation_id: &'a str,
    descriptor: &'a PublicBlobDescriptor,
}

#[derive(Deserialize)]
struct UploadSessionEnvelope {
    schema_version: u16,
    session: UploadSession,
}

#[derive(Deserialize)]
struct ChunkReceiptEnvelope {
    schema_version: u16,
    receipt: ChunkReceipt,
}

#[derive(Serialize)]
struct CompleteBlobUploadBody<'a> {
    schema_version: u16,
    manifest: &'a CipherManifest,
}

#[derive(Deserialize)]
struct BlobReceiptEnvelope {
    schema_version: u16,
    receipt: BlobReceipt,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ErrorEnvelopeDetail,
}

#[derive(Deserialize)]
struct ErrorEnvelopeDetail {
    code: String,
}

/// 带 schema 版本的响应信封；未知版本显式失败，不做“尽力兼容”。
trait Versioned {
    fn schema_version(&self) -> u16;
}

macro_rules! versioned {
    ($($name:ident),+ $(,)?) => {
        $(
            impl Versioned for $name {
                fn schema_version(&self) -> u16 {
                    self.schema_version
                }
            }
        )+
    };
}

versioned!(
    ChallengeEnvelope,
    RegisterDeviceEnvelope,
    GroupEnvelope,
    EntryEnvelope,
    LogEnvelope,
    HeadEnvelope,
    MailboxReceiptEnvelope,
    MailboxPageEnvelope,
    AckEnvelope,
    UploadSessionEnvelope,
    ChunkReceiptEnvelope,
    BlobReceiptEnvelope,
);
/// 节点客户端：固定端点 + 固定设备身份 + Ed25519 签名钥。
pub struct NodeClient {
    endpoint: NodeEndpoint,
    http: reqwest::Client,
    device_id: String,
    signing_key: SigningKey,
    limits: ClientLimits,
}

impl std::fmt::Debug for NodeClient {
    /// 只暴露端点、设备标识与上限；签名钥永不进入日志。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NodeClient")
            .field("endpoint", &self.endpoint)
            .field("device_id", &self.device_id)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

/// 只在“挑战本身不可用”或传输失败时重取一次；其余错误立即返回。
fn retryable_failure(error: &NodeClientError) -> bool {
    match error.kind() {
        NodeClientErrorKind::Network | NodeClientErrorKind::Timeout => true,
        _ => matches!(
            error.node_code(),
            Some("INVALID_CHALLENGE" | "AUTH_EXPIRED" | "AUTH_REPLAY")
        ),
    }
}

/// 查询串里的不透明 token（游标）只允许无歧义字符，避免拼出第二个参数。
fn validate_query_token(value: &str) -> Result<(), NodeClientError> {
    if value.is_empty() || value.len() > chat_protocol::MAX_CURSOR_TOKEN_BYTES {
        return Err(NodeClientError::local_invalid("query token is empty or too long"));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(NodeClientError::local_invalid(
            "query token contains unsupported characters",
        ));
    }
    Ok(())
}

impl NodeClient {
    /// 用固定端点与设备签名钥建立客户端（默认上限与超时）。
    pub fn new(
        endpoint: NodeEndpoint,
        device_id: impl Into<String>,
        signing_key: SigningKey,
    ) -> Result<Self, NodeClientError> {
        Self::with_limits(endpoint, device_id, signing_key, ClientLimits::default())
    }

    /// 用显式上限与超时建立客户端；越界配置显式拒绝。
    pub fn with_limits(
        endpoint: NodeEndpoint,
        device_id: impl Into<String>,
        signing_key: SigningKey,
        limits: ClientLimits,
    ) -> Result<Self, NodeClientError> {
        let device_id = device_id.into();
        validate_bounded_id("device id", &device_id, MAX_DEVICE_ID_BYTES)
            .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        limits.validate()?;
        let http = reqwest::Client::builder()
            .timeout(limits.timeout)
            // 不跟随重定向：跳转后的路径与签名原文不再一致，属于协议违规。
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("chat-node-client/1")
            .build()
            .map_err(|_| NodeClientError::config("http client could not be built"))?;
        Ok(Self {
            endpoint,
            http,
            device_id,
            signing_key,
            limits,
        })
    }

    /// 固定端点。
    pub fn endpoint(&self) -> &NodeEndpoint {
        &self.endpoint
    }

    /// 本客户端的设备标识。
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// 当前上限。
    pub fn limits(&self) -> &ClientLimits {
        &self.limits
    }

    /// 设备签名公钥（用于构造凭据/能力之前核对绑定）。
    pub fn device_public_key(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    /// 设备标识的协议规范文本（PROTOCOL-001）；用于准备凭据。
    pub fn protocol_device_id(&self) -> Result<String, NodeClientError> {
        DeviceId::parse(&self.device_id)
            .map(|id| id.encode())
            .map_err(|_| NodeClientError::local_invalid("device id is not a canonical protocol id"))
    }

    /// 用本设备签名钥签群创建原文（内层设备签名，与请求鉴权签名相互独立）。
    pub fn sign_group_creation(
        &self,
        creation: GroupCreation,
    ) -> Result<SignedGroupCreation, NodeClientError> {
        let bytes = creation
            .signing_bytes()
            .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        let signature: Signature = self.signing_key.sign(&bytes);
        Ok(SignedGroupCreation {
            creation,
            device_signature: hex_encode(&signature.to_bytes()),
        })
    }

    /// 用本设备签名钥签 PROTOCOL-004 候选原文（作者签名）。
    ///
    /// 同一份候选重复签名是确定性的（Ed25519 无随机量），因此“同 submission 重试”
    /// 会得到逐字节相同的正文，节点据此返回原 seq/回执。
    pub fn sign_candidate(
        &self,
        candidate: GroupCandidate,
    ) -> Result<SignedCandidate, NodeClientError> {
        let unsigned = candidate
            .to_unsigned(chat_protocol::MAX_CIPHERTEXT_BYTES)
            .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        let bytes = candidate_signing_bytes(&unsigned)
            .map_err(|_| NodeClientError::local_invalid("candidate is not a valid protocol object"))?;
        let signature: Signature = self.signing_key.sign(bytes.as_bytes());
        Ok(SignedCandidate {
            candidate,
            author_signature: hex_encode(&signature.to_bytes()),
        })
    }

    /// 取一个一次性挑战（匿名端点）。
    ///
    /// 固定了 node_public_key 时会验证节点签名；任何情况下都要求挑战的 node_id 就是
    /// 固定端点声明的节点，否则显式失败（换 URL 不会静默换节点）。
    pub async fn fetch_challenge(
        &self,
        purpose: ChallengePurpose,
    ) -> Result<AuthChallenge, NodeClientError> {
        let body = serde_json::to_vec(&ChallengeBody {
            schema_version: CLIENT_SCHEMA_VERSION,
            purpose: purpose.as_str(),
        })
        .map_err(|_| NodeClientError::local_invalid("challenge request could not be encoded"))?;
        if body.len() > self.limits.max_request_bytes {
            return Err(NodeClientError::request_too_large());
        }
        let url = self.endpoint.request_url(CHALLENGE_PATH, "")?;
        let response = self
            .http
            .post(url)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|error| NodeClientError::from_transport(&error))?;
        let status = response.status();
        let bytes = self.read_bounded(response).await?;
        if !status.is_success() {
            return Err(self.error_from_body(status.as_u16(), &bytes));
        }
        let envelope: ChallengeEnvelope = self.decode(&bytes)?;
        if envelope.challenge.purpose != purpose {
            return Err(NodeClientError::protocol(
                ErrorCode::InvalidCredential,
                "challenge purpose does not match the request",
            ));
        }
        self.ensure_node_identity(&envelope.challenge.node_id)?;
        if let Some(expected) = self.endpoint.node_public_key() {
            verify_auth_challenge(&envelope.challenge, expected).map_err(|_| {
                NodeClientError::protocol(
                    ErrorCode::InvalidSignature,
                    "challenge is not signed by the pinned node key",
                )
            })?;
        }
        Ok(envelope.challenge)
    }

    /// 低层：用调用方给的挑战签名并发送**一次**请求，返回响应体字节。
    ///
    /// 这个入口不重取挑战（调用方负责挑战生命周期），用于需要精确控制重试或需要观察
    /// “挑战一次性”语义的调用方；业务方法内部也是走它。
    pub async fn send_with_challenge(
        &self,
        challenge: &AuthChallenge,
        method: &str,
        path: &str,
        query: &str,
        body: &[u8],
    ) -> Result<Vec<u8>, NodeClientError> {
        let meta = RequestMeta::canonical(method, path, query)
            .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        if body.len() > self.limits.max_request_bytes {
            return Err(NodeClientError::request_too_large());
        }
        self.send_signed(challenge, &meta, body, &self.device_id, JSON_CONTENT_TYPE)
            .await
    }
    /// 取新挑战 → 签名 → 发送；失败按 [`retryable_failure`] 决定是否重取一次。
    async fn send_authenticated(
        &self,
        method: &str,
        path: &str,
        query: &str,
        body: &[u8],
        purpose: ChallengePurpose,
    ) -> Result<Vec<u8>, NodeClientError> {
        self.send_authenticated_for_device(method, path, query, body, purpose, None, JSON_CONTENT_TYPE)
            .await
    }

    async fn send_authenticated_for_device(
        &self,
        method: &str,
        path: &str,
        query: &str,
        body: &[u8],
        purpose: ChallengePurpose,
        signer_device_id: Option<&str>,
        content_type: &'static str,
    ) -> Result<Vec<u8>, NodeClientError> {
        let meta = RequestMeta::canonical(method, path, query)
            .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        if body.len() > self.limits.max_request_bytes {
            return Err(NodeClientError::request_too_large());
        }
        let mut attempts = 0_usize;
        loop {
            attempts += 1;
            let signer = match signer_device_id {
                Some(device_id) => device_id,
                None => self.device_id.as_str(),
            };
            match self
                .attempt_signed(&meta, body, purpose, signer, content_type)
                .await
            {
                Ok(bytes) => return Ok(bytes),
                Err(error) => {
                    if attempts >= self.limits.challenge_attempts || !retryable_failure(&error) {
                        return Err(error);
                    }
                    // 否则重取一个新挑战再试一次（总次数受 challenge_attempts 硬上限约束）。
                }
            }
        }
    }

    async fn attempt_signed(
        &self,
        meta: &RequestMeta,
        body: &[u8],
        purpose: ChallengePurpose,
        signer_device_id: &str,
        content_type: &'static str,
    ) -> Result<Vec<u8>, NodeClientError> {
        let challenge = self.fetch_challenge(purpose).await?;
        self.send_signed(&challenge, meta, body, signer_device_id, content_type)
            .await
    }

    /// 原始字节正文的已登记设备请求（附件密文块用）。
    async fn send_authenticated_raw(
        &self,
        method: &str,
        path: &str,
        query: &str,
        body: &[u8],
        content_type: &'static str,
    ) -> Result<Vec<u8>, NodeClientError> {
        self.send_authenticated_for_device(
            method,
            path,
            query,
            body,
            ChallengePurpose::RequestAuth,
            None,
            content_type,
        )
        .await
    }

    async fn send_signed(
        &self,
        challenge: &AuthChallenge,
        meta: &RequestMeta,
        body: &[u8],
        signer_device_id: &str,
        content_type: &'static str,
    ) -> Result<Vec<u8>, NodeClientError> {
        validate_bounded_id("device id", signer_device_id, MAX_DEVICE_ID_BYTES)
            .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        let nonce = hex_decode::<32>("nonce", &challenge.nonce)
            .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        let signed = request_signing_bytes(
            signer_device_id,
            meta,
            &sha256(body),
            &nonce,
            self.endpoint.node_id(),
            challenge.purpose,
        );
        let signature: Signature = self.signing_key.sign(&signed);
        let method = Method::from_bytes(meta.method.as_bytes())
            .map_err(|_| NodeClientError::local_invalid("http method is not valid"))?;
        let url = self.endpoint.request_url(&meta.path, &meta.query)?;
        let response = self
            .http
            .request(method, url)
            .header(HEADER_DEVICE_ID, signer_device_id)
            .header(HEADER_CHALLENGE_ID, challenge.challenge_id.as_str())
            .header(HEADER_NONCE, challenge.nonce.as_str())
            .header(HEADER_SIGNATURE, hex_encode(&signature.to_bytes()))
            .header(CONTENT_TYPE, content_type)
            .body(body.to_vec())
            .send()
            .await
            .map_err(|error| NodeClientError::from_transport(&error))?;
        let status = response.status();
        let bytes = self.read_bounded(response).await?;
        if status.is_success() {
            Ok(bytes)
        } else {
            Err(self.error_from_body(status.as_u16(), &bytes))
        }
    }

    /// 有界读取响应体：先看 Content-Length，再按块累计，超限立刻失败（不截断）。
    async fn read_bounded(&self, mut response: Response) -> Result<Vec<u8>, NodeClientError> {
        if let Some(length) = response.content_length() {
            if usize::try_from(length).map_or(true, |length| length > self.limits.max_response_bytes) {
                return Err(NodeClientError::response_too_large());
            }
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| NodeClientError::from_transport(&error))?
        {
            if body.len().saturating_add(chunk.len()) > self.limits.max_response_bytes {
                return Err(NodeClientError::response_too_large());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// 解出节点错误体；没有可识别错误体时按 HTTP 状态归类。
    fn error_from_body(&self, status: u16, bytes: &[u8]) -> NodeClientError {
        match serde_json::from_slice::<ErrorEnvelope>(bytes) {
            Ok(envelope) if !envelope.error.code.is_empty() => {
                NodeClientError::from_node_code(status, &envelope.error.code)
            }
            _ => NodeClientError::from_status(status),
        }
    }

    /// 解码响应并校验 schema 版本；未知版本显式失败。
    fn decode<T: DeserializeOwned + Versioned>(&self, bytes: &[u8]) -> Result<T, NodeClientError> {
        let value: T = serde_json::from_slice(bytes).map_err(|_| {
            NodeClientError::protocol(
                ErrorCode::MalformedFrame,
                "node response is not valid json for this endpoint",
            )
        })?;
        if value.schema_version() != CLIENT_SCHEMA_VERSION {
            return Err(NodeClientError::protocol(
                ErrorCode::ProtocolUnsupported,
                "node response schema version is not supported",
            ));
        }
        Ok(value)
    }

    /// 节点产物里的 node_id 必须等于固定端点的 node_id（换 URL 不换节点）。
    fn ensure_node_identity(&self, node_id: &str) -> Result<(), NodeClientError> {
        if node_id != self.endpoint.node_id() {
            return Err(NodeClientError::protocol(
                ErrorCode::InvalidTarget,
                "node output names another node than the pinned endpoint",
            ));
        }
        Ok(())
    }

    /// 固定了节点公钥时验证节点签名；未固定时只能依赖 node_id 绑定（已在文档中声明限制）。
    fn verify_node_output(
        &self,
        verify: impl FnOnce(&[u8; 32]) -> Result<(), NodeError>,
    ) -> Result<(), NodeClientError> {
        match self.endpoint.node_public_key() {
            Some(expected) => verify(expected).map_err(|_| {
                NodeClientError::protocol(
                    ErrorCode::InvalidSignature,
                    "node output is not signed by the pinned node key",
                )
            }),
            None => Ok(()),
        }
    }

    /// NODE-001：设备证明持有私钥后幂等注册（用凭据里的 device_id 与设备公钥绑定）。
    pub async fn register_device(
        &self,
        request: &RegisterDeviceRequest,
    ) -> Result<DeviceRegistration, NodeClientError> {
        request
            .validate_shape()
            .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        let device_key = request
            .credential
            .device_key()
            .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        if device_key != self.signing_key.verifying_key().to_bytes() {
            return Err(NodeClientError::config(
                "device credential does not match the client signing key",
            ));
        }
        let body = serde_json::to_vec(&RegisterDeviceBody {
            schema_version: CLIENT_SCHEMA_VERSION,
            account_id: &request.account_id,
            credential: &request.credential,
            capability: &request.capability,
        })
        .map_err(|_| NodeClientError::local_invalid("register device request could not be encoded"))?;
        let bytes = self
            .send_authenticated_for_device(
                "POST",
                DEVICES_PATH,
                "",
                &body,
                ChallengePurpose::DeviceRegistration,
                Some(request.credential.device_id.as_str()),
                JSON_CONTENT_TYPE,
            )
            .await?;
        let envelope: RegisterDeviceEnvelope = self.decode(&bytes)?;
        let registration = envelope.registration;
        self.ensure_node_identity(&registration.node_id)?;
        self.verify_node_output(|key| registration.verify(key))?;
        Ok(registration)
    }
    /// NODE-005：用本设备签名钥签创建原文并登记群。
    pub async fn create_group(
        &self,
        creation: &GroupCreation,
    ) -> Result<GroupRegistration, NodeClientError> {
        let signed = self.sign_group_creation(creation.clone())?;
        self.create_group_signed(&signed).await
    }

    /// NODE-005：提交调用方已签名的群创建请求（同摘要重试返回同一份回执）。
    pub async fn create_group_signed(
        &self,
        signed: &SignedGroupCreation,
    ) -> Result<GroupRegistration, NodeClientError> {
        let body = serde_json::to_vec(&GroupBody {
            schema_version: CLIENT_SCHEMA_VERSION,
            creation: &signed.creation,
            device_signature: &signed.device_signature,
        })
        .map_err(|_| NodeClientError::local_invalid("group creation could not be encoded"))?;
        let bytes = self
            .send_authenticated(
                "POST",
                GROUPS_PATH,
                "",
                &body,
                ChallengePurpose::RequestAuth,
            )
            .await?;
        let envelope: GroupEnvelope = self.decode(&bytes)?;
        let registration = envelope.registration;
        self.ensure_node_identity(&registration.node_id)?;
        self.verify_node_output(|key| registration.verify(key))?;
        if registration.group_id != signed.creation.group_id {
            return Err(NodeClientError::protocol(
                ErrorCode::InvalidTarget,
                "node registration is for another group",
            ));
        }
        Ok(registration)
    }

    /// NODE-006：追加一条应用/控制候选（path 由候选的 group_id 决定，二者不可能不一致）。
    pub async fn append_entry(
        &self,
        candidate: &GroupCandidate,
    ) -> Result<AppendResult, NodeClientError> {
        let signed = self.sign_candidate(candidate.clone())?;
        self.append_entry_signed(&signed).await
    }

    /// NODE-006：提交已签名候选；同一 submission 同字节重试得到原 seq/回执。
    pub async fn append_entry_signed(
        &self,
        signed: &SignedCandidate,
    ) -> Result<AppendResult, NodeClientError> {
        let group_id = signed.candidate.group_id.clone();
        validate_bounded_id(
            "group id",
            &group_id,
            MAX_GROUP_ID_BYTES,
        )
        .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        let path = format!("{GROUPS_PATH}/{group_id}/entries");
        let body = serde_json::to_vec(&EntryBody {
            schema_version: CLIENT_SCHEMA_VERSION,
            candidate: &signed.candidate,
            author_signature: &signed.author_signature,
        })
        .map_err(|_| NodeClientError::local_invalid("candidate could not be encoded"))?;
        let bytes = self
            .send_authenticated(
                "POST",
                &path,
                "",
                &body,
                ChallengePurpose::RequestAuth,
            )
            .await?;
        let envelope: EntryEnvelope = self.decode(&bytes)?;
        let result = envelope.result;
        self.ensure_node_identity(&result.node_id)?;
        self.verify_node_output(|key| result.verify(key))?;
        if result
            .verify_candidate(signed, chat_protocol::MAX_CIPHERTEXT_BYTES)
            .is_err()
        {
            return Err(NodeClientError::protocol(
                ErrorCode::InvalidReceiptScope,
                "node receipt does not bind the submitted candidate",
            ));
        }
        Ok(result)
    }

    /// NODE-007：分页读取群日志（after 为“已看到的最大 seq”）。
    pub async fn fetch_log(
        &self,
        group_id: &str,
        after: u64,
        limit: u32,
    ) -> Result<LogPage, NodeClientError> {
        validate_bounded_id(
            "group id",
            group_id,
            MAX_GROUP_ID_BYTES,
        )
        .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        let path = format!("{GROUPS_PATH}/{group_id}/log");
        let query = format!("after={after}&limit={limit}");
        let bytes = self
            .send_authenticated(
                "GET",
                &path,
                &query,
                b"",
                ChallengePurpose::RequestAuth,
            )
            .await?;
        let envelope: LogEnvelope = self.decode(&bytes)?;
        let page = envelope.page;
        self.ensure_node_identity(&page.head.node_id)?;
        self.verify_node_output(|key| page.head.verify(key))?;
        for entry in &page.entries {
            self.verify_node_output(|key| entry.receipt.verify(key))?;
        }
        if page.group_id != group_id {
            return Err(NodeClientError::protocol(
                ErrorCode::InvalidTarget,
                "node log page is for another group",
            ));
        }
        Ok(page)
    }

    /// NODE-008：取节点签名 head。
    pub async fn get_head(&self, group_id: &str) -> Result<SignedLogHead, NodeClientError> {
        validate_bounded_id(
            "group id",
            group_id,
            MAX_GROUP_ID_BYTES,
        )
        .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        let path = format!("{GROUPS_PATH}/{group_id}/head");
        let bytes = self
            .send_authenticated(
                "GET",
                &path,
                "",
                b"",
                ChallengePurpose::RequestAuth,
            )
            .await?;
        let envelope: HeadEnvelope = self.decode(&bytes)?;
        let head = envelope.head;
        self.ensure_node_identity(&head.node_id)?;
        self.verify_node_output(|key| head.verify(key))?;
        if head.group_id != group_id {
            return Err(NodeClientError::protocol(
                ErrorCode::InvalidTarget,
                "node head is for another group",
            ));
        }
        Ok(head)
    }

    /// NODE-009：把客户端密文投递到目标设备信箱。
    pub async fn store_mailbox_item(
        &self,
        item: &MailboxItem,
    ) -> Result<MailboxReceipt, NodeClientError> {
        let body = serde_json::to_vec(&MailboxItemBody {
            schema_version: CLIENT_SCHEMA_VERSION,
            item,
        })
        .map_err(|_| NodeClientError::local_invalid("mailbox item could not be encoded"))?;
        let bytes = self
            .send_authenticated(
                "POST",
                MAILBOX_ITEMS_PATH,
                "",
                &body,
                ChallengePurpose::RequestAuth,
            )
            .await?;
        let envelope: MailboxReceiptEnvelope = self.decode(&bytes)?;
        let receipt = envelope.receipt;
        self.ensure_node_identity(&receipt.node_id)?;
        self.verify_node_output(|key| receipt.verify(key))?;
        let payload = decode_hex_bytes(
            "mailbox payload",
            &item.ciphertext_envelope,
            chat_protocol::MAX_CIPHERTEXT_BYTES,
        )
        .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        if receipt.verify_item(item, &payload).is_err() {
            return Err(NodeClientError::protocol(
                ErrorCode::InvalidReceiptScope,
                "node receipt does not bind the submitted mailbox item",
            ));
        }
        Ok(receipt)
    }

    /// NODE-010：读取本设备的一页信箱（只读不删，可重复读）。
    pub async fn read_mailbox_page(
        &self,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<MailboxPage, NodeClientError> {
        let query = match cursor {
            Some(cursor) => {
                validate_query_token(cursor)?;
                format!("limit={limit}&cursor={cursor}")
            }
            None => format!("limit={limit}"),
        };
        let bytes = self
            .send_authenticated(
                "GET",
                MAILBOX_PATH,
                &query,
                b"",
                ChallengePurpose::RequestAuth,
            )
            .await?;
        let envelope: MailboxPageEnvelope = self.decode(&bytes)?;
        Ok(envelope.page)
    }

    /// NODE-011：幂等 ACK 本设备条目（重复 ACK 不会二次释放配额）。
    pub async fn ack_mailbox_items(
        &self,
        item_ids: &[String],
    ) -> Result<AckSummary, NodeClientError> {
        let body = serde_json::to_vec(&AckBody {
            schema_version: CLIENT_SCHEMA_VERSION,
            item_ids,
        })
        .map_err(|_| NodeClientError::local_invalid("ack request could not be encoded"))?;
        let bytes = self
            .send_authenticated(
                "POST",
                MAILBOX_ACK_PATH,
                "",
                &body,
                ChallengePurpose::RequestAuth,
            )
            .await?;
        let envelope: AckEnvelope = self.decode(&bytes)?;
        Ok(envelope.summary)
    }

    /// NODE-012：创建密文附件上传会话（同 operation + 同描述幂等返回同一会话）。
    pub async fn begin_blob_upload(
        &self,
        operation_id: &str,
        descriptor: &PublicBlobDescriptor,
    ) -> Result<UploadSession, NodeClientError> {
        let body = serde_json::to_vec(&BeginBlobUploadBody {
            schema_version: CLIENT_SCHEMA_VERSION,
            operation_id,
            descriptor,
        })
        .map_err(|_| NodeClientError::local_invalid("blob upload request could not be encoded"))?;
        let bytes = self
            .send_authenticated(
                "POST",
                BLOBS_PATH,
                "",
                &body,
                ChallengePurpose::RequestAuth,
            )
            .await?;
        let envelope: UploadSessionEnvelope = self.decode(&bytes)?;
        let session = envelope.session;
        self.ensure_node_identity(&session.node_id)?;
        self.verify_node_output(|key| session.verify(key))?;
        // 会话必须绑定本设备与本次声明的 blob，避免用别人的会话上传。
        if session.owner_device_id != self.device_id || session.blob_id != descriptor.blob_id {
            return Err(NodeClientError::protocol(
                ErrorCode::InvalidReceiptScope,
                "upload session does not bind this device and blob",
            ));
        }
        if session.chunk_count != descriptor.chunk_count
            || session.ciphertext_bytes != descriptor.ciphertext_bytes
        {
            return Err(NodeClientError::protocol(
                ErrorCode::InvalidReceiptScope,
                "upload session does not bind the declared chunk layout",
            ));
        }
        Ok(session)
    }

    /// NODE-013：按序上传一块密文（原始字节，不做 hex/JSON 二次编码）。
    pub async fn put_blob_chunk(
        &self,
        upload_id: &str,
        chunk_index: u32,
        bytes: &[u8],
    ) -> Result<ChunkReceipt, NodeClientError> {
        validate_bounded_id("upload id", upload_id, MAX_BLOB_UPLOAD_ID_BYTES)
            .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        if bytes.is_empty() {
            return Err(NodeClientError::local_invalid("cipher chunk must not be empty"));
        }
        let path = format!("{BLOBS_PATH}/{upload_id}/chunks/{chunk_index}");
        let response = self
            .send_authenticated_raw("PUT", &path, "", bytes, OCTET_STREAM_CONTENT_TYPE)
            .await?;
        let envelope: ChunkReceiptEnvelope = self.decode(&response)?;
        let receipt = envelope.receipt;
        self.ensure_node_identity(&receipt.node_id)?;
        self.verify_node_output(|key| receipt.verify(key))?;
        if receipt.upload_id != upload_id
            || receipt.chunk_index != chunk_index
            || receipt.cipher_hash != hex_encode(&sha256(bytes))
        {
            return Err(NodeClientError::protocol(
                ErrorCode::InvalidReceiptScope,
                "chunk receipt does not bind this chunk",
            ));
        }
        Ok(receipt)
    }

    /// NODE-014：提交清单、校验总长与整体摘要并发布（重复完成返回同一回执）。
    pub async fn complete_blob_upload(
        &self,
        upload_id: &str,
        manifest: &CipherManifest,
    ) -> Result<BlobReceipt, NodeClientError> {
        validate_bounded_id("upload id", upload_id, MAX_BLOB_UPLOAD_ID_BYTES)
            .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        let body = serde_json::to_vec(&CompleteBlobUploadBody {
            schema_version: CLIENT_SCHEMA_VERSION,
            manifest,
        })
        .map_err(|_| NodeClientError::local_invalid("blob manifest could not be encoded"))?;
        let path = format!("{BLOBS_PATH}/{upload_id}/complete");
        let bytes = self
            .send_authenticated(
                "POST",
                &path,
                "",
                &body,
                ChallengePurpose::RequestAuth,
            )
            .await?;
        let envelope: BlobReceiptEnvelope = self.decode(&bytes)?;
        let receipt = envelope.receipt;
        self.ensure_node_identity(&receipt.node_id)?;
        self.verify_node_output(|key| receipt.verify(key))?;
        if receipt.upload_id != upload_id
            || receipt.chunk_count != manifest.chunk_count
            || receipt.ciphertext_bytes != manifest.ciphertext_bytes
            || receipt.ciphertext_digest != manifest.ciphertext_digest
        {
            return Err(NodeClientError::protocol(
                ErrorCode::InvalidReceiptScope,
                "publish receipt does not bind this manifest",
            ));
        }
        Ok(receipt)
    }

    /// NODE-015：取回一块密文（原始字节；调用方按清单摘要自行校验）。
    pub async fn read_blob_chunk(
        &self,
        blob_id: &str,
        chunk_index: u32,
    ) -> Result<Vec<u8>, NodeClientError> {
        validate_bounded_id("blob id", blob_id, MAX_BLOB_ID_BYTES)
            .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        ObjectId::parse(blob_id)
            .map_err(|_| NodeClientError::local_invalid("blob id is not a canonical protocol id"))?;
        let path = format!("{BLOBS_PATH}/{blob_id}/chunks/{chunk_index}");
        self.send_authenticated("GET", &path, "", b"", ChallengePurpose::RequestAuth)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> NodeClient {
        let endpoint = NodeEndpoint::new("http://127.0.0.1:1", "node-1").expect("endpoint");
        NodeClient::new(endpoint, "dev-1", SigningKey::from_bytes(&[3_u8; 32])).expect("client")
    }

    #[test]
    fn limits_are_bounded_and_never_retry_forever() {
        assert!(ClientLimits::default().validate().is_ok());
        let unlimited = ClientLimits {
            challenge_attempts: 0,
            ..ClientLimits::default()
        };
        assert!(unlimited.validate().is_err());
        let too_many = ClientLimits {
            challenge_attempts: 3,
            ..ClientLimits::default()
        };
        assert_eq!(
            too_many.validate().unwrap_err().kind(),
            NodeClientErrorKind::Config
        );
        let zero_response = ClientLimits {
            max_response_bytes: 0,
            ..ClientLimits::default()
        };
        assert!(zero_response.validate().is_err());
        let huge_response = ClientLimits {
            max_response_bytes: 128 * 1024 * 1024,
            ..ClientLimits::default()
        };
        assert!(huge_response.validate().is_err());
        let zero_timeout = ClientLimits {
            timeout: Duration::from_millis(0),
            ..ClientLimits::default()
        };
        assert!(zero_timeout.validate().is_err());
    }

    #[test]
    fn retry_is_limited_to_retakeable_failures() {
        let auth_expired = NodeClientError::from_node_code(401, "AUTH_EXPIRED");
        assert!(retryable_failure(&auth_expired));
        let replay = NodeClientError::from_node_code(401, "AUTH_REPLAY");
        assert!(retryable_failure(&replay));
        let unknown = NodeClientError::from_node_code(401, "INVALID_CHALLENGE");
        assert!(retryable_failure(&unknown));
        let timeout = NodeClientError::timeout("node request timed out");
        assert!(retryable_failure(&timeout));
        let network = NodeClientError::network("node connection failed");
        assert!(retryable_failure(&network));
        // 错误的设备签名不是挑战的问题：重取挑战也不会变好，因此不重试。
        let wrong_key = NodeClientError::from_node_code(401, "INVALID_SIGNATURE");
        assert!(!retryable_failure(&wrong_key));
        let denied = NodeClientError::from_node_code(403, "PERMISSION_DENIED");
        assert!(!retryable_failure(&denied));
    }

    #[test]
    fn query_tokens_cannot_smuggle_extra_parameters() {
        assert!(validate_query_token("cur1.aabb.ccdd").is_ok());
        assert!(validate_query_token("").is_err());
        assert!(validate_query_token("cur1.a&limit=999").is_err());
        assert!(validate_query_token("cur1.a=b").is_err());
        assert!(validate_query_token("cur1.%2e").is_err());
        assert!(validate_query_token(&"a".repeat(chat_protocol::MAX_CURSOR_TOKEN_BYTES + 1)).is_err());
    }

    #[test]
    fn client_rejects_bad_device_ids_and_endpoints() {
        let endpoint = NodeEndpoint::new("http://127.0.0.1:1", "node-1").expect("endpoint");
        assert!(NodeClient::new(endpoint.clone(), "", SigningKey::from_bytes(&[1_u8; 32])).is_err());
        assert!(
            NodeClient::new(endpoint.clone(), "dev 1", SigningKey::from_bytes(&[1_u8; 32])).is_err()
        );
        let client = client();
        // 非协议规范 ID 不能冒充设备身份。
        assert!(client.protocol_device_id().is_err());
        assert_eq!(client.device_id(), "dev-1");
    }

    #[test]
    fn debug_never_prints_the_signing_key() {
        let client = client();
        let text = format!("{client:?}");
        assert!(text.contains("NodeClient"));
        assert!(text.contains("dev-1"));
        assert!(!text.contains("signing_key"));
    }
}
