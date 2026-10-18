//! 脚本化最小 HTTP/1.1 服务：证明客户端的重取一次、响应上限、超时与网络失败语义。
//!
//! 这些边界（尤其是“只重试一次”“不无限缓冲”“不跟随重定向”）用真实节点难以稳定复现，
//! 因此这里用受控服务端逐条断言；真节点链路见 tests/http_flow.rs。
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chat_node_client::{ClientLimits, NodeClient, NodeClientErrorKind, NodeEndpoint};
use chat_node_services::auth::ChallengePurpose;
use ed25519_dalek::SigningKey;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const GROUP_SEED: u8 = 31;

fn group_text() -> String {
    chat_protocol::GroupId::from_bytes([GROUP_SEED; 32]).encode()
}

fn hex32(byte: u8) -> String {
    (0..32).map(|_| format!("{byte:02x}")).collect()
}

fn hex64(byte: u8) -> String {
    (0..64).map(|_| format!("{byte:02x}")).collect()
}

fn challenge_body(node_id: &str, purpose: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "challenge": {
            "challenge_id": "chal-fake",
            "nonce": hex32(3),
            "node_id": node_id,
            "purpose": purpose,
            "issued_at": 1_700_000_000_u64,
            "expires_at": 1_700_000_060_u64,
            "node_public_key": hex32(9),
            "signature": hex64(0),
        },
    }))
    .expect("challenge json")
}

fn head_body(node_id: &str, group_id: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "head": {
            "group_id": group_id,
            "coordinator_id": node_id,
            "seq": 7,
            "log_hash": hex32(1),
            "control_head": hex32(2),
            "policy_version": 1,
            "mls_epoch": 0,
            "node_id": node_id,
            "node_public_key": hex32(9),
            "signature": hex64(0),
        },
    }))
    .expect("head json")
}

fn error_body(code: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "error": { "code": code, "message": "scripted failure" },
    }))
    .expect("error json")
}

#[derive(Clone, Debug)]
struct FakeRequest {
    method: String,
    target: String,
}

#[derive(Clone, Debug)]
struct FakeResponse {
    status: u16,
    body: Vec<u8>,
    delay: Duration,
    chunked: bool,
    location: Option<String>,
}

impl FakeResponse {
    fn json(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            body,
            delay: Duration::ZERO,
            chunked: false,
            location: None,
        }
    }

    fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    fn chunked(mut self) -> Self {
        self.chunked = true;
        self
    }

    fn redirect(status: u16, location: &str) -> Self {
        Self {
            status,
            body: Vec::new(),
            delay: Duration::ZERO,
            chunked: false,
            location: Some(location.to_owned()),
        }
    }
}

#[derive(Default, Debug)]
struct Counters {
    challenge: usize,
    signed: usize,
}

type Handler = Box<dyn Fn(&FakeRequest, &mut Counters) -> FakeResponse + Send + Sync>;

struct FakeNode {
    url: String,
    counters: Arc<Mutex<Counters>>,
    targets: Arc<Mutex<Vec<String>>>,
}

impl FakeNode {
    fn endpoint(&self, node_id: &str) -> NodeEndpoint {
        NodeEndpoint::new(&self.url, node_id).expect("endpoint")
    }

    fn challenge_count(&self) -> usize {
        self.counters.lock().expect("counters").challenge
    }

    fn signed_count(&self) -> usize {
        self.counters.lock().expect("counters").signed
    }

    fn targets(&self) -> Vec<String> {
        self.targets.lock().expect("targets").clone()
    }
}

async fn spawn_fake(handler: Handler) -> FakeNode {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind fake node");
    let address = listener.local_addr().expect("local addr").to_string();
    let counters = Arc::new(Mutex::new(Counters::default()));
    let targets = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(handler);
    let shared_counters = Arc::clone(&counters);
    let shared_targets = Arc::clone(&targets);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let handler = Arc::clone(&handler);
            let counters = Arc::clone(&shared_counters);
            let targets = Arc::clone(&shared_targets);
            tokio::spawn(async move {
                let Some((head, _body)) = read_request(&mut socket).await else {
                    return;
                };
                let request = parse_request(&head);
                targets
                    .lock()
                    .expect("targets")
                    .push(request.target.clone());
                let response = {
                    let mut counters = counters.lock().expect("counters");
                    if request.target.starts_with("/v1/auth/challenge") {
                        counters.challenge += 1;
                    } else {
                        counters.signed += 1;
                    }
                    handler(&request, &mut counters)
                };
                if !response.delay.is_zero() {
                    tokio::time::sleep(response.delay).await;
                }
                write_response(&mut socket, &response).await;
            });
        }
    });
    FakeNode {
        url: format!("http://{address}"),
        counters,
        targets,
    }
}

