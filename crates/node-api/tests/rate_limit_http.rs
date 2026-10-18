//! API-005 限流的真 TCP 契约：匿名维度与设备维度都要返回 429 RATE_LIMITED + retry_after。
//!
//! 服务端是真实 node-api（axum 真端口），请求用原始 HTTP/1.1 发送，签名走真实 API-003。
use std::sync::Arc;

use chat_node_api::build_router;
use chat_node_api::state::{AppState, FixedClock};
use chat_node_services::auth::{AuthChallenge, ChallengePurpose, RequestMeta, request_signing_bytes};
use chat_node_services::canonical::{hex_decode, hex_encode, sha256};
use chat_node_services::limits::RateLimitPolicy;
use chat_node_services::registration::{
    DeviceCredential, DeviceRegistration, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest,
    RegistrationCapability,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use chat_protocol::DeviceId;
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
    supervisor: SigningKey,
    account_id: String,
    node_id: String,
}

/// 带显式限流策略起节点。
async fn start_node(rate_limit: RateLimitPolicy) -> Node {
    let policy = NodeAuthPolicy {
        rate_limit,
        ..NodeAuthPolicy::default()
    };
    let service = NodeService::open_in_memory(NodeServiceConfig {
        node_id: "node-rate-limit".into(),
        signing_seed: [7_u8; 32],
        policy,
    })
    .expect("node service opens");
    let state = AppState::new(service, Arc::new(FixedClock::new(START)));
    let router = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("local addr").to_string();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Node {
        address,
        supervisor: SigningKey::from_bytes(&[11_u8; 32]),
        account_id: "acct-rate".into(),
        node_id: "node-rate-limit".into(),
    }
}

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!(
                "response is not json: {error} body={:?}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    fn error_code(&self) -> String {
        self.json()["error"]["code"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
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
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .expect("status code");
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect();
    HttpResponse {
        status,
        headers,
        body: raw[header_end + 4..].to_vec(),
    }
}

fn device_text(seed: u8) -> String {
    DeviceId::from_bytes([seed; 32]).encode()
}

fn device(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

async fn issue_challenge(node: &Node, purpose: &str) -> AuthChallenge {
    let body = serde_json::to_vec(&serde_json::json!({ "schema_version": 1, "purpose": purpose }))
        .expect("challenge json");
    let response = http(&node.address, "POST", "/v1/auth/challenge", &[], &body).await;
    assert_eq!(
        response.status,
        200,
        "challenge: {:?}",
        String::from_utf8_lossy(&response.body)
    );
    serde_json::from_value(response.json()["challenge"].clone()).expect("challenge dto")
}

fn auth_headers(
    node: &Node,
    challenge: &AuthChallenge,
    seed: u8,
    purpose: ChallengePurpose,
    method: &str,
    path: &str,
    body: &[u8],
) -> Vec<(String, String)> {
    let nonce: [u8; 32] = hex_decode::<32>("nonce", &challenge.nonce).expect("nonce");
    let meta = RequestMeta::canonical(method, path, "").expect("meta");
    let signed = request_signing_bytes(
        &device_text(seed),
        &meta,
        &sha256(body),
        &nonce,
        &node.node_id,
        purpose,
    );
    let signature: Signature = device(seed).sign(&signed);
    vec![
        (DEVICE_HEADER.to_owned(), device_text(seed)),
        (CHALLENGE_HEADER.to_owned(), challenge.challenge_id.clone()),
        (NONCE_HEADER.to_owned(), challenge.nonce.clone()),
        (SIGNATURE_HEADER.to_owned(), hex_encode(&signature.to_bytes())),
    ]
}

async fn register_device(node: &Node, seed: u8) -> DeviceRegistration {
    let device_id = device_text(seed);
    let key = device(seed);
    let mut credential = DeviceCredential {
        account_id: node.account_id.clone(),
        device_id: device_id.clone(),
        device_public_key: hex_encode(&key.verifying_key().to_bytes()),
        endpoint_id: format!("endpoint-{seed}"),
        credential_version: 1,
        not_before: START - 10,
        expires_at: START + 3_600,
        supervisor_public_key: hex_encode(&node.supervisor.verifying_key().to_bytes()),
        signature: String::new(),
    };
    let signature: Signature = node
        .supervisor
        .sign(&credential.signing_bytes().expect("credential bytes"));
    credential.signature = hex_encode(&signature.to_bytes());
    let mut capability = RegistrationCapability {
        capability_id: format!("cap-{seed}"),
        account_id: node.account_id.clone(),
        device_id: device_id.clone(),
        purpose: REGISTRATION_CAPABILITY_PURPOSE.into(),
        issued_at: START - 10,
        expires_at: START + 600,
        revocation_version: 1,
        supervisor_public_key: hex_encode(&node.supervisor.verifying_key().to_bytes()),
        signature: String::new(),
    };
    let signature: Signature = node
        .supervisor
        .sign(&capability.signing_bytes().expect("capability bytes"));
    capability.signature = hex_encode(&signature.to_bytes());
    let request = RegisterDeviceRequest {
        account_id: node.account_id.clone(),
        credential,
        capability,
    };
    let body = serde_json::to_vec(&request).expect("registration json");
    let challenge = issue_challenge(node, "device_registration").await;
    let headers = auth_headers(
        node,
        &challenge,
        seed,
        ChallengePurpose::DeviceRegistration,
        "POST",
        "/v1/devices",
        &body,
    );
    let response = http(&node.address, "POST", "/v1/devices", &headers, &body).await;
    assert_eq!(
        response.status,
        200,
        "registration: {:?}",
        String::from_utf8_lossy(&response.body)
    );
    serde_json::from_value(response.json()["registration"].clone()).expect("registration dto")
}

/// 一次已登记设备的 GET（用一个必然不存在的群，只为消耗设备预算）。
async fn authenticated_get(node: &Node, seed: u8) -> HttpResponse {
    let path = "/v1/groups/grp:1:0000000000000000000000000000000000000000000000000000000000000000/head";
    let challenge = issue_challenge(node, "request_auth").await;
    let headers = auth_headers(node, &challenge, seed, ChallengePurpose::RequestAuth, "GET", path, b"");
    http(&node.address, "GET", path, &headers, b"").await
}

fn tight_policy() -> RateLimitPolicy {
    RateLimitPolicy {
        enabled: true,
        window_seconds: 60,
        max_requests_per_device: 2,
        max_requests_per_account: 4,
        max_requests_anonymous: 100,
        max_bytes_per_device: 1024,
        max_buckets: 64,
    }
}

#[tokio::test]
async fn anonymous_endpoints_return_429_with_retry_after() {
    let node = start_node(RateLimitPolicy {
        max_requests_anonymous: 2,
        ..tight_policy()
    })
    .await;
    for attempt in 0..2 {
        let body = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "purpose": "request_auth",
        }))
        .expect("json");
        let response = http(&node.address, "POST", "/v1/auth/challenge", &[], &body).await;
        assert_eq!(response.status, 200, "attempt {attempt}");
    }
    let body = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "purpose": "request_auth",
    }))
    .expect("json");
    let limited = http(&node.address, "POST", "/v1/auth/challenge", &[], &body).await;
    assert_eq!(limited.status, 429);
    assert_eq!(limited.error_code(), "RATE_LIMITED");
    // body 与 HTTP 头都带 retry_after（毫秒 / 秒）。
    let retry_after_ms = limited.json()["error"]["retry_after_ms"]
        .as_u64()
        .expect("retry_after_ms in body");
    assert!(retry_after_ms >= 1_000, "retry_after_ms={retry_after_ms}");
    let header = limited.header("retry-after").expect("retry-after header");
    let seconds: u64 = header.parse().expect("retry-after is seconds");
    assert!(seconds >= 1);
    // 健康检查不受限流影响。
    let health = http(&node.address, "GET", "/v1/health", &[], b"").await;
    assert_eq!(health.status, 200);
}

