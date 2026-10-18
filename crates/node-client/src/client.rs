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
    MAX_BLOB_CHUNK_BODY_BYTES, MAX_BLOB_ID_BYTES, MAX_BLOB_UPLOAD_ID_BYTES, MAX_DEVICE_ID_BYTES,
    MAX_GROUP_ID_BYTES, MAX_HTTP_BODY_BYTES,
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

/// CAP 包：能力令牌的两种用途文本（与节点 `CapabilityScope::as_str` 一致）。
const SCOPE_BLOB_UPLOAD: &'static str = "blob:upload";
const SCOPE_BLOB_DOWNLOAD: &'static str = "blob:download";

#[derive(Serialize)]
struct CapabilityBody<'a> {
    schema_version: u16,
    object_id: &'a str,
    scope: &'a str,
    ttl_seconds: u64,
}

#[derive(Deserialize)]
struct CapabilityEnvelope {
    schema_version: u16,
    capability: CapabilityView,
}

#[derive(Deserialize)]
struct CapabilityView {
    token: String,
    object_id: String,
    device_id: String,
    scope: String,
    issued_at: u64,
    expires_at: u64,
}

/// 鉴权头（与节点 API-003 的传输字段一致）。
pub const HEADER_DEVICE_ID: &str = "x-chat-device-id";
pub const HEADER_CHALLENGE_ID: &str = "x-chat-challenge-id";
pub const HEADER_NONCE: &str = "x-chat-challenge-nonce";
pub const HEADER_SIGNATURE: &str = "x-chat-signature";
/// CAP 包：能力令牌请求头（与设备鉴权头并存，节点强制模式下必带）。
pub const HEADER_CAPABILITY: &str = "x-chat-capability";

const CHALLENGE_PATH: &str = "/v1/auth/challenge";
const DEVICES_PATH: &str = "/v1/devices";
const GROUPS_PATH: &str = "/v1/groups";
const MAILBOX_ITEMS_PATH: &str = "/v1/mailbox/items";
const MAILBOX_PATH: &str = "/v1/mailbox";
const MAILBOX_ACK_PATH: &str = "/v1/mailbox/ack";
const BLOBS_PATH: &str = "/v1/blobs";
const CAPABILITIES_PATH: &str = "/v1/capabilities";
/// CAP 包：默认申请的能力令牌有效期（秒；服务端上限 1 小时）。
const CAPABILITY_TTL_SECONDS: u64 = 600;

/// 一次已登记设备的签名发送请求（参数集中，避免 9 参数函数）。
struct SignedSend<'a> {
    method: &'a str,
    path: &'a str,
    query: &'a str,
    body: &'a [u8],
    purpose: ChallengePurpose,
    signer_device_id: Option<&'a str>,
    content_type: &'static str,
    /// 正文本上限：JSON DTO 用 `max_request_bytes`，密文块用 `max_chunk_bytes`。
    request_limit: usize,
    /// CAP 包：可选能力令牌（只作为请求头附加，不参与签名原文）。
    capability: Option<&'a str>,
}

/// 客户端上限与超时。全部在构造时校验，越界配置显式拒绝。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientLimits {
    /// 单次响应体上限（字节）。
    pub max_response_bytes: usize,
    /// 单次 JSON 请求正文上限（字节）。
    pub max_request_bytes: usize,
    /// 单块密文 PUT 的正文上限（字节）。
    ///
    /// 附件块是原始字节、可达节点分块硬上限，不能套用 JSON DTO 的小上限；
    /// 默认取节点分块硬上限（4 MiB），仍由本地先拒、不把超大块发到网络。
    pub max_chunk_bytes: usize,
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
            max_chunk_bytes: MAX_BLOB_CHUNK_BODY_BYTES,
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
        if self.max_chunk_bytes == 0 || self.max_chunk_bytes > MAX_BLOB_CHUNK_BODY_BYTES {
            return Err(NodeClientError::config(
                "max chunk bytes must be within 1..=node chunk hard limit",
            ));
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
    CapabilityEnvelope,
);
/// 节点客户端：固定端点 + 固定设备身份 + Ed25519 签名钥。
pub struct NodeClient {
    endpoint: NodeEndpoint,
    http: reqwest::Client,
    device_id: String,
    signing_key: SigningKey,
    limits: ClientLimits,
    /// CAP 包：按 (对象, 用途) 复用的能力令牌缓存（读到过期即失效）。
    capabilities: std::sync::Mutex<std::collections::HashMap<String, CachedCapability>>,
    /// CAP 包：节点是否强制能力令牌（初次 `AUTH_REQUIRED` 后自适应为 true）。
    capability_required: std::sync::atomic::AtomicBool,
}