async fn read_request(socket: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&buffer[..position]).to_string();
        let content_length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if name.eq_ignore_ascii_case("content-length") {
                    value.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0);
        let expected = position + 4 + content_length;
        while buffer.len() < expected {
            let read = socket.read(&mut chunk).await.ok()?;
            if read == 0 {
                return None;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        return Some((head, buffer[position + 4..expected].to_vec()));
    }
}

fn parse_request(head: &str) -> FakeRequest {
    let line = head.lines().next().unwrap_or_default();
    let mut parts = line.split_whitespace();
    FakeRequest {
        method: parts.next().unwrap_or_default().to_owned(),
        target: parts.next().unwrap_or_default().to_owned(),
    }
}

async fn write_response(socket: &mut TcpStream, response: &FakeResponse) {
    let mut head = format!(
        "HTTP/1.1 {} Scripted\r\nContent-Type: application/json\r\nConnection: close\r\n",
        response.status
    );
    if let Some(location) = &response.location {
        head.push_str(&format!("Location: {location}\r\n"));
    }
    if response.chunked {
        head.push_str("Transfer-Encoding: chunked\r\n\r\n");
        if socket.write_all(head.as_bytes()).await.is_err() {
            return;
        }
        for chunk in response.body.chunks(4096) {
            let size = format!("{:x}\r\n", chunk.len());
            if socket.write_all(size.as_bytes()).await.is_err() {
                return;
            }
            if socket.write_all(chunk).await.is_err() || socket.write_all(b"\r\n").await.is_err() {
                return;
            }
        }
        let _ = socket.write_all(b"0\r\n\r\n").await;
    } else {
        head.push_str(&format!("Content-Length: {}\r\n\r\n", response.body.len()));
        if socket.write_all(head.as_bytes()).await.is_err() {
            return;
        }
        let _ = socket.write_all(&response.body).await;
    }
    let _ = socket.flush().await;
    let _ = socket.shutdown().await;
}

fn client_for(node: &FakeNode, node_id: &str, limits: ClientLimits) -> NodeClient {
    NodeClient::with_limits(
        node.endpoint(node_id),
        "dev-fake",
        SigningKey::from_bytes(&[21_u8; 32]),
        limits,
    )
    .expect("client")
}

#[tokio::test]
async fn auth_failure_is_retried_once_and_then_succeeds() {
    let group = group_text();
    let fake = spawn_fake(Box::new(move |request, counters| {
        if request.method == "POST" && request.target.starts_with("/v1/auth/challenge") {
            FakeResponse::json(200, challenge_body("node-fake", "request_auth"))
        } else if request.target.ends_with("/head") {
            if counters.signed == 1 {
                // 第一次签名请求：挑战已过期，客户端应重取一次。
                FakeResponse::json(401, error_body("AUTH_EXPIRED"))
            } else {
                FakeResponse::json(200, head_body("node-fake", &group))
            }
        } else {
            FakeResponse::json(404, error_body("INVALID_REQUEST"))
        }
    }))
    .await;
    let client = client_for(&fake, "node-fake", ClientLimits::default());
    let head = client.get_head(&group_text()).await.expect("retry succeeds");
    assert_eq!(head.seq, 7);
    assert_eq!(fake.challenge_count(), 2, "a fresh challenge per attempt");
    assert_eq!(fake.signed_count(), 2, "exactly one retry");
}

#[tokio::test]
async fn retry_budget_is_exactly_one_retry() {
    let fake = spawn_fake(Box::new(|request, _counters| {
        if request.target.starts_with("/v1/auth/challenge") {
            FakeResponse::json(200, challenge_body("node-fake", "request_auth"))
        } else {
            FakeResponse::json(401, error_body("AUTH_REPLAY"))
        }
    }))
    .await;
    let client = client_for(&fake, "node-fake", ClientLimits::default());
    let error = client
        .get_head(&group_text())
        .await
        .expect_err("repeated auth failure is reported");
    assert_eq!(error.node_code(), Some("AUTH_REPLAY"));
    assert_eq!(error.kind(), NodeClientErrorKind::Authentication);
    assert_eq!(fake.challenge_count(), 2, "two attempts, never more");
    assert_eq!(fake.signed_count(), 2);
}

#[tokio::test]
async fn single_attempt_budget_sends_one_request() {
    let fake = spawn_fake(Box::new(|request, _counters| {
        if request.target.starts_with("/v1/auth/challenge") {
            FakeResponse::json(200, challenge_body("node-fake", "request_auth"))
        } else {
            FakeResponse::json(401, error_body("AUTH_EXPIRED"))
        }
    }))
    .await;
    let limits = ClientLimits {
        challenge_attempts: 1,
        ..ClientLimits::default()
    };
    let client = client_for(&fake, "node-fake", limits);
    let error = client.get_head(&group_text()).await.expect_err("no retry");
    assert_eq!(error.node_code(), Some("AUTH_EXPIRED"));
    assert_eq!(fake.challenge_count(), 1);
    assert_eq!(fake.signed_count(), 1);
}

#[tokio::test]
async fn oversized_response_with_content_length_is_rejected() {
    let fake = spawn_fake(Box::new(|request, _counters| {
        if request.target.starts_with("/v1/auth/challenge") {
            let mut body = challenge_body("node-fake", "request_auth");
            body.resize(2 * 1024 * 1024, b' ');
            FakeResponse::json(200, body)
        } else {
            FakeResponse::json(500, error_body("INTERNAL_FAILURE"))
        }
    }))
    .await;
    let limits = ClientLimits {
        max_response_bytes: 64 * 1024,
        ..ClientLimits::default()
    };
    let client = client_for(&fake, "node-fake", limits);
    let error = client
        .fetch_challenge(ChallengePurpose::RequestAuth)
        .await
        .expect_err("oversized response rejected");
    assert_eq!(error.kind(), NodeClientErrorKind::Limit);
    assert_eq!(error.code(), chat_protocol::ErrorCode::ContentTooLarge);
}

#[tokio::test]
async fn oversized_chunked_response_is_rejected_by_incremental_budget() {
    let fake = spawn_fake(Box::new(|request, _counters| {
        if request.target.starts_with("/v1/auth/challenge") {
            let mut body = challenge_body("node-fake", "request_auth");
            body.resize(2 * 1024 * 1024, b' ');
            // 没有 Content-Length：只能靠分块累计预算拒绝。
            FakeResponse::json(200, body).chunked()
        } else {
            FakeResponse::json(500, error_body("INTERNAL_FAILURE"))
        }
    }))
    .await;
    let limits = ClientLimits {
        max_response_bytes: 64 * 1024,
        ..ClientLimits::default()
    };
    let client = client_for(&fake, "node-fake", limits);
    let error = client
        .fetch_challenge(ChallengePurpose::RequestAuth)
        .await
        .expect_err("oversized chunked response rejected");
    assert_eq!(error.kind(), NodeClientErrorKind::Limit);
    assert_eq!(error.code(), chat_protocol::ErrorCode::ContentTooLarge);
}

#[tokio::test]
async fn signed_request_timeout_is_retried_once_then_reported() {
    let group = group_text();
    let fake = spawn_fake(Box::new(move |request, _counters| {
        if request.target.starts_with("/v1/auth/challenge") {
            FakeResponse::json(200, challenge_body("node-fake", "request_auth"))
        } else {
            // 签名请求一直不返回：客户端必须在超时后重取挑战一次，再用尽预算报错。
            FakeResponse::json(200, head_body("node-fake", &group)).delayed(Duration::from_secs(2))
        }
    }))
    .await;
    let limits = ClientLimits {
        timeout: Duration::from_millis(150),
        ..ClientLimits::default()
    };
    let client = client_for(&fake, "node-fake", limits);
    let started = Instant::now();
    let error = client
        .get_head(&group_text())
        .await
        .expect_err("timeout reported");
    let elapsed = started.elapsed();
    assert_eq!(error.kind(), NodeClientErrorKind::Timeout);
    assert_eq!(error.code(), chat_protocol::ErrorCode::NetworkUnavailable);
    // NETWORK_UNAVAILABLE 在 PROTOCOL-014 里是可重试码；客户端只允许一次重取，
    // 所以这里既证明“会重试”，也证明“不会无限重试”。
    assert!(error.is_retryable());
    assert_eq!(fake.challenge_count(), 2, "one retry, then a stable error");
    assert_eq!(fake.signed_count(), 2);
    assert!(elapsed < Duration::from_secs(5), "timeout must be bounded: {elapsed:?}");
}

#[tokio::test]
async fn anonymous_challenge_timeout_is_reported_without_retry() {
    let fake = spawn_fake(Box::new(|_request, _counters| {
        FakeResponse::json(200, challenge_body("node-fake", "request_auth"))
            .delayed(Duration::from_secs(2))
    }))
    .await;
    let limits = ClientLimits {
        timeout: Duration::from_millis(150),
        ..ClientLimits::default()
    };
    let client = client_for(&fake, "node-fake", limits);
    let error = client
        .fetch_challenge(ChallengePurpose::RequestAuth)
        .await
        .expect_err("timeout reported");
    assert_eq!(error.kind(), NodeClientErrorKind::Timeout);
    // 匿名取挑战没有“上一份挑战”需要重取，因此只发一次请求。
    assert_eq!(fake.challenge_count(), 1);
    assert_eq!(fake.signed_count(), 0);
}
#[tokio::test]
async fn network_failure_is_reported_and_bounded() {
    // 端口 1 上没有监听者：连接立即失败（不是“等待直到超时”）。
    let endpoint = NodeEndpoint::new("http://127.0.0.1:1", "node-fake").expect("endpoint");
    let client = NodeClient::new(endpoint, "dev-fake", SigningKey::from_bytes(&[21_u8; 32]))
        .expect("client");
    let started = Instant::now();
    let error = client
        .fetch_challenge(ChallengePurpose::RequestAuth)
        .await
        .expect_err("connection failure reported");
    let elapsed = started.elapsed();
    assert_eq!(error.kind(), NodeClientErrorKind::Network);
    assert_eq!(error.code(), chat_protocol::ErrorCode::NetworkUnavailable);
    assert!(error.http_status().is_none());
    assert!(elapsed < Duration::from_secs(5), "network failure must be bounded: {elapsed:?}");
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let fake = spawn_fake(Box::new(|_request, _counters| {
        FakeResponse::redirect(302, "http://127.0.0.1:1/elsewhere")
    }))
    .await;
    let client = client_for(&fake, "node-fake", ClientLimits::default());
    let error = client
        .fetch_challenge(ChallengePurpose::RequestAuth)
        .await
        .expect_err("redirect is not followed");
    assert_eq!(error.kind(), NodeClientErrorKind::Protocol);
    assert_eq!(error.http_status(), Some(302));
    assert_eq!(
        fake.targets().len(),
        1,
        "a redirect must not produce a second request"
    );
}

#[tokio::test]
async fn wrong_node_id_in_challenge_is_rejected() {
    let fake = spawn_fake(Box::new(|_request, _counters| {
        FakeResponse::json(200, challenge_body("node-impostor", "request_auth"))
    }))
    .await;
    let client = client_for(&fake, "node-fake", ClientLimits::default());
    let error = client
        .fetch_challenge(ChallengePurpose::RequestAuth)
        .await
        .expect_err("challenge from another node rejected");
    assert_eq!(error.kind(), NodeClientErrorKind::Protocol);
    assert_eq!(error.code(), chat_protocol::ErrorCode::InvalidTarget);
}

#[tokio::test]
async fn unknown_error_code_is_not_silently_treated_as_success() {
    let fake = spawn_fake(Box::new(|request, _counters| {
        if request.target.starts_with("/v1/auth/challenge") {
            FakeResponse::json(200, challenge_body("node-fake", "request_auth"))
        } else {
            FakeResponse::json(409, error_body("SOMETHING_NEW"))
        }
    }))
    .await;
    let client = client_for(&fake, "node-fake", ClientLimits::default());
    let error = client
        .get_head(&group_text())
        .await
        .expect_err("unknown code is a failure");
    assert_eq!(error.code(), chat_protocol::ErrorCode::UnknownResult);
    assert_eq!(error.node_code(), Some("UNKNOWN_NODE_CODE"));
    assert_eq!(error.http_status(), Some(409));
    assert!(!error.is_retryable());
}
