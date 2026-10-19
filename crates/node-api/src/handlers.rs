//! axum 路由与端点适配（API-006 / API-007 风格的单一适配层）。
//!
//! 每个 handler 只做：限制正文、解码 DTO、鉴权、调用服务、映射结果。
//! 领域规则与事务全部在 crates/node-services；本层不重复实现，也不放宽任何安全条件。
//! 所有已登记设备端点都走同一个 API-003 鉴权：请求签名绑定 method + 规范 path + query + 正文摘要。
use std::collections::BTreeMap;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Extension, Path as UrlPath, Request, State};
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::http::{HeaderMap, Uri};
use axum::response::{IntoResponse, Response};
use axum::http::header::CONTENT_TYPE;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chat_node_services::auth::{ChallengePurpose, RequestMeta};
use chat_node_services::services::AuthenticatedDevice;
use chat_node_services::canonical::{hex_encode, sha256};
use chat_node_services::audit::AuditOutcome;
use chat_node_services::capability::CapabilityScope;
use chat_node_services::limits::{MAX_BLOB_CHUNK_BODY_BYTES, MAX_HTTP_BODY_BYTES};
use serde::de::DeserializeOwned;

use crate::dto::{
    AckMailboxItemsRequestDto, AckMailboxItemsResponseDto, AppendGroupEntryRequestDto,
    AppendGroupEntryResponseDto, BeginBlobUploadRequestDto, BlobReceiptResponseDto,
    ChallengeRequestDto, ChallengeResponseDto, ChunkReceiptResponseDto,
    CompleteBlobUploadRequestDto, DeviceRegistrationResponseDto, GroupHeadResponseDto,
    GroupLogResponseDto, GroupRegistrationResponseDto, MailboxPageResponseDto,
    MailboxReceiptResponseDto, RegisterDeviceRequestDto, RegisterGroupRequestDto,
    CapabilityResponseDto, HealthResponseDto, IssueCapabilityRequestDto,
    RevokeCapabilityRequestDto, RevokeCapabilityResponseDto, SignedAuthHeaders,
    StoreMailboxItemRequestDto, UploadSessionResponseDto, parse_chunk_index,
};
use crate::error::ApiError;
use crate::state::{AppState, REQUEST_ID_HEADER, RequestId};

/// 日志分页默认条数（服务层再按策略上限校验）。
const DEFAULT_LOG_PAGE_LIMIT: u32 = 50;
/// 信箱分页默认条数（服务层再按策略上限校验）。
const DEFAULT_MAILBOX_PAGE_LIMIT: u32 = 20;

/// 构造节点 HTTP 路由。
///
/// 公开端点只有挑战签发；其余端点都要求 API-003 已登记设备鉴权。
/// 逐 IP 限流（API-005）与事件套接字（API-009）尚未在本阶段实现。
pub fn build_router(state: AppState) -> Router {
    // NODE-018：每个请求生成/透传 x-chat-request-id，并把结果写进审计。
    let audit_state = state.clone();
    Router::new()
        // 健康检查：匿名、不鉴权、不限流，只返回公开身份（不含任何秘密）。
        .route("/v1/health", get(health))
        .route("/v1/auth/challenge", post(issue_challenge))
        .route("/v1/devices", post(register_device))
        .route("/v1/groups", post(register_group))
        .route("/v1/groups/{group_id}/entries", post(append_group_entry))
        .route("/v1/groups/{group_id}/log", get(fetch_group_log))
        .route("/v1/groups/{group_id}/head", get(group_head))
        .route("/v1/mailbox/items", post(store_mailbox_item))
        .route("/v1/mailbox", get(read_mailbox_page))
        .route("/v1/mailbox/ack", post(ack_mailbox_items))
        .route("/v1/capabilities", post(issue_capability))
        // CAP-REV：撤销能力令牌（设备鉴权；撤销立即生效）。
        .route("/v1/capabilities/{token_id}/revoke", post(revoke_capability))
        .route("/v1/blobs", post(begin_blob_upload))
        // 同一个 URL 形状承载两个动作：PUT = 写入上传会话的一块，GET = 读取已发布 blob 的一块。
        // 路由参数因此只能有一个名字（axum 不允许同名形状的两种参数命名）。
        // 原始密文块正文可达分块硬上限（4 MiB）；路由级 DefaultBodyLimit 覆盖全局 64 KiB，
        // 因此只有这一条路由被放宽，JSON DTO 路由仍然受全局小上限保护。
        .route(
            "/v1/blobs/{id}/chunks/{chunk_index}",
            put(put_blob_chunk)
                .get(read_blob_chunk)
                .layer(DefaultBodyLimit::max(MAX_BLOB_CHUNK_BODY_BYTES)),
        )
        .route(
            "/v1/blobs/{upload_id}/complete",
            post(complete_blob_upload),
        )
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .layer(axum::middleware::from_fn_with_state(
            audit_state,
            request_audit_layer,
        ))
        .with_state(state)
}

/// NODE-018：请求级审计中间件。
///
/// 语义：
/// - 生成或透传 `x-chat-request-id`（调用方给的是 1..=64 字节可打印 ASCII 才复用，否则派生）；
/// - 请求进入前放进 extensions（handler 可取出写进审计）；
/// - 响应统一带 `x-chat-request-id` 头；
/// - 记录一条 `http` 审计：结果按状态码分类，错误码能从响应扩展里取到就带上（只记元数据）。
pub async fn request_audit_layer(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let provided = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let request_id = state.next_request_id(provided.as_deref());
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));
    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    let status = response.status();
    let outcome = if status.is_success() {
        AuditOutcome::Ok
    } else if status.as_u16() == 401 || status.as_u16() == 403 || status.as_u16() == 404 {
        AuditOutcome::Denied
    } else {
        AuditOutcome::Failed
    };
    let now_ms = state.now().saturating_mul(1_000);
    state.record_http_audit(&request_id, outcome, None, now_ms);
    response
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

/// API-003 鉴权 + API-005 限流：所有已登记设备端点都走这一条路径。
///
/// 签名原文由服务层从 method/path/query/正文摘要重建，因此换方法、换路径、换查询、
/// 换正文、换节点或重放同一挑战都会在鉴权阶段失败。
///
/// 鉴权成功后立刻按 (设备, 账户) 两个桶扣减预算：超限返回 429 RATE_LIMITED 并带
/// retry_after。放在这里而不是逐个 handler 里，是为了让“新增端点忘记限流”不可能发生；
/// 限流只在 HTTP 层生效，服务层单测不经过它。
fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
    uri: &Uri,
    method: &str,
    body: &[u8],
    now: u64,
) -> Result<AuthenticatedDevice, ApiError> {
    let auth_headers = SignedAuthHeaders::from_headers(headers)?;
    let meta = RequestMeta::canonical(method, uri.path(), uri.query().unwrap_or(""))
        .map_err(ApiError::from_node)?;
    let body_hash = sha256(body);
    let signed = auth_headers.to_signed_auth();
    let actor = state.with_service(|service| {
        service.authenticate_request(&meta, &body_hash, &signed, ChallengePurpose::RequestAuth, now)
    })?;
    state.limit_device(&actor.device_id, &actor.account_id, body.len() as u64)?;
    Ok(actor)
}

/// 严格解析查询串：只接受 `key=value`、不重复、字符集受限。
///
/// 查询串本身参与请求签名，因此这里不做百分号解码：让线上原文与签名原文只有唯一写法。
fn parse_query(query: &str) -> Result<BTreeMap<String, String>, ApiError> {
    let mut parsed = BTreeMap::new();
    if query.is_empty() {
        return Ok(parsed);
    }
    for pair in query.split('&') {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| ApiError::invalid_request("query parameter must be key=value"))?;
        if key.is_empty() || parsed.contains_key(key) {
            return Err(ApiError::invalid_request("query key is empty or repeated"));
        }
        if !key
            .bytes()
            .chain(value.bytes())
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(ApiError::invalid_request(
                "query parameter contains unsupported characters",
            ));
        }
        parsed.insert(key.to_owned(), value.to_owned());
    }
    Ok(parsed)
}

fn take_number<T: std::str::FromStr>(parsed: &mut BTreeMap<String, String>, key: &str) -> Result<Option<T>, ApiError> {
    match parsed.remove(key) {
        Some(text) => text
            .parse::<T>()
            .map(Some)
            .map_err(|_| ApiError::invalid_request("query parameter is not a valid number")),
        None => Ok(None),
    }
}

fn ensure_no_unknown_query(parsed: &BTreeMap<String, String>) -> Result<(), ApiError> {
    if parsed.is_empty() {
        Ok(())
    } else {
        Err(ApiError::invalid_request("unknown query parameter"))
    }
}

/// POST /v1/auth/challenge：签发一次性短期挑战。
pub async fn issue_challenge(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, ApiError> {
    ensure_body_bounded(&state, body.len())?;
    // API-005：匿名端点先扣全局匿名预算（不读任何代理头）。
    state.limit_anonymous(body.len() as u64)?;
    let request: ChallengeRequestDto = parse_body(&body)?;
    let purpose = request.purpose()?;
    let now = state.now();
    let challenge = state.with_service(|service| service.issue_auth_challenge(purpose, now))?;
    Ok(Json(ChallengeResponseDto::new(challenge)).into_response())
}

/// POST /v1/devices：设备证明持有私钥后幂等注册。
///
/// 顺序：限长 -> 解码 DTO -> 用凭据中的设备公钥验证请求签名（消费挑战）-> 注册。
pub async fn register_device(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    ensure_body_bounded(&state, body.len())?;
    // API-005：注册同样是匿名端点，先扣全局匿名预算。
    state.limit_anonymous(body.len() as u64)?;
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

/// POST /v1/groups：群登记（NODE-005）。
pub async fn register_group(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    ensure_body_bounded(&state, body.len())?;
    let request: RegisterGroupRequestDto = parse_body(&body)?;
    let request = request.into_signed()?;
    let now = state.now();
    let actor = authenticate(&state, &headers, &uri, "POST", &body, now)?;
    let registration =
        state.with_service(|service| service.register_group(&request, &actor, now))?;
    Ok(Json(GroupRegistrationResponseDto::new(registration)).into_response())
}

/// POST /v1/groups/{id}/entries：日志追加（NODE-006）。
pub async fn append_group_entry(
    State(state): State<AppState>,
    UrlPath(group_id): UrlPath<String>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    ensure_body_bounded(&state, body.len())?;
    let request: AppendGroupEntryRequestDto = parse_body(&body)?;
    let (candidate, domain) = request.into_request()?;
    let now = state.now();
    let actor = authenticate(&state, &headers, &uri, "POST", &body, now)?;
    // 带领域声明走 with_domain 路径（声明参与验签与日志投影）；不带则与旧行为完全一致。
    let result = state.with_service(|service| {
        service.append_group_entry_with_domain(
            &group_id,
            &candidate,
            domain.as_ref(),
            &actor,
            now,
        )
    })?;
    Ok(Json(AppendGroupEntryResponseDto::new(result)).into_response())
}

/// GET /v1/groups/{id}/log?after=N&limit=M：分页日志（NODE-007）。
pub async fn fetch_group_log(
    State(state): State<AppState>,
    UrlPath(group_id): UrlPath<String>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let mut query = parse_query(uri.query().unwrap_or(""))?;
    let after = take_number::<u64>(&mut query, "after")?.unwrap_or(0);
    let limit = take_number::<u32>(&mut query, "limit")?.unwrap_or(DEFAULT_LOG_PAGE_LIMIT);
    ensure_no_unknown_query(&query)?;
    let now = state.now();
    let actor = authenticate(&state, &headers, &uri, "GET", b"", now)?;
    // CAP-REV：强制模式下日志读端点也要带未撤销的能力令牌（默认关闭）。
    enforce_read_capability(&state, &headers, &actor.device_id, now)?;
    let page = state.with_service(|service| service.fetch_group_log(&group_id, after, limit, &actor))?;
    Ok(Json(GroupLogResponseDto::new(page)).into_response())
}

/// GET /v1/groups/{id}/head：节点签名 head（NODE-008）。
pub async fn group_head(
    State(state): State<AppState>,
    UrlPath(group_id): UrlPath<String>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let query = parse_query(uri.query().unwrap_or(""))?;
    ensure_no_unknown_query(&query)?;
    let now = state.now();
    let actor = authenticate(&state, &headers, &uri, "GET", b"", now)?;
    let head = state.with_service(|service| service.get_group_head(&group_id, &actor))?;
    Ok(Json(GroupHeadResponseDto::new(head)).into_response())
}

/// POST /v1/mailbox/items：投递密文条目（NODE-009）。
pub async fn store_mailbox_item(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    ensure_body_bounded(&state, body.len())?;
    let request: StoreMailboxItemRequestDto = parse_body(&body)?;
    let item = request.into_item()?;
    let now = state.now();
    let actor = authenticate(&state, &headers, &uri, "POST", &body, now)?;
    let receipt = state.with_service(|service| service.store_mailbox_item(&item, &actor, now))?;
    Ok(Json(MailboxReceiptResponseDto::new(receipt)).into_response())
}

/// GET /v1/mailbox?cursor=...&limit=N：分页读取本设备信箱（NODE-010）。
pub async fn read_mailbox_page(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let mut query = parse_query(uri.query().unwrap_or(""))?;
    let cursor = query.remove("cursor");
    let limit =
        take_number::<u32>(&mut query, "limit")?.unwrap_or(DEFAULT_MAILBOX_PAGE_LIMIT);
    ensure_no_unknown_query(&query)?;
    let now = state.now();
    let actor = authenticate(&state, &headers, &uri, "GET", b"", now)?;
    // CAP-REV：强制模式下信箱读端点也要带未撤销的能力令牌（默认关闭）。
    enforce_read_capability(&state, &headers, &actor.device_id, now)?;
    let page = state.with_service(|service| {
        service.read_mailbox_page(&actor, cursor.as_deref(), limit, now)
    })?;
    Ok(Json(MailboxPageResponseDto::new(page)).into_response())
}

/// POST /v1/mailbox/ack：幂等 ACK 本设备条目（NODE-011）。
pub async fn ack_mailbox_items(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    ensure_body_bounded(&state, body.len())?;
    let request: AckMailboxItemsRequestDto = parse_body(&body)?;
    let ids = request.into_ids()?;
    let now = state.now();
    let actor = authenticate(&state, &headers, &uri, "POST", &body, now)?;
    let summary = state.with_service(|service| service.ack_mailbox_items(&actor, &ids, now))?;
    Ok(Json(AckMailboxItemsResponseDto::new(summary)).into_response())
}
/// API-004：能力令牌请求头（与设备鉴权头并存，二者都要满足）。
const CAPABILITY_HEADER: &str = "x-chat-capability";

/// 取出可选的能力令牌（缺省表示调用方没用令牌，仍走设备鉴权）。
fn capability_token(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let Some(value) = headers.get(CAPABILITY_HEADER) else {
        return Ok(None);
    };
    let text = value
        .to_str()
        .map_err(|_| ApiError::invalid_request("capability token is not valid text"))?;
    if text.is_empty() || text.len() > 2_048 {
        return Err(ApiError::invalid_request("capability token is out of range"));
    }
    Ok(Some(text.to_owned()))
}

/// POST /v1/capabilities：为本设备签发一个对象级能力令牌（API-004）。
///
/// 先做**对象级授权**（上传会话归属 / 附件可读性），再签发；令牌本身只证明“节点在
/// (对象, 设备, 用途, 期限) 上授权过”，不含密钥也无需落库。
pub async fn issue_capability(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    ensure_body_bounded(&state, body.len())?;
    let request: IssueCapabilityRequestDto = parse_body(&body)?;
    let (object_id, scope, ttl_seconds) = request.into_parts()?;
    let now = state.now();
    let actor = authenticate(&state, &headers, &uri, "POST", &body, now)?;
    let device_id = actor.device_id.clone();
    let (issued, grant) = state.with_service(|service| {
        match scope {
            CapabilityScope::BlobUpload => {
                service.authorize_blob_upload(&object_id, &actor, now)?;
            }
            CapabilityScope::BlobDownload => {
                service.authorize_blob_read(&object_id, &actor, now)?;
            }
        }
        let issued =
            service.issue_capability_with_id(&object_id, &device_id, scope, ttl_seconds, now)?;
        let grant = service.verify_capability(
            &issued.token,
            &object_id,
            &device_id,
            scope,
            now,
        )?;
        Ok((issued, grant))
    })?;
    Ok(
        Json(CapabilityResponseDto::new(
            issued.token,
            issued.token_id,
            grant,
        ))
        .into_response(),
    )
}


/// POST /v1/capabilities/{token_id}/revoke：撤销一个能力令牌（CAP-REV）。
///
/// 需要**设备鉴权**（与其它端点同一套签名挑战），撤销立即生效：之后任何带该令牌的
/// 请求都会被判 `CAPABILITY_REVOKED`。幂等：重复撤销返回 `revoked=false`。
pub async fn revoke_capability(
    State(state): State<AppState>,
    UrlPath(token_id): UrlPath<String>,
    Extension(request_id): Extension<RequestId>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    ensure_body_bounded(&state, body.len())?;
    let request: RevokeCapabilityRequestDto = if body.is_empty() {
        RevokeCapabilityRequestDto::default()
    } else {
        parse_body(&body)?
    };
    let reason = request.into_reason()?;
    let now = state.now();
    let actor = authenticate(&state, &headers, &uri, "POST", &body, now)?;
    let now_ms = u64::try_from(now)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000))
        .ok_or_else(|| ApiError::invalid_request("node clock is out of range"))?;
    // CAP-AUTHZ：撤销按**调用方设备**授权（默认只允许签发设备撤销自己的令牌；跨设备 → 403）。
    let outcome = state.with_service(|service| {
        let result = service.revoke_capability(&token_id, &actor.device_id, &reason, now_ms);
        // NODE-018：撤销是敏感动作，无论成败都记一条只含元数据的审计。
        let mut event = chat_node_services::audit::AuditEvent::new(
            "capability_revoke",
            now_ms,
            if result.is_ok() {
                AuditOutcome::Ok
            } else {
                AuditOutcome::Denied
            },
        )
        .with_device(actor.device_id.clone())
        .with_request_id(request_id.0.clone());
        if let Err(error) = &result {
            event = event.with_code(error.code());
        }
        service.record_audit(event);
        result
    })?;
    Ok(Json(RevokeCapabilityResponseDto::new(token_id, outcome)).into_response())
}

/// CAP-REV：强制模式下的**读端点**能力门（信箱/日志）。
///
/// 语义（与 blob 端点一致，但读端点没有对象级绑定）：
/// - 关闭强制时完全不介入（旧客户端行为不变，默认关闭）；
/// - 强制时请求必须带 `x-chat-capability`，令牌必须验签通过、绑定到**本设备**、
///   未过期且**未被撤销**；否则 `AUTH_REQUIRED` / `CAPABILITY_REVOKED`。
fn enforce_read_capability(
    state: &AppState,
    headers: &HeaderMap,
    device_id: &str,
    now: u64,
) -> Result<(), ApiError> {
    if !state.capability_required() {
        return Ok(());
    }
    let declared = capability_token(&headers)?;
    let Some(token) = declared else {
        return Err(ApiError::from_node(
            chat_node_services::error::NodeError::AuthRequired,
        ));
    };
    let grant = state.with_service(|service| service.parse_capability(&token))?;
    if grant.device_id != device_id {
        return Err(ApiError::from_node(
            chat_node_services::error::NodeError::InvalidCapability,
        ));
    }
    if now >= grant.expires_at {
        return Err(ApiError::from_node(
            chat_node_services::error::NodeError::AuthExpired,
        ));
    }
    let token_id = chat_node_services::capability::capability_token_id(&token)
        .map_err(ApiError::from_node)?;
    let revoked = state.with_service(|service| service.capability_revoked(&token_id))?;
    if revoked {
        return Err(ApiError::from_node(
            chat_node_services::error::NodeError::CapabilityRevoked,
        ));
    }
    Ok(())
}

/// POST /v1/blobs：创建密文附件上传会话（NODE-012）。
pub async fn begin_blob_upload(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    ensure_body_bounded(&state, body.len())?;
    let request: BeginBlobUploadRequestDto = parse_body(&body)?;
    let (operation_id, descriptor) = request.into_request()?;
    let now = state.now();
    let actor = authenticate(&state, &headers, &uri, "POST", &body, now)?;
    let session = state.with_service(|service| {
        service.begin_blob_upload(&descriptor, &operation_id, &actor, now)
    })?;
    Ok(Json(UploadSessionResponseDto::new(session)).into_response())
}

/// PUT /v1/blobs/{upload_id}/chunks/{index}：按序写入一块密文（NODE-013）。
///
/// 正文就是原始密文字节（`application/octet-stream`），不是 JSON/hex：
/// 附件块不需要二次编码，请求签名仍然覆盖 `sha256(正文)`。
pub async fn put_blob_chunk(
    State(state): State<AppState>,
    UrlPath((upload_id, chunk_index)): UrlPath<(String, String)>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    // 块正文是原始密文（不是 JSON DTO）：传输上限由路由级 DefaultBodyLimit 兜底
    // （MAX_BLOB_CHUNK_BODY_BYTES），具体块大小由服务层按会话声明的 chunk_bytes 与
    // BlobPolicy 校验，超限仍返回 CONTENT_TOO_LARGE / MANIFEST_MISMATCH，不在这里改错误码。
    let index = parse_chunk_index(&chunk_index)?;
    let now = state.now();
    let actor = authenticate(&state, &headers, &uri, "PUT", &body, now)?;
    // API-004/CAP：带上能力令牌时，令牌必须与 (上传会话, 本设备, blob:upload) 绑定且未过期；
    // 强制模式下**必须**带（缺失 → AUTH_REQUIRED，401），关闭模式下不传保持旧行为。
    let declared = capability_token(&headers)?;
    if declared.is_none() && state.capability_required() {
        return Err(ApiError::from_node(chat_node_services::error::NodeError::AuthRequired));
    }
    if let Some(token) = declared {
        let device_id = actor.device_id.clone();
        state.with_service(|service| {
            service.verify_capability(
                &token,
                &upload_id,
                &device_id,
                CapabilityScope::BlobUpload,
                now,
            )
        })?;
    }
    let receipt = state.with_service(|service| {
        service.put_blob_chunk(&upload_id, index, &body, &actor, now)
    })?;
    Ok(Json(ChunkReceiptResponseDto::new(receipt)).into_response())
}

/// POST /v1/blobs/{upload_id}/complete：校验清单并原子发布（NODE-014）。
pub async fn complete_blob_upload(
    State(state): State<AppState>,
    UrlPath(upload_id): UrlPath<String>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    ensure_body_bounded(&state, body.len())?;
    let request: CompleteBlobUploadRequestDto = parse_body(&body)?;
    let manifest = request.into_manifest()?;
    let now = state.now();
    let actor = authenticate(&state, &headers, &uri, "POST", &body, now)?;
    let receipt = state
        .with_service(|service| service.complete_blob_upload(&upload_id, &manifest, &actor, now))?;
    Ok(Json(BlobReceiptResponseDto::new(receipt)).into_response())
}

/// GET /v1/blobs/{blob_id}/chunks/{index}：取回一块密文（NODE-015）。
///
/// 成功时正文是原始密文块（`application/octet-stream`）；失败仍然返回 JSON 错误体。
pub async fn read_blob_chunk(
    State(state): State<AppState>,
    UrlPath((blob_id, chunk_index)): UrlPath<(String, String)>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let index = parse_chunk_index(&chunk_index)?;
    let now = state.now();
    let actor = authenticate(&state, &headers, &uri, "GET", b"", now)?;
    // API-004/CAP：带上能力令牌时，令牌必须与 (对象, 本设备, blob:download) 绑定且未过期；
    // 强制模式下必须带（缺失 → AUTH_REQUIRED）。
    let declared = capability_token(&headers)?;
    if declared.is_none() && state.capability_required() {
        return Err(ApiError::from_node(chat_node_services::error::NodeError::AuthRequired));
    }
    if let Some(token) = declared {
        let device_id = actor.device_id.clone();
        state.with_service(|service| {
            service.verify_capability(
                &token,
                &blob_id,
                &device_id,
                CapabilityScope::BlobDownload,
                now,
            )
        })?;
    }
    let chunk = state
        .with_service(|service| service.read_blob_chunk(&blob_id, index, &actor, now))?;
    Ok((
        [(CONTENT_TYPE, "application/octet-stream")],
        chunk.bytes,
    )
        .into_response())
}
/// GET /v1/health：最小健康检查（匿名、不鉴权、不限流）。
///
/// 只返回节点公开身份与 API schema 版本；不返回密钥、配置、库路径或任何内部状态。
pub async fn health(State(state): State<AppState>) -> Result<Response, ApiError> {
    let (node_id, node_public_key) = state.node_identity()?;
    Ok(Json(HealthResponseDto::new(node_id, hex_encode(&node_public_key))).into_response())
}