#[tokio::test]
async fn device_endpoints_return_429_after_the_device_budget() {
    let node = start_node(tight_policy()).await;
    register_device(&node, 21).await;
    // 设备预算 2：前两次鉴权请求允许（这里用“未知群 head”，返回 404 也算消耗预算）。
    for attempt in 0..2 {
        let response = authenticated_get(&node, 21).await;
        assert_eq!(response.status, 404, "attempt {attempt}");
        assert_eq!(response.error_code(), "GROUP_NOT_FOUND");
    }
    let limited = authenticated_get(&node, 21).await;
    assert_eq!(limited.status, 429);
    assert_eq!(limited.error_code(), "RATE_LIMITED");
    let retry_after_ms = limited.json()["error"]["retry_after_ms"]
        .as_u64()
        .expect("retry_after_ms in body");
    assert!(retry_after_ms >= 1_000);
    assert!(limited.header("retry-after").is_some());
    // 其它设备仍有自己的预算。
    register_device(&node, 22).await;
    let other = authenticated_get(&node, 22).await;
    assert_eq!(other.status, 404);
}

#[tokio::test]
async fn health_is_never_rate_limited() {
    let node = start_node(RateLimitPolicy {
        max_requests_anonymous: 1,
        ..tight_policy()
    })
    .await;
    for _ in 0..20 {
        let response = http(&node.address, "GET", "/v1/health", &[], b"").await;
        assert_eq!(response.status, 200);
        let body = response.json();
        assert_eq!(body["status"], "ok");
        assert_eq!(body["node_id"], "node-rate-limit");
        assert_eq!(body["schema_version"], 1);
        // 健康检查不含密钥材料。
        let text = String::from_utf8_lossy(&response.body).to_string();
        assert!(!text.contains("signing"));
        assert!(!text.contains("secret"));
    }
}