/// 一个缓存的能力令牌及其本地到期点。
///
/// 用**单调时钟**（`Instant`）而不是令牌里的绝对时间：节点时钟与客户端时钟可能相差很大
/// （测试用固定时钟、生产也可能有偏差），按“签发时拿到的有效期长度”换算成本地截止点，
/// 是唯一不受时钟偏差影响的判据。
struct CachedCapability {
    token: String,
    deadline: std::time::Instant,
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
            capabilities: std::sync::Mutex::new(std::collections::HashMap::new()),
            capability_required: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// CAP 包：显式打开/关闭“自动取能力令牌”模式。
    ///
    /// 关闭时仍会在收到 `AUTH_REQUIRED` 后自适应打开一次（服务端强制时旧调用方自动可用）。
    pub fn set_capability_mode(&self, required: bool) {
        self.capability_required
            .store(required, std::sync::atomic::Ordering::SeqCst);
    }

    /// CAP 包：当前是否处于“必须带令牌”模式。
    pub fn capability_mode(&self) -> bool {
        self.capability_required
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// CAP 包：当前缓存里未过期的令牌数量（诊断/测试用，不含令牌内容）。
    pub fn cached_capability_count(&self) -> usize {
        let now = std::time::Instant::now();
        self.capabilities
            .lock()
            .map(|cache| {
                cache
                    .values()
                    .filter(|cached| cached.deadline > now)
                    .count()
            })
            .unwrap_or(0)
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
        self.send_signed(
            challenge,
            &meta,
            body,
            &self.device_id,
            JSON_CONTENT_TYPE,
            None,
        )
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
        self.send_authenticated_request(SignedSend {
            method,
            path,
            query,
            body,
            purpose,
            signer_device_id: None,
            content_type: JSON_CONTENT_TYPE,
            request_limit: self.limits.max_request_bytes,
            capability: None,
        })
        .await
    }

    /// 带可选能力令牌的已登记设备请求（CAP 包：附件块上传/下载复用同一条发送逻辑）。
    async fn send_authenticated_with_capability(
        &self,
        method: &str,
        path: &str,
        query: &str,
        body: &[u8],
        content_type: &'static str,
        request_limit: usize,
        capability: Option<&str>,
    ) -> Result<Vec<u8>, NodeClientError> {
        let owned: Option<String> = capability.map(str::to_owned);
        self.send_authenticated_request(SignedSend {
            method,
            path,
            query,
            body,
            purpose: ChallengePurpose::RequestAuth,
            signer_device_id: None,
            content_type,
            request_limit,
            capability: owned.as_deref(),
        })
        .await
    }

    /// 已登记设备的一次签名发送；参数集中在 [`SignedSend`] 里，避免超长参数列表。
    async fn send_authenticated_request(
        &self,
        request: SignedSend<'_>,
    ) -> Result<Vec<u8>, NodeClientError> {
        let SignedSend {
            method,
            path,
            query,
            body,
            purpose,
            signer_device_id,
            content_type,
            request_limit,
            capability,
        } = request;
        let meta = RequestMeta::canonical(method, path, query)
            .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        if body.len() > request_limit {
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
                .attempt_signed(&meta, body, purpose, signer, content_type, capability)
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
        capability: Option<&str>,
    ) -> Result<Vec<u8>, NodeClientError> {
        let challenge = self.fetch_challenge(purpose).await?;
        self.send_signed(&challenge, meta, body, signer_device_id, content_type, capability)
            .await
    }

    async fn send_signed(
        &self,
        challenge: &AuthChallenge,
        meta: &RequestMeta,
        body: &[u8],
        signer_device_id: &str,
        content_type: &'static str,
        capability: Option<&str>,
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
        let mut request = self
            .http
            .request(method, url)
            .header(HEADER_DEVICE_ID, signer_device_id)
            .header(HEADER_CHALLENGE_ID, challenge.challenge_id.as_str())
            .header(HEADER_NONCE, challenge.nonce.as_str())
            .header(HEADER_SIGNATURE, hex_encode(&signature.to_bytes()))
            .header(CONTENT_TYPE, content_type);
        // CAP 包：能力令牌只作为附加请求头，不进入签名原文（签名已覆盖方法/路径/正文）。
        if let Some(token) = capability {
            request = request.header(HEADER_CAPABILITY, token);
        }
        let response = request
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
            .send_authenticated_request(SignedSend {
                method: "POST",
                path: DEVICES_PATH,
                query: "",
                body: &body,
                purpose: ChallengePurpose::DeviceRegistration,
                signer_device_id: Some(request.credential.device_id.as_str()),
                content_type: JSON_CONTENT_TYPE,
                request_limit: self.limits.max_request_bytes,
                capability: None,
            })
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

    /// CAP 包：向节点申请一个对象级能力令牌（POST /v1/capabilities）并写入缓存。
    ///
    /// 令牌公开绑定 (对象, 本设备, 用途, 期限)；客户端只核对绑定是否与本机一致，不解析签名。
    pub async fn issue_capability(
        &self,
        object_id: &str,
        scope: &str,
        ttl_seconds: Option<u64>,
    ) -> Result<String, NodeClientError> {
        if !matches!(scope, SCOPE_BLOB_UPLOAD | SCOPE_BLOB_DOWNLOAD) {
            return Err(NodeClientError::local_invalid("capability scope is not supported"));
        }
        let body = serde_json::to_vec(&CapabilityBody {
            schema_version: CLIENT_SCHEMA_VERSION,
            object_id,
            scope,
            ttl_seconds: ttl_seconds.unwrap_or(CAPABILITY_TTL_SECONDS),
        })
        .map_err(|_| NodeClientError::local_invalid("capability request could not be encoded"))?;
        let bytes = self
            .send_authenticated(
                "POST",
                CAPABILITIES_PATH,
                "",
                &body,
                ChallengePurpose::RequestAuth,
            )
            .await?;
        let envelope: CapabilityEnvelope = self.decode(&bytes)?;
        let view = envelope.capability;
        if view.object_id != object_id || view.scope != scope || view.device_id != self.device_id
        {
            return Err(NodeClientError::protocol(
                ErrorCode::InvalidReceiptScope,
                "capability token does not bind this object, device and scope",
            ));
        }
        if view.expires_at <= view.issued_at || view.token.is_empty() {
            return Err(NodeClientError::protocol(
                ErrorCode::InvalidReceiptScope,
                "capability token has an invalid validity window",
            ));
        }
        self.store_capability(object_id, scope, &view.token, view.issued_at, view.expires_at);
        Ok(view.token)
    }

    /// CAP 包：取一个当前可用的令牌（缓存命中直接用；否则按需申请）。
    async fn capability_for(
        &self,
        object_id: &str,
        scope: &str,
    ) -> Result<Option<String>, NodeClientError> {
        if let Some(token) = self.cached_capability(object_id, scope) {
            return Ok(Some(token));
        }
        if !self.capability_mode() {
            // 服务端不强制时不做额外请求（保持旧客户端行为与请求数）。
            return Ok(None);
        }
        self.issue_capability(object_id, scope, None).await.map(Some)
    }

    fn cached_capability(&self, object_id: &str, scope: &str) -> Option<String> {
        let key = format!("{scope}|{object_id}");
        let now = std::time::Instant::now();
        let cache = self.capabilities.lock().ok()?;
        cache
            .get(&key)
            .filter(|cached| cached.deadline > now)
            .map(|cached| cached.token.clone())
    }

    /// 写入缓存：有效期长度 = `expires_at - issued_at`（秒），换算成本地单调截止点；
    /// 留 1 秒余量，避免刚好卡在边界时把即将过期的令牌发出去。
    fn store_capability(
        &self,
        object_id: &str,
        scope: &str,
        token: &str,
        issued_at: u64,
        expires_at: u64,
    ) {
        let key = format!("{scope}|{object_id}");
        let lifetime = expires_at.saturating_sub(issued_at);
        let usable = lifetime.saturating_sub(1).max(1);
        let deadline = std::time::Instant::now() + Duration::from_secs(usable);
        if let Ok(mut cache) = self.capabilities.lock() {
            // 附带清理过期项，避免缓存随对象数量无界增长。
            let now = std::time::Instant::now();
            cache.retain(|_, cached| cached.deadline > now);
            cache.insert(
                key,
                CachedCapability {
                    token: token.to_owned(),
                    deadline,
                },
            );
        }
    }

    /// NODE-013：按序上传一块密文（原始字节，不做 hex/JSON 二次编码）。
    ///
    /// 正文上限用 [`ClientLimits::max_chunk_bytes`]（默认节点分块硬上限 4 MiB），
    /// 不是 JSON DTO 的 [`ClientLimits::max_request_bytes`]：客户端附件按 256 KiB 明文
    /// 分块（密文块 262_173 字节），套用 DTO 小上限会让多块附件在本地就被拒。
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
        let capability = self.capability_for(upload_id, SCOPE_BLOB_UPLOAD).await?;
        let response = match self
            .send_authenticated_with_capability(
                "PUT",
                &path,
                "",
                bytes,
                OCTET_STREAM_CONTENT_TYPE,
                self.limits.max_chunk_bytes,
                capability.as_deref(),
            )
            .await
        {
            // 服务端强制而客户端还不知道：自适应打开模式并重试一次。
            Err(error) if error.node_code() == Some("AUTH_REQUIRED") => {
                self.set_capability_mode(true);
                let capability = self.issue_capability(upload_id, SCOPE_BLOB_UPLOAD, None).await?;
                self.send_authenticated_with_capability(
                    "PUT",
                    &path,
                    "",
                    bytes,
                    OCTET_STREAM_CONTENT_TYPE,
                    self.limits.max_chunk_bytes,
                    Some(&capability),
                )
                .await?
            }
            // 令牌被判过期/无效：丢掉缓存再取一次（只重试一次，不做无限循环）。
            Err(error)
                if matches!(
                    error.node_code(),
                    Some("AUTH_EXPIRED" | "INVALID_CAPABILITY")
                ) =>
            {
                self.set_capability_mode(true);
                let capability = self.issue_capability(upload_id, SCOPE_BLOB_UPLOAD, None).await?;
                self.send_authenticated_with_capability(
                    "PUT",
                    &path,
                    "",
                    bytes,
                    OCTET_STREAM_CONTENT_TYPE,
                    self.limits.max_chunk_bytes,
                    Some(&capability),
                )
                .await?
            }
            other => other?,
        };
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
        let capability = self.capability_for(blob_id, SCOPE_BLOB_DOWNLOAD).await?;
        match self
            .send_authenticated_with_capability(
                "GET",
                &path,
                "",
                b"",
                JSON_CONTENT_TYPE,
                self.limits.max_request_bytes,
                capability.as_deref(),
            )
            .await
        {
            Ok(bytes) => Ok(bytes),
            Err(error)
                if matches!(
                    error.node_code(),
                    Some("AUTH_REQUIRED" | "AUTH_EXPIRED" | "INVALID_CAPABILITY")
                ) =>
            {
                // 强制模式自适应 + 过期令牌刷新，只重试一次。
                self.set_capability_mode(true);
                let capability = self
                    .issue_capability(blob_id, SCOPE_BLOB_DOWNLOAD, None)
                    .await?;
                self.send_authenticated_with_capability(
                    "GET",
                    &path,
                    "",
                    b"",
                    JSON_CONTENT_TYPE,
                    self.limits.max_request_bytes,
                    Some(&capability),
                )
                .await
            }
            Err(error) => Err(error),
        }
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