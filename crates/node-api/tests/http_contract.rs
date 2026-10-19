//! /v1/auth/challenge 与 /v1/devices 的真 TCP 端到端契约测试。
//!
//! 用 axum::serve 绑定本地随机端口，测试用原始 HTTP/1.1 客户端收发，
//! 因此覆盖路由、正文限长层、DTO 解码、鉴权与错误映射的完整链路。
use std::sync::Arc;

use chat_node_api::state::{AppState, Clock, FixedClock};
use chat_node_api::build_router;
use chat_node_services::auth::{
    ChallengePurpose, RequestMeta, request_signing_bytes, verify_auth_challenge,
};
use chat_node_services::canonical::{hex_decode, hex_encode, sha256};
use chat_node_services::registration::{
    DeviceCredential, DeviceRegistration, REGISTRATION_CAPABILITY_PURPOSE,
    RegisterDeviceRequest, RegistrationCapability,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use ed25519_dalek::{Signature, Signer, SigningKey};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const START: u64 = 1_700_000_000;
const DEVICE_HEADER: &str = "x-chat-device-id";
const CHALLENGE_HEADER: &str = "x-chat-challenge-id";
const NONCE_HEADER: &str = "x-chat-challenge-nonce";
const SIGNATURE_HEADER: &str = "x-chat-signature";

struct Node {
    address: String,
    /// 与路由共享的状态句柄：用于断言审计记录（只读）。
    state: AppState,
    clock: Arc<FixedClock>,
    supervisor: SigningKey,
    account_id: String,
    node_id: String,
    node_public_key: [u8; 32],
}

async fn spawn(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("local addr").to_string();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    address
}

async fn start_node(max_devices: usize, max_request_body_bytes: usize) -> Node {
    let policy = NodeAuthPolicy {
        max_devices_per_account: max_devices,
        max_request_body_bytes,
        ..NodeAuthPolicy::default()
    };
    let seed = [7_u8; 32];
    let service = NodeService::open_in_memory(NodeServiceConfig {
        node_id: "node-http-1".into(),
        signing_seed: seed,
        policy,
    })
    .expect("node service opens");
    let node_public_key = service.node_public_key();
    let clock = Arc::new(FixedClock::new(START));
    let state = AppState::new(service, clock.clone());
    let probe = state.clone();
    let address = spawn(build_router(state)).await;
    Node {
        address,
        state: probe,
        clock,
        supervisor: SigningKey::from_bytes(&[11_u8; 32]),
        account_id: "acct-http-1".into(),
        node_id: "node-http-1".into(),
        node_public_key,
    }
}

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|error| panic!("response is not json: {error} body={:?}", String::from_utf8_lossy(&self.body)))
    }

    fn error_code(&self) -> String {
        self.json()["error"]["code"].as_str().unwrap_or_default().to_owned()
    }

    /// 按名字读响应头（大小写不敏感）。
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

async fn http(
    address: &str,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> HttpResponse {
    let mut stream = TcpStream::connect(address).await.expect("connect");
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Length: {}\r\ncontent-type: application/json\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let mut raw = head.into_bytes();
    raw.extend_from_slice(body);
    stream.write_all(&raw).await.expect("write request");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.expect("read response");
    parse_response(&response)
}

fn parse_response(raw: &[u8]) -> HttpResponse {
    let header_end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response header terminator");
    let head = String::from_utf8_lossy(&raw[..header_end]);
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .expect("status code");
    let headers: Vec<(String, String)> = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect();
    HttpResponse {
        status,
        headers,
        body: raw[header_end + 4..].to_vec(),
    }
}

async fn issue_challenge(node: &Node, purpose: &str) -> HttpResponse {
    let body = serde_json::json!({ "schema_version": 1, "purpose": purpose });
    http(
        &node.address,
        "POST",
        "/v1/auth/challenge",
        &[],
        &serde_json::to_vec(&body).expect("json"),
    )
    .await
}

fn device(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn credential(node: &Node, device_id: &str, device: &SigningKey, now: u64) -> DeviceCredential {
    let mut credential = DeviceCredential {
        account_id: node.account_id.clone(),
        device_id: device_id.to_owned(),
        device_public_key: hex_encode(&device.verifying_key().to_bytes()),
        endpoint_id: format!("endpoint-{device_id}"),
        credential_version: 1,
        not_before: now.saturating_sub(10),
        expires_at: now + 3_600,
        supervisor_public_key: hex_encode(&node.supervisor.verifying_key().to_bytes()),
        signature: String::new(),
    };
    let signature: Signature = node
        .supervisor
        .sign(&credential.signing_bytes().expect("credential bytes"));
    credential.signature = hex_encode(&signature.to_bytes());
    credential
}

fn capability(node: &Node, capability_id: &str, device_id: &str, now: u64) -> RegistrationCapability {
    let mut capability = RegistrationCapability {
        capability_id: capability_id.to_owned(),
        account_id: node.account_id.clone(),
        device_id: device_id.to_owned(),
        purpose: REGISTRATION_CAPABILITY_PURPOSE.into(),
        issued_at: now.saturating_sub(10),
        expires_at: now + 600,
        revocation_version: 1,
        supervisor_public_key: hex_encode(&node.supervisor.verifying_key().to_bytes()),
        signature: String::new(),
    };
    let signature: Signature = node
        .supervisor
        .sign(&capability.signing_bytes().expect("capability bytes"));
    capability.signature = hex_encode(&signature.to_bytes());
    capability
}

fn registration_request(
    node: &Node,
    device_id: &str,
    device: &SigningKey,
    capability_id: &str,
    now: u64,
) -> RegisterDeviceRequest {
    RegisterDeviceRequest {
        account_id: node.account_id.clone(),
        credential: credential(node, device_id, device, now),
        capability: capability(node, capability_id, device_id, now),
    }
}

fn auth_headers(
    device: &SigningKey,
    device_id: &str,
    challenge: &Value,
    method: &str,
    path: &str,
    node_id: &str,
    body_hash: [u8; 32],
) -> Vec<(String, String)> {
    let nonce = challenge["nonce"].as_str().expect("nonce");
    let nonce_bytes = hex_decode::<32>("nonce", nonce).expect("nonce hex");
    let meta = RequestMeta::canonical(method, path, "").expect("meta");
    let signed = request_signing_bytes(
        device_id,
        &meta,
        &body_hash,
        &nonce_bytes,
        node_id,
        ChallengePurpose::DeviceRegistration,
    );
    let signature: Signature = device.sign(&signed);
    vec![
        (DEVICE_HEADER.to_owned(), device_id.to_owned()),
        (
            CHALLENGE_HEADER.to_owned(),
            challenge["challenge_id"].as_str().expect("challenge id").to_owned(),
        ),
        (NONCE_HEADER.to_owned(), nonce.to_owned()),
        (SIGNATURE_HEADER.to_owned(), hex_encode(&signature.to_bytes())),
    ]
}

/// 走完整链路注册一台设备：取挑战 -> 构造请求 -> 签名 -> POST /v1/devices。
async fn register(node: &Node, device_id: &str, device: &SigningKey, capability_id: &str) -> HttpResponse {
    let request = registration_request(node, device_id, device, capability_id, node.clock.now_unix());
    let body = serde_json::to_vec(&request).expect("request json");
    let challenge_response = issue_challenge(node, "device_registration").await;
    assert_eq!(challenge_response.status, 200, "challenge issued");
    let challenge = challenge_response.json()["challenge"].clone();
    let headers = auth_headers(
        device,
        device_id,
        &challenge,
        "POST",
        "/v1/devices",
        &node.node_id,
        sha256(&body),
    );
    http(&node.address, "POST", "/v1/devices", &headers, &body).await
}

#[tokio::test]
async fn challenge_endpoint_returns_signed_short_lived_challenge() {
    let node = start_node(3, NodeAuthPolicy::default().max_request_body_bytes).await;
    let response = issue_challenge(&node, "device_registration").await;
    assert_eq!(response.status, 200);
    let body = response.json();
    assert_eq!(body["schema_version"], 1);
    let challenge: chat_node_services::auth::AuthChallenge =
        serde_json::from_value(body["challenge"].clone()).expect("challenge dto");
    verify_auth_challenge(&challenge, &node.node_public_key).expect("node-signed challenge");
    assert_eq!(challenge.node_id, node.node_id);
    assert_eq!(challenge.purpose, ChallengePurpose::DeviceRegistration);
    assert_eq!(challenge.issued_at, START);
    assert_eq!(challenge.expires_at, START + NodeAuthPolicy::default().challenge_ttl_seconds);

    // 未知用途明确拒绝，不静默降级。
    let response = issue_challenge(&node, "admin").await;
    assert_eq!(response.status, 400);
    assert_eq!(response.error_code(), "INVALID_REQUEST");

    // 不支持的 schema 版本。
    let body = serde_json::json!({ "schema_version": 2, "purpose": "device_registration" });
    let response = http(
        &node.address,
        "POST",
        "/v1/auth/challenge",
        &[],
        &serde_json::to_vec(&body).expect("json"),
    )
    .await;
    assert_eq!(response.status, 400);
    assert_eq!(response.error_code(), "UNSUPPORTED_SCHEMA_VERSION");
}

#[tokio::test]
async fn valid_signature_registers_device_and_duplicate_is_idempotent() {
    let node = start_node(3, NodeAuthPolicy::default().max_request_body_bytes).await;
    let telephone = device(21);
    let response = register(&node, "dev-1", &telephone, "cap-1").await;
    assert_eq!(response.status, 200, "body={:?}", String::from_utf8_lossy(&response.body));
    let body = response.json();
    assert_eq!(body["schema_version"], 1);
    let registration: DeviceRegistration =
        serde_json::from_value(body["registration"].clone()).expect("registration dto");
    registration.verify(&node.node_public_key).expect("receipt");
    assert_eq!(registration.device_id, "dev-1");
    assert_eq!(registration.account_id, node.account_id);

    // 同正文、同能力、全新挑战重试：返回同一份回执。
    let repeat = register(&node, "dev-1", &telephone, "cap-1").await;
    assert_eq!(repeat.status, 200);
    let repeat: DeviceRegistration =
        serde_json::from_value(repeat.json()["registration"].clone()).expect("registration dto");
    assert_eq!(repeat, registration);
}

#[tokio::test]
async fn wrong_device_key_is_rejected() {
    let node = start_node(3, NodeAuthPolicy::default().max_request_body_bytes).await;
    let genuine = device(21);
    let attacker = device(22);
    let request = registration_request(&node, "dev-1", &genuine, "cap-1", START);
    let body = serde_json::to_vec(&request).expect("json");
    let challenge = issue_challenge(&node, "device_registration").await.json()["challenge"].clone();
    let headers = auth_headers(&attacker, "dev-1", &challenge, "POST", "/v1/devices", &node.node_id, sha256(&body));
    let response = http(&node.address, "POST", "/v1/devices", &headers, &body).await;
    assert_eq!(response.status, 401);
    assert_eq!(response.error_code(), "INVALID_SIGNATURE");
}

#[tokio::test]
async fn replayed_challenge_is_rejected() {
    let node = start_node(3, NodeAuthPolicy::default().max_request_body_bytes).await;
    let telephone = device(21);
    let request = registration_request(&node, "dev-1", &telephone, "cap-1", START);
    let body = serde_json::to_vec(&request).expect("json");
    let challenge = issue_challenge(&node, "device_registration").await.json()["challenge"].clone();
    let headers = auth_headers(&telephone, "dev-1", &challenge, "POST", "/v1/devices", &node.node_id, sha256(&body));
    let first = http(&node.address, "POST", "/v1/devices", &headers, &body).await;
    assert_eq!(first.status, 200);
    let second = http(&node.address, "POST", "/v1/devices", &headers, &body).await;
    assert_eq!(second.status, 401);
    assert_eq!(second.error_code(), "AUTH_REPLAY");
}

#[tokio::test]
async fn signature_is_bound_to_method_path_body_and_node() {
    let node = start_node(3, NodeAuthPolicy::default().max_request_body_bytes).await;
    let telephone = device(21);
    let request = registration_request(&node, "dev-1", &telephone, "cap-1", START);
    let body = serde_json::to_vec(&request).expect("json");
    let hash = sha256(&body);

    // 换方法：签名覆盖 GET，实际以 POST 提交。
    let challenge = issue_challenge(&node, "device_registration").await.json()["challenge"].clone();
    let headers = auth_headers(&telephone, "dev-1", &challenge, "GET", "/v1/devices", &node.node_id, hash);
    let response = http(&node.address, "POST", "/v1/devices", &headers, &body).await;
    assert_eq!(response.status, 401);
    assert_eq!(response.error_code(), "INVALID_SIGNATURE");

    // 换路径。
    let challenge = issue_challenge(&node, "device_registration").await.json()["challenge"].clone();
    let headers = auth_headers(&telephone, "dev-1", &challenge, "POST", "/v1/auth/challenge", &node.node_id, hash);
    let response = http(&node.address, "POST", "/v1/devices", &headers, &body).await;
    assert_eq!(response.status, 401);
    assert_eq!(response.error_code(), "INVALID_SIGNATURE");

    // 换节点标识。
    let challenge = issue_challenge(&node, "device_registration").await.json()["challenge"].clone();
    let headers = auth_headers(&telephone, "dev-1", &challenge, "POST", "/v1/devices", "node-other", hash);
    let response = http(&node.address, "POST", "/v1/devices", &headers, &body).await;
    assert_eq!(response.status, 401);
    assert_eq!(response.error_code(), "INVALID_SIGNATURE");

    // 换正文：签名覆盖另一份正文，实际提交本正文。
    let other = registration_request(&node, "dev-2", &device(21), "cap-1", START);
    let other_body = serde_json::to_vec(&other).expect("json");
    let challenge = issue_challenge(&node, "device_registration").await.json()["challenge"].clone();
    let headers = auth_headers(&telephone, "dev-2", &challenge, "POST", "/v1/devices", &node.node_id, sha256(&other_body));
    let response = http(&node.address, "POST", "/v1/devices", &headers, &body).await;
    assert_eq!(response.status, 401);
    assert_eq!(response.error_code(), "INVALID_SIGNATURE");

    // 正确绑定仍然通过。
    let response = register(&node, "dev-1", &telephone, "cap-1").await;
    assert_eq!(response.status, 200);
}

#[tokio::test]
async fn expired_challenge_is_rejected() {
    let node = start_node(3, NodeAuthPolicy::default().max_request_body_bytes).await;
    let telephone = device(21);
    let request = registration_request(&node, "dev-1", &telephone, "cap-1", START);
    let body = serde_json::to_vec(&request).expect("json");
    let challenge = issue_challenge(&node, "device_registration").await.json()["challenge"].clone();
    let headers = auth_headers(&telephone, "dev-1", &challenge, "POST", "/v1/devices", &node.node_id, sha256(&body));
    node.clock.advance(NodeAuthPolicy::default().challenge_ttl_seconds);
    let response = http(&node.address, "POST", "/v1/devices", &headers, &body).await;
    assert_eq!(response.status, 401);
    assert_eq!(response.error_code(), "AUTH_EXPIRED");
}

#[tokio::test]
async fn device_limit_is_enforced_end_to_end() {
    // 默认策略：每账户 2 台设备（产品口径），第三台 409。
    let default_limit = NodeAuthPolicy::default().max_devices_per_account;
    assert_eq!(default_limit, 2);
    let node = start_node(default_limit, NodeAuthPolicy::default().max_request_body_bytes).await;
    assert_eq!(register(&node, "dev-0", &device(41), "cap-0").await.status, 200);
    assert_eq!(register(&node, "dev-1", &device(42), "cap-1").await.status, 200);
    let response = register(&node, "dev-2", &device(43), "cap-2").await;
    assert_eq!(response.status, 409);
    assert_eq!(response.error_code(), "DEVICE_LIMIT_EXCEEDED");

    // 上限可配：cap=3 时接受三台，第四台仍 409。
    let configured = start_node(3, NodeAuthPolicy::default().max_request_body_bytes).await;
    for (index, seed) in [21_u8, 22, 23].into_iter().enumerate() {
        let response = register(&configured, &format!("dev-{index}"), &device(seed), &format!("cap-{index}")).await;
        assert_eq!(response.status, 200, "device {index} accepted");
    }
    let response = register(&configured, "dev-3", &device(24), "cap-3").await;
    assert_eq!(response.status, 409);
    assert_eq!(response.error_code(), "DEVICE_LIMIT_EXCEEDED");
}

#[tokio::test]
async fn malformed_and_oversized_requests_are_rejected() {
    let node = start_node(3, 1024).await;

    // 非法 JSON。
    let response = http(&node.address, "POST", "/v1/devices", &[], b"{not json").await;
    assert_eq!(response.status, 400);
    assert_eq!(response.error_code(), "INVALID_REQUEST");

    // 缺失鉴权头。
    let telephone = device(21);
    let request = registration_request(&node, "dev-1", &telephone, "cap-1", START);
    let body = serde_json::to_vec(&request).expect("json");
    let response = http(&node.address, "POST", "/v1/devices", &[], &body).await;
    assert_eq!(response.status, 400);
    assert_eq!(response.error_code(), "INVALID_REQUEST");

    // 非法 nonce 十六进制。
    let challenge = issue_challenge(&node, "device_registration").await.json()["challenge"].clone();
    let mut headers = auth_headers(&telephone, "dev-1", &challenge, "POST", "/v1/devices", &node.node_id, sha256(&body));
    for header in headers.iter_mut() {
        if header.0 == NONCE_HEADER {
            header.1 = "zz".into();
        }
    }
    let response = http(&node.address, "POST", "/v1/devices", &headers, &body).await;
    assert_eq!(response.status, 400);
    assert_eq!(response.error_code(), "INVALID_REQUEST");

    // 超出策略正文上限（1024 字节）：413 且不进入领域逻辑。
    let mut padding = request.clone();
    padding.credential.endpoint_id = format!("endpoint-{}", "x".repeat(2_000));
    let oversized = serde_json::to_vec(&padding).expect("json");
    assert!(oversized.len() > 1024);
    let challenge = issue_challenge(&node, "device_registration").await.json()["challenge"].clone();
    let headers = auth_headers(&telephone, "dev-1", &challenge, "POST", "/v1/devices", &node.node_id, sha256(&oversized));
    let response = http(&node.address, "POST", "/v1/devices", &headers, &oversized).await;
    assert_eq!(response.status, 413);
    assert_eq!(response.error_code(), "PAYLOAD_TOO_LARGE");

    // 方法/路径不匹配由路由器明确拒绝。
    let response = http(&node.address, "GET", "/v1/devices", &[], &[]).await;
    assert_eq!(response.status, 405);
    let response = http(&node.address, "POST", "/v1/unknown", &[], b"{}").await;
    assert_eq!(response.status, 404);
}

/// NODE-018：响应带 `x-chat-request-id`，并写进审计（含调用方透传的 id）。
#[tokio::test]
async fn responses_carry_a_request_id_that_is_recorded_in_the_audit_tail() {
    let node = start_node(4, 64 * 1024).await;
    // 第一次：没有请求头 → 服务端派生一个 id，并回写响应头、写进审计。
    let response = http(&node.address, "GET", "/v1/health", &[], b"").await;
    assert_eq!(response.status, 200);
    let generated = response
        .header("x-chat-request-id")
        .expect("the response carries a request id")
        .to_owned();
    assert!(generated.starts_with("req-"), "unexpected id: {generated}");
    let tail = node.state.audit_tail(16);
    assert!(
        tail.iter().any(|event| {
            event.kind == "http"
                && event.request_id.as_deref() == Some(generated.as_str())
                && event.outcome.as_str() == "ok"
        }),
        "the request id must appear in the audit tail: {tail:?}"
    );
    // 第二次：调用方透传 id → 响应原样回显，审计里也用同一个 id。
    let headers = [("x-chat-request-id".to_owned(), "req-client-42".to_owned())];
    let response = http(&node.address, "GET", "/v1/health", &headers, b"").await;
    assert_eq!(response.status, 200);
    assert_eq!(response.header("x-chat-request-id"), Some("req-client-42"));
    let tail = node.state.audit_tail(16);
    assert!(
        tail.iter()
            .any(|event| event.request_id.as_deref() == Some("req-client-42")),
        "the propagated id must be recorded: {tail:?}"
    );
    // 审计只含元数据：没有 token/正文类字段（结构上就没有这些字段）。
    assert!(tail.iter().all(|event| !event.kind.is_empty()));
}
