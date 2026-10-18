//! 群登记/日志/信箱端点的真 TCP 端到端契约测试。
//!
//! 用 axum::serve 绑定本地随机端口，测试用原始 HTTP/1.1 客户端收发，
//! 因此覆盖路由、正文限长层、DTO 解码、API-003 鉴权与错误映射的完整链路。
use std::sync::Arc;

use chat_node_api::build_router;
use chat_node_api::state::{AppState, Clock, FixedClock};
use chat_node_services::auth::{ChallengePurpose, RequestMeta, AuthChallenge};
use chat_node_services::canonical::{hex_decode, hex_encode, sha256};
use chat_node_services::group::{
    AppendResult, CandidateControlMetadata, CandidateDomainOperation, GroupCandidate, GroupCreation,
    GroupMemberSpec, GroupRegistration, KIND_APPLICATION, KIND_CONTROL, SignedGroupCreation, SignedLogHead,
};
use chat_node_services::mailbox::{AckSummary, MailboxPage, MailboxReceipt};
use chat_node_services::registration::{
    DeviceCredential, DeviceRegistration, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest,
    RegistrationCapability,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use chat_protocol::{DeviceId, GroupId, SubmissionId, candidate_signing_bytes};
use ed25519_dalek::{Signature, Signer, SigningKey};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const START: u64 = 1_700_000_000;
const DEVICE_HEADER: &str = "x-chat-device-id";
const CHALLENGE_HEADER: &str = "x-chat-challenge-id";
const NONCE_HEADER: &str = "x-chat-challenge-nonce";
const SIGNATURE_HEADER: &str = "x-chat-signature";
const INITIAL_CONTROL_HEAD: [u8; 32] = [0x11; 32];

struct Node {
    address: String,
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

async fn start_node(policy: NodeAuthPolicy) -> Node {
    let service = NodeService::open_in_memory(NodeServiceConfig {
        node_id: "node-http-group".into(),
        signing_seed: [7_u8; 32],
        policy,
    })
    .expect("node service opens");
    let node_public_key = service.node_public_key();
    let clock = Arc::new(FixedClock::new(START));
    let state = AppState::new(service, clock.clone());
    let address = spawn(build_router(state)).await;
    Node {
        address,
        clock,
        supervisor: SigningKey::from_bytes(&[11_u8; 32]),
        account_id: "acct-http".into(),
        node_id: "node-http-group".into(),
        node_public_key,
    }
}

struct HttpResponse {
    status: u16,
    /// 响应头（小写名，便于按名查找）。
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

    /// 响应头取值（不存在时为 None）。
    fn content_type(&self) -> Option<String> {
        self.headers
            .iter()
            .find(|(name, _)| name == "content-type")
            .map(|(_, value)| value.clone())
    }
}

async fn http(
    address: &str,
    method: &str,
    path_and_query: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> HttpResponse {
    http_with(
        address,
        method,
        path_and_query,
        headers,
        body,
        "application/json",
    )
    .await
}

/// 与 http() 相同，但可指定请求 content-type（附件密文块用 octet-stream）。
async fn http_with(
    address: &str,
    method: &str,
    path_and_query: &str,
    headers: &[(String, String)],
    body: &[u8],
    content_type: &str,
) -> HttpResponse {
    let mut stream = TcpStream::connect(address).await.expect("connect");
    let mut head = format!(
        "{method} {path_and_query} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Length: {}\r\ncontent-type: {content_type}\r\n",
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

fn group_text(seed: u8) -> String {
    GroupId::from_bytes([seed; 32]).encode()
}

fn submission_text(seed: u8) -> String {
    SubmissionId::from_bytes([seed; 32]).encode()
}

fn device(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn credential(node: &Node, seed: u8, now: u64) -> DeviceCredential {
    let device_id = device_text(seed);
    let key = device(seed);
    let mut credential = DeviceCredential {
        account_id: node.account_id.clone(),
        device_id,
        device_public_key: hex_encode(&key.verifying_key().to_bytes()),
        endpoint_id: format!("endpoint-{seed}"),
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

fn capability(node: &Node, seed: u8, now: u64) -> RegistrationCapability {
    let mut capability = RegistrationCapability {
        capability_id: format!("cap-{seed}"),
        account_id: node.account_id.clone(),
        device_id: device_text(seed),
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

async fn issue_challenge(node: &Node, purpose: &str) -> AuthChallenge {
    let body = serde_json::json!({ "schema_version": 1, "purpose": purpose });
    let response = http(
        &node.address,
        "POST",
        "/v1/auth/challenge",
        &[],
        &serde_json::to_vec(&body).expect("json"),
    )
    .await;
    assert_eq!(response.status, 200, "challenge issued");
    serde_json::from_value(response.json()["challenge"].clone()).expect("challenge dto")
}

/// 用真实挑战与设备私钥签名（绑定 method/path/query/正文摘要/节点/用途）。
fn auth_headers(
    node: &Node,
    challenge: &AuthChallenge,
    key: &SigningKey,
    seed: u8,
    purpose: ChallengePurpose,
    method: &str,
    path: &str,
    query: &str,
    body: &[u8],
) -> Vec<(String, String)> {
    let nonce = hex_decode::<32>("nonce", &challenge.nonce).expect("nonce hex");
    let meta = RequestMeta::canonical(method, path, query).expect("canonical meta");
    let signed = chat_node_services::auth::request_signing_bytes(
        &device_text(seed),
        &meta,
        &sha256(body),
        &nonce,
        &node.node_id,
        purpose,
    );
    let signature: Signature = key.sign(&signed);
    vec![
        (DEVICE_HEADER.to_owned(), device_text(seed)),
        (CHALLENGE_HEADER.to_owned(), challenge.challenge_id.clone()),
        (NONCE_HEADER.to_owned(), challenge.nonce.clone()),
        (SIGNATURE_HEADER.to_owned(), hex_encode(&signature.to_bytes())),
    ]
}

async fn register_device(node: &Node, seed: u8) -> DeviceRegistration {
    let now = node.clock.now_unix();
    let request = RegisterDeviceRequest {
        account_id: node.account_id.clone(),
        credential: credential(node, seed, now),
        capability: capability(node, seed, now),
    };
    let body = serde_json::to_vec(&request).expect("request json");
    let challenge = issue_challenge(node, "device_registration").await;
    let headers = auth_headers(
        node,
        &challenge,
        &device(seed),
        seed,
        ChallengePurpose::DeviceRegistration,
        "POST",
        "/v1/devices",
        "",
        &body,
    );
    let response = http(&node.address, "POST", "/v1/devices", &headers, &body).await;
    assert_eq!(
        response.status,
        200,
        "device registers: {:?}",
        String::from_utf8_lossy(&response.body)
    );
    serde_json::from_value(response.json()["registration"].clone()).expect("registration dto")
}

/// 已登记设备的普通请求鉴权头。
async fn request_headers(
    node: &Node,
    seed: u8,
    method: &str,
    path: &str,
    query: &str,
    body: &[u8],
) -> Vec<(String, String)> {
    let challenge = issue_challenge(node, "request_auth").await;
    auth_headers(
        node,
        &challenge,
        &device(seed),
        seed,
        ChallengePurpose::RequestAuth,
        method,
        path,
        query,
        body,
    )
}

fn group_creation(group: &str, owner_seed: u8, member_seeds: &[u8]) -> SignedGroupCreation {
    let mut members: Vec<GroupMemberSpec> = Vec::new();
    members.push(GroupMemberSpec {
        device_id: device_text(owner_seed),
        role: "owner".into(),
    });
    for seed in member_seeds {
        members.push(GroupMemberSpec {
            device_id: device_text(*seed),
            role: "member".into(),
        });
    }
    let creation = GroupCreation {
        group_id: group.to_owned(),
        owner_account_id: "acct-http".into(),
        initial_control_head: hex_encode(&INITIAL_CONTROL_HEAD),
        policy_version: 1,
        mls_epoch: 0,
        members,
    };
    let signature: Signature = device(owner_seed).sign(&creation.signing_bytes().expect("bytes"));
    SignedGroupCreation {
        creation,
        device_signature: hex_encode(&signature.to_bytes()),
    }
}

fn candidate(
    group: &str,
    author_seed: u8,
    submission_seed: u8,
    payload: &[u8],
    kind: &str,
    expected_control_head: [u8; 32],
) -> (GroupCandidate, String) {
    let candidate = GroupCandidate {
        protocol_major: 1,
        protocol_minor: 0,
        submission_id: submission_text(submission_seed),
        group_id: group.to_owned(),
        author_device_id: device_text(author_seed),
        kind: kind.to_owned(),
        expected_control_head: hex_encode(&expected_control_head),
        policy_version: 1,
        mls_epoch: 0,
        encrypted_payload: hex_encode(payload),
        control_metadata: if kind == KIND_CONTROL {
            Some(CandidateControlMetadata {
                operation: "invite".into(),
                target_digest: hex_encode(&[0x33_u8; 32]),
            })
        } else {
            None
        },
    };
    let unsigned = candidate
        .to_unsigned(chat_protocol::MAX_CIPHERTEXT_BYTES)
        .expect("candidate converts");
    let bytes = candidate_signing_bytes(&unsigned).expect("candidate bytes");
    let signature: Signature = device(author_seed).sign(bytes.as_bytes());
    let signature_hex = hex_encode(&signature.to_bytes());
    (candidate, signature_hex)
}

fn group_body(creation: &SignedGroupCreation) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "creation": creation.creation,
        "device_signature": creation.device_signature,
    }))
    .expect("group body")
}

fn entry_body(candidate: &GroupCandidate, signature: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "candidate": candidate,
        "author_signature": signature,
    }))
    .expect("entry body")
}

async fn register_group_request(node: &Node, seed: u8, creation: &SignedGroupCreation) -> HttpResponse {
    let body = group_body(creation);
    let headers = request_headers(node, seed, "POST", "/v1/groups", "", &body).await;
    http(&node.address, "POST", "/v1/groups", &headers, &body).await
}

async fn append_request(
    node: &Node,
    seed: u8,
    group: &str,
    candidate: &GroupCandidate,
    signature: &str,
) -> HttpResponse {
    let path = format!("/v1/groups/{group}/entries");
    let body = entry_body(candidate, signature);
    let headers = request_headers(node, seed, "POST", &path, "", &body).await;
    http(&node.address, "POST", &path, &headers, &body).await
}

async fn get_request(
    node: &Node,
    seed: u8,
    path: &str,
    query: &str,
) -> HttpResponse {
    let headers = request_headers(node, seed, "GET", path, query, b"").await;
    let target = if query.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{query}")
    };
    http(&node.address, "GET", &target, &headers, b"").await
}
/// 建节点 + 两台设备 + 一个两成员群。
async fn setup(policy: NodeAuthPolicy) -> (Node, String) {
    let node = start_node(policy).await;
    register_device(&node, 21).await;
    register_device(&node, 22).await;
    let group = group_text(31);
    let creation = group_creation(&group, 21, &[22]);
    let response = register_group_request(&node, 21, &creation).await;
    assert_eq!(
        response.status,
        200,
        "group registers over http: {:?}",
        String::from_utf8_lossy(&response.body)
    );
    (node, group)
}

#[tokio::test]
async fn group_registration_and_append_are_idempotent_over_http() {
    let (node, group) = setup(NodeAuthPolicy::default()).await;
    let creation = group_creation(&group, 21, &[22]);
    let response = register_group_request(&node, 21, &creation).await;
    assert_eq!(response.status, 200);
    let registration: GroupRegistration =
        serde_json::from_value(response.json()["registration"].clone()).expect("registration dto");
    registration
        .verify(&node.node_public_key)
        .expect("node-signed registration");
    assert_eq!(registration.group_id, group);
    assert_eq!(registration.owner_account_id, "acct-http");

    // 同内容重试（新挑战）：同一份回执。
    let repeat = register_group_request(&node, 21, &creation).await;
    assert_eq!(repeat.status, 200);
    let repeat: GroupRegistration =
        serde_json::from_value(repeat.json()["registration"].clone()).expect("registration dto");
    assert_eq!(repeat, registration);

    // 追加一条应用候选。
    let (first_candidate, first_signature) = candidate(
        &group,
        21,
        41,
        b"first cipher",
        KIND_APPLICATION,
        INITIAL_CONTROL_HEAD,
    );
    let response = append_request(&node, 21, &group, &first_candidate, &first_signature).await;
    assert_eq!(
        response.status,
        200,
        "append accepted: {:?}",
        String::from_utf8_lossy(&response.body)
    );
    let receipt: AppendResult =
        serde_json::from_value(response.json()["result"].clone()).expect("append dto");
    assert_eq!(receipt.seq, 1);
    receipt
        .verify(&node.node_public_key)
        .expect("node-signed receipt");

    // 提交后响应丢失：同密文重试得到同一 seq/回执。
    let retry = append_request(&node, 21, &group, &first_candidate, &first_signature).await;
    assert_eq!(retry.status, 200);
    let retried: AppendResult =
        serde_json::from_value(retry.json()["result"].clone()).expect("append dto");
    assert_eq!(retried, receipt);

    // 同 submission 换密文：409 SUBMISSION_ID_CONFLICT。
    let (divergent, divergent_signature) = candidate(
        &group,
        21,
        41,
        b"different cipher",
        KIND_APPLICATION,
        INITIAL_CONTROL_HEAD,
    );
    let response = append_request(&node, 21, &group, &divergent, &divergent_signature).await;
    assert_eq!(response.status, 409);
    assert_eq!(response.error_code(), "SUBMISSION_ID_CONFLICT");

    // 旧的 Expected 409 stale head 与 403 权限可分：控制候选推进链头后旧前置状态失效。
    let (control_one, control_one_signature) = candidate(
        &group,
        21,
        42,
        b"commit one",
        KIND_CONTROL,
        INITIAL_CONTROL_HEAD,
    );
    let accepted = append_request(&node, 21, &group, &control_one, &control_one_signature).await;
    assert_eq!(accepted.status, 200);
    let control_receipt: AppendResult =
        serde_json::from_value(accepted.json()["result"].clone()).expect("control dto");
    let (control_two, control_two_signature) = candidate(
        &group,
        22,
        43,
        b"commit two",
        KIND_CONTROL,
        INITIAL_CONTROL_HEAD,
    );
    let response = append_request(&node, 22, &group, &control_two, &control_two_signature).await;
    assert_eq!(response.status, 409);
    assert_eq!(response.error_code(), "STALE_CONTROL_HEAD");

    // 权限失败与 stale 明确区分：非成员作者 403。
    let (impersonated, impersonated_signature) = candidate(
        &group,
        22,
        44,
        b"as bob",
        KIND_APPLICATION,
        INITIAL_CONTROL_HEAD,
    );
    let response = append_request(&node, 21, &group, &impersonated, &impersonated_signature).await;
    assert_eq!(response.status, 403);
    assert_eq!(response.error_code(), "PERMISSION_DENIED");
    let _ = control_receipt;
}

#[tokio::test]
async fn append_requires_bound_authentication_and_matching_group() {
    let (node, group) = setup(NodeAuthPolicy::default()).await;
    let (candidate_body, signature) = candidate(
        &group,
        21,
        51,
        b"bound cipher",
        KIND_APPLICATION,
        INITIAL_CONTROL_HEAD,
    );
    let path = format!("/v1/groups/{group}/entries");
    let body = entry_body(&candidate_body, &signature);

    // 缺失鉴权头。
    let response = http(&node.address, "POST", &path, &[], &body).await;
    assert_eq!(response.status, 400);
    assert_eq!(response.error_code(), "INVALID_REQUEST");

    // 签名覆盖 GET 却以 POST 提交。
    let headers = request_headers(&node, 21, "GET", &path, "", &body).await;
    let response = http(&node.address, "POST", &path, &headers, &body).await;
    assert_eq!(response.status, 401);
    assert_eq!(response.error_code(), "INVALID_SIGNATURE");

    // 签名覆盖另一个路径。
    let other_path = format!("/v1/groups/{}/entries", group_text(77));
    let headers = request_headers(&node, 21, "POST", &other_path, "", &body).await;
    let response = http(&node.address, "POST", &path, &headers, &body).await;
    assert_eq!(response.status, 401);
    assert_eq!(response.error_code(), "INVALID_SIGNATURE");

    // 签名覆盖另一份正文。
    let (other_candidate, other_signature) = candidate(
        &group,
        21,
        52,
        b"other cipher",
        KIND_APPLICATION,
        INITIAL_CONTROL_HEAD,
    );
    let other_body = entry_body(&other_candidate, &other_signature);
    let headers = request_headers(&node, 21, "POST", &path, "", &other_body).await;
    let response = http(&node.address, "POST", &path, &headers, &body).await;
    assert_eq!(response.status, 401);
    assert_eq!(response.error_code(), "INVALID_SIGNATURE");

    // 路径 group 与正文 group 不一致：403。
    let (foreign_candidate, foreign_signature) = candidate(
        &group_text(78),
        21,
        53,
        b"foreign cipher",
        KIND_APPLICATION,
        INITIAL_CONTROL_HEAD,
    );
    let foreign_body = entry_body(&foreign_candidate, &foreign_signature);
    let headers = request_headers(&node, 21, "POST", &path, "", &foreign_body).await;
    let response = http(&node.address, "POST", &path, &headers, &foreign_body).await;
    assert_eq!(response.status, 403);
    assert_eq!(response.error_code(), "PERMISSION_DENIED");

    // 同一挑战重放：AUTH_REPLAY。
    let headers = request_headers(&node, 21, "POST", &path, "", &body).await;
    let first = http(&node.address, "POST", &path, &headers, &body).await;
    assert_eq!(first.status, 200);
    let second = http(&node.address, "POST", &path, &headers, &body).await;
    assert_eq!(second.status, 401);
    assert_eq!(second.error_code(), "AUTH_REPLAY");
}

#[tokio::test]
async fn log_and_head_endpoints_paginate_and_reject_bad_input() {
    let (node, group) = setup(NodeAuthPolicy::default()).await;
    for (index, payload) in [b"one".as_slice(), b"two".as_slice(), b"three".as_slice()]
        .into_iter()
        .enumerate()
    {
        let (body_candidate, signature) = candidate(
            &group,
            21,
            60 + index as u8,
            payload,
            KIND_APPLICATION,
            INITIAL_CONTROL_HEAD,
        );
        let response = append_request(&node, 21, &group, &body_candidate, &signature).await;
        assert_eq!(response.status, 200);
    }
    let log_path = format!("/v1/groups/{group}/log");
    let response = get_request(&node, 22, &log_path, "after=0&limit=2").await;
    assert_eq!(
        response.status,
        200,
        "log page: {:?}",
        String::from_utf8_lossy(&response.body)
    );
    let page = response.json()["page"].clone();
    assert_eq!(page["entries"].as_array().expect("entries").len(), 2);
    assert_eq!(page["next_after"], 2);
    assert_eq!(page["has_more"], true);
    assert_eq!(page["head"]["seq"], 3);
    let head: SignedLogHead = serde_json::from_value(page["head"].clone()).expect("head dto");
    head.verify(&node.node_public_key).expect("signed head");

    // head 端点给出同一份签名头。
    let head_path = format!("/v1/groups/{group}/head");
    let response = get_request(&node, 22, &head_path, "").await;
    assert_eq!(response.status, 200);
    let head_dto: SignedLogHead =
        serde_json::from_value(response.json()["head"].clone()).expect("head dto");
    assert_eq!(head_dto, head);

    // 第二页。
    let response = get_request(&node, 22, &log_path, "after=2&limit=2").await;
    assert_eq!(response.status, 200);
    let page = response.json()["page"].clone();
    assert_eq!(page["entries"].as_array().expect("entries").len(), 1);
    assert_eq!(page["has_more"], false);

    // limit 上限与未知参数显式拒绝。
    for query in ["after=0&limit=0", "after=0&limit=1000", "after=0&unknown=1", "after=x"] {
        let response = get_request(&node, 22, &log_path, query).await;
        assert_eq!(response.status, 400, "query {query} must be rejected");
        assert_eq!(response.error_code(), "INVALID_REQUEST", "query {query}");
    }

    // 查询串参与签名：签名覆盖 limit=2 却请求 limit=3 → 401。
    let headers = request_headers(&node, 22, "GET", &log_path, "after=0&limit=2", b"").await;
    let response = http(
        &node.address,
        "GET",
        &format!("{log_path}?after=0&limit=3"),
        &headers,
        b"",
    )
    .await;
    assert_eq!(response.status, 401);
    assert_eq!(response.error_code(), "INVALID_SIGNATURE");
}

#[tokio::test]
async fn mailbox_store_read_and_ack_over_http() {
    let (node, group) = setup(NodeAuthPolicy::default()).await;
    let (body_candidate, signature) = candidate(
        &group,
        21,
        70,
        b"authorized",
        KIND_APPLICATION,
        INITIAL_CONTROL_HEAD,
    );
    let response = append_request(&node, 21, &group, &body_candidate, &signature).await;
    assert_eq!(response.status, 200);

    // 投递三条密文到 device 22。
    for index in 0..3_u8 {
        let item = serde_json::json!({
            "schema_version": 1,
            "item": {
                "item_id": format!("http-item-{index}"),
                "group_id": group,
                "target_device_id": device_text(22),
                "source_seq": 1,
                "expires_at": START + 3_600,
                "ciphertext_envelope": hex_encode(b"cipher text"),
            },
        });
        let body = serde_json::to_vec(&item).expect("item json");
        let headers = request_headers(&node, 21, "POST", "/v1/mailbox/items", "", &body).await;
        let response = http(&node.address, "POST", "/v1/mailbox/items", &headers, &body).await;
        assert_eq!(
            response.status,
            200,
            "item stored: {:?}",
            String::from_utf8_lossy(&response.body)
        );
        if index == 0 {
            let receipt: MailboxReceipt =
                serde_json::from_value(response.json()["receipt"].clone()).expect("receipt dto");
            receipt.verify(&node.node_public_key).expect("signed receipt");
        }
    }

    let response = get_request(&node, 22, "/v1/mailbox", "limit=2").await;
    assert_eq!(response.status, 200);
    let page: MailboxPage =
        serde_json::from_value(response.json()["page"].clone()).expect("page dto");
    assert_eq!(page.items.len(), 2);
    let cursor = page.next_cursor.clone().expect("next cursor");
    assert_eq!(page.expired_items, 0);

    let response = get_request(
        &node,
        22,
        "/v1/mailbox",
        &format!("limit=2&cursor={cursor}"),
    )
    .await;
    assert_eq!(response.status, 200);
    let page2: MailboxPage =
        serde_json::from_value(response.json()["page"].clone()).expect("page dto");
    assert_eq!(page2.items.len(), 1);
    assert!(page2.next_cursor.is_none());

    // 跨设备使用游标 → 400 INVALID_CURSOR。
    let response = get_request(
        &node,
        21,
        "/v1/mailbox",
        &format!("limit=2&cursor={cursor}"),
    )
    .await;
    assert_eq!(response.status, 400);
    assert_eq!(response.error_code(), "INVALID_CURSOR");
    // 伪造游标同样拒绝。
    let response = get_request(&node, 22, "/v1/mailbox", "cursor=garbage").await;
    assert_eq!(response.status, 400);
    assert_eq!(response.error_code(), "INVALID_CURSOR");
    // 未知查询参数。
    let response = get_request(&node, 22, "/v1/mailbox", "device=other").await;
    assert_eq!(response.status, 400);
    assert_eq!(response.error_code(), "INVALID_REQUEST");

    // ACK 自己的两条。
    let ack = serde_json::json!({
        "schema_version": 1,
        "item_ids": ["http-item-0", "http-item-1"],
    });
    let body = serde_json::to_vec(&ack).expect("ack json");
    let headers = request_headers(&node, 22, "POST", "/v1/mailbox/ack", "", &body).await;
    let response = http(&node.address, "POST", "/v1/mailbox/ack", &headers, &body).await;
    assert_eq!(response.status, 200);
    let summary: AckSummary =
        serde_json::from_value(response.json()["summary"].clone()).expect("ack dto");
    assert_eq!(summary.acknowledged.len(), 2);
    assert_eq!(summary.released_bytes, 22);

    // 重复 ACK：幂等。
    let body = serde_json::to_vec(&ack).expect("ack json");
    let headers = request_headers(&node, 22, "POST", "/v1/mailbox/ack", "", &body).await;
    let response = http(&node.address, "POST", "/v1/mailbox/ack", &headers, &body).await;
    assert_eq!(response.status, 200);
    let summary: AckSummary =
        serde_json::from_value(response.json()["summary"].clone()).expect("ack dto");
    assert!(summary.acknowledged.is_empty());
    assert_eq!(summary.already_consumed.len(), 2);

    // 未消费的那条仍可读。
    let response = get_request(&node, 22, "/v1/mailbox", "limit=10").await;
    let page: MailboxPage =
        serde_json::from_value(response.json()["page"].clone()).expect("page dto");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].item_id, "http-item-2");

    // 他设备条目：403，且未知条目同为 403（不泄漏存在性）。
    let ack = serde_json::json!({ "schema_version": 1, "item_ids": ["http-item-2"] });
    let body = serde_json::to_vec(&ack).expect("ack json");
    let headers = request_headers(&node, 21, "POST", "/v1/mailbox/ack", "", &body).await;
    let response = http(&node.address, "POST", "/v1/mailbox/ack", &headers, &body).await;
    assert_eq!(response.status, 403);
    assert_eq!(response.error_code(), "PERMISSION_DENIED");
    let ack = serde_json::json!({ "schema_version": 1, "item_ids": ["does-not-exist"] });
    let body = serde_json::to_vec(&ack).expect("ack json");
    let headers = request_headers(&node, 22, "POST", "/v1/mailbox/ack", "", &body).await;
    let response = http(&node.address, "POST", "/v1/mailbox/ack", &headers, &body).await;
    assert_eq!(response.status, 403);
    assert_eq!(response.error_code(), "PERMISSION_DENIED");
}

#[tokio::test]
async fn non_member_is_forbidden_on_group_and_mailbox_endpoints() {
    let policy = NodeAuthPolicy {
        max_devices_per_account: 3,
        ..NodeAuthPolicy::default()
    };
    let (node, group) = setup(policy).await;
    let outsider = register_device(&node, 23).await;
    assert_eq!(outsider.device_id, device_text(23));

    let log_path = format!("/v1/groups/{group}/log");
    let response = get_request(&node, 23, &log_path, "after=0&limit=5").await;
    assert_eq!(response.status, 403);
    assert_eq!(response.error_code(), "HISTORY_NOT_AUTHORIZED");
    let head_path = format!("/v1/groups/{group}/head");
    let response = get_request(&node, 23, &head_path, "").await;
    assert_eq!(response.status, 403);
    assert_eq!(response.error_code(), "PERMISSION_DENIED");

    // 非成员追加日志：403。
    let (body_candidate, signature) = candidate(
        &group,
        23,
        80,
        b"not a member",
        KIND_APPLICATION,
        INITIAL_CONTROL_HEAD,
    );
    let response = append_request(&node, 23, &group, &body_candidate, &signature).await;
    assert_eq!(response.status, 403);
    assert_eq!(response.error_code(), "PERMISSION_DENIED");

    // 未知群：404。
    let unknown_group = group_text(99);
    let (unknown_candidate, unknown_signature) = candidate(
        &unknown_group,
        21,
        81,
        b"unknown group",
        KIND_APPLICATION,
        INITIAL_CONTROL_HEAD,
    );
    let response = append_request(&node, 21, &unknown_group, &unknown_candidate, &unknown_signature).await;
    assert_eq!(response.status, 404);
    assert_eq!(response.error_code(), "GROUP_NOT_FOUND");
}
// ---------------------------------------------------------------- 附件 blob 端点（NODE-012~015）

fn object_text(seed: u8) -> String {
    chat_protocol::ObjectId::from_bytes([seed; 32]).encode()
}

fn operation_text(seed: u8) -> String {
    chat_protocol::OperationId::from_bytes([seed; 32]).encode()
}

/// 整体密文摘要 = 按块序拼接后的一次 SHA-256（与节点侧校验一致）。
fn ciphertext_digest(chunks: &[&[u8]]) -> String {
    let mut combined = Vec::new();
    for chunk in chunks {
        combined.extend_from_slice(chunk);
    }
    hex_encode(&chat_node_services::canonical::sha256(&combined))
}

#[allow(clippy::too_many_arguments)]
async fn begin_blob(
    node: &Node,
    seed: u8,
    group: &str,
    operation: &str,
    blob: &str,
    source_seq: u64,
    total: u64,
    chunk_bytes: u32,
    chunk_count: u32,
) -> HttpResponse {
    let body = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "operation_id": operation,
        "descriptor": {
            "blob_id": blob,
            "group_id": group,
            "source_seq": source_seq,
            "ciphertext_bytes": total,
            "chunk_bytes": chunk_bytes,
            "chunk_count": chunk_count,
        },
    }))
    .expect("blob begin json");
    let headers = request_headers(node, seed, "POST", "/v1/blobs", "", &body).await;
    http(&node.address, "POST", "/v1/blobs", &headers, &body).await
}

async fn put_chunk(
    node: &Node,
    seed: u8,
    upload_id: &str,
    index: u32,
    bytes: &[u8],
) -> HttpResponse {
    let path = format!("/v1/blobs/{upload_id}/chunks/{index}");
    let headers = request_headers(node, seed, "PUT", &path, "", bytes).await;
    http_with(
        &node.address,
        "PUT",
        &path,
        &headers,
        bytes,
        "application/octet-stream",
    )
    .await
}

async fn complete_blob(
    node: &Node,
    seed: u8,
    upload_id: &str,
    chunk_count: u32,
    total: u64,
    digest: &str,
) -> HttpResponse {
    let path = format!("/v1/blobs/{upload_id}/complete");
    let body = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "manifest": {
            "chunk_count": chunk_count,
            "ciphertext_bytes": total,
            "ciphertext_digest": digest,
        },
    }))
    .expect("manifest json");
    let headers = request_headers(node, seed, "POST", &path, "", &body).await;
    http(&node.address, "POST", &path, &headers, &body).await
}

async fn read_chunk(node: &Node, seed: u8, blob_id: &str, index: u32) -> HttpResponse {
    let path = format!("/v1/blobs/{blob_id}/chunks/{index}");
    let headers = request_headers(node, seed, "GET", &path, "", b"").await;
    http(&node.address, "GET", &path, &headers, b"").await
}

/// 上传一条已接受日志条目，返回其 seq。
async fn append_attachment_entry(node: &Node, group: &str, seed: u8) -> u64 {
    let (entry, signature) = candidate(
        group,
        seed,
        41,
        b"attachment message",
        chat_node_services::group::KIND_APPLICATION,
        INITIAL_CONTROL_HEAD,
    );
    let response = append_request(node, seed, group, &entry, &signature).await;
    assert_eq!(response.status, 200);
    response.json()["result"]["seq"].as_u64().expect("seq")
}

#[tokio::test]
async fn blob_upload_and_read_round_trip_over_http() {
    let (node, group) = setup(NodeAuthPolicy::default()).await;
    let source_seq = append_attachment_entry(&node, &group, 21).await;
    let blob = object_text(131);
    let first: &[u8] = b"head";
    let second: &[u8] = b"tail";
    let begin = begin_blob(&node, 21, &group, &operation_text(231), &blob, source_seq, 8, 4, 2).await;
    assert_eq!(
        begin.status,
        200,
        "begin: {:?}",
        String::from_utf8_lossy(&begin.body)
    );
    let session = begin.json()["session"].clone();
    let upload_id = session["upload_id"].as_str().expect("upload id").to_owned();
    assert_eq!(session["blob_id"], blob);
    assert_eq!(session["chunk_count"], 2);
    // 原始字节正文（不是 hex/JSON）。
    let uploaded = put_chunk(&node, 21, &upload_id, 0, first).await;
    assert_eq!(uploaded.status, 200);
    let receipt = uploaded.json()["receipt"].clone();
    assert_eq!(receipt["chunk_index"], 0);
    assert_eq!(receipt["received_chunks"], 1);
    // 同块同字节重传：同一份回执。
    let repeat = put_chunk(&node, 21, &upload_id, 0, first).await;
    assert_eq!(repeat.status, 200);
    assert_eq!(repeat.json()["receipt"], receipt);
    assert_eq!(put_chunk(&node, 21, &upload_id, 1, second).await.status, 200);
    let digest = ciphertext_digest(&[first, second]);
    let completed = complete_blob(&node, 21, &upload_id, 2, 8, &digest).await;
    assert_eq!(
        completed.status,
        200,
        "complete: {:?}",
        String::from_utf8_lossy(&completed.body)
    );
    let published = completed.json()["receipt"].clone();
    assert_eq!(published["blob_id"], blob);
    assert_eq!(published["ciphertext_digest"], digest);

    // 成员按块取回**原始**密文（响应不是 JSON）。
    let chunk = read_chunk(&node, 22, &blob, 0).await;
    assert_eq!(chunk.status, 200);
    assert_eq!(chunk.body, first);
    assert_eq!(
        chunk.content_type().as_deref(),
        Some("application/octet-stream")
    );
    let chunk = read_chunk(&node, 22, &blob, 1).await;
    assert_eq!(chunk.body, second);
    // 上传者本人也能读。
    assert_eq!(read_chunk(&node, 21, &blob, 0).await.body, first);
}
fn small_blob_policy() -> NodeAuthPolicy {
    NodeAuthPolicy {
        max_devices_per_account: 3,
        blob: chat_node_services::limits::BlobPolicy {
            max_bytes_per_device: 64,
            max_object_bytes: 32,
            max_chunk_bytes: 8,
            max_chunk_count: 4,
            upload_ttl_seconds: 60,
        },
        ..NodeAuthPolicy::default()
    }
}

#[tokio::test]
async fn blob_http_error_statuses_are_stable() {
    let (node, group) = setup(small_blob_policy()).await;
    let source_seq = append_attachment_entry(&node, &group, 21).await;
    let blob = object_text(132);
    let begin = begin_blob(
        &node,
        21,
        &group,
        &operation_text(232),
        &blob,
        source_seq,
        16,
        8,
        2,
    )
    .await;
    assert_eq!(
        begin.status,
        200,
        "begin: {:?}",
        String::from_utf8_lossy(&begin.body)
    );
    let upload_id = begin.json()["session"]["upload_id"]
        .as_str()
        .expect("upload id")
        .to_owned();

    // 乱序写入 → 409。
    let out_of_order = put_chunk(&node, 21, &upload_id, 1, b"abcdefgh").await;
    assert_eq!(out_of_order.status, 409);
    assert_eq!(out_of_order.error_code(), "CHUNK_OUT_OF_ORDER");
    // 超过声明块大小 → 413。
    let oversized = put_chunk(&node, 21, &upload_id, 0, b"123456789").await;
    assert_eq!(oversized.status, 413);
    assert_eq!(oversized.error_code(), "CONTENT_TOO_LARGE");
    // 短于声明边界 → 409。
    let short = put_chunk(&node, 21, &upload_id, 0, b"123").await;
    assert_eq!(short.status, 409);
    assert_eq!(short.error_code(), "MANIFEST_MISMATCH");
    // 非数字块序号 → 400（不是 axum 默认错误体）。
    let bad_path = format!("/v1/blobs/{upload_id}/chunks/abc");
    let headers = request_headers(&node, 21, "PUT", &bad_path, "", b"12345678").await;
    let bad_index = http_with(
        &node.address,
        "PUT",
        &bad_path,
        &headers,
        b"12345678",
        "application/octet-stream",
    )
    .await;
    assert_eq!(bad_index.status, 400);
    assert_eq!(bad_index.error_code(), "INVALID_REQUEST");
    // 未知上传会话 → 404。
    let unknown = put_chunk(&node, 21, "upl_missing", 0, b"12345678").await;
    assert_eq!(unknown.status, 404);
    assert_eq!(unknown.error_code(), "UPLOAD_NOT_FOUND");

    // 正常写第一块，但缺末块就完成 → 409 MISSING_CHUNKS。
    assert_eq!(put_chunk(&node, 21, &upload_id, 0, b"12345678").await.status, 200);
    let correct_digest = ciphertext_digest(&[b"12345678", b"abcdefgh"]);
    let missing = complete_blob(&node, 21, &upload_id, 2, 16, &correct_digest).await;
    assert_eq!(missing.status, 409);
    assert_eq!(missing.error_code(), "MISSING_CHUNKS");
    // 写满后摘要不符 → 409 MANIFEST_MISMATCH。
    assert_eq!(put_chunk(&node, 21, &upload_id, 1, b"abcdefgh").await.status, 200);
    let wrong_digest = complete_blob(&node, 21, &upload_id, 2, 16, &"00".repeat(32)).await;
    assert_eq!(wrong_digest.status, 409);
    assert_eq!(wrong_digest.error_code(), "MANIFEST_MISMATCH");
    // 未发布不可读 → 404。
    let unpublished = read_chunk(&node, 21, &blob, 0).await;
    assert_eq!(unpublished.status, 404);
    assert_eq!(unpublished.error_code(), "BLOB_NOT_FOUND");
    // 正确清单发布成功。
    let published = complete_blob(&node, 21, &upload_id, 2, 16, &correct_digest).await;
    assert_eq!(
        published.status,
        200,
        "complete: {:?}",
        String::from_utf8_lossy(&published.body)
    );
    // 越界块序号 → 400。
    let range = read_chunk(&node, 21, &blob, 5).await;
    assert_eq!(range.status, 400);
    assert_eq!(range.error_code(), "INVALID_REQUEST");
    // 声明超过对象上限 → 413。
    let too_big = begin_blob(
        &node,
        21,
        &group,
        &operation_text(233),
        &object_text(133),
        source_seq,
        33,
        8,
        5,
    )
    .await;
    assert_eq!(too_big.status, 413);
    assert_eq!(too_big.error_code(), "CONTENT_TOO_LARGE");
    // 会话过期后再写块 → 410 UPLOAD_EXPIRED。
    let expiring = begin_blob(
        &node,
        21,
        &group,
        &operation_text(234),
        &object_text(134),
        source_seq,
        8,
        8,
        1,
    )
    .await;
    assert_eq!(expiring.status, 200);
    let expiring_id = expiring.json()["session"]["upload_id"]
        .as_str()
        .expect("upload id")
        .to_owned();
    node.clock.advance(61);
    let expired = put_chunk(&node, 21, &expiring_id, 0, b"12345678").await;
    assert_eq!(expired.status, 410);
    assert_eq!(expired.error_code(), "UPLOAD_EXPIRED");
}

#[tokio::test]
async fn blob_endpoints_require_bound_authentication() {
    let (node, group) = setup(NodeAuthPolicy {
        max_devices_per_account: 3,
        ..NodeAuthPolicy::default()
    })
    .await;
    let source_seq = append_attachment_entry(&node, &group, 21).await;
    let blob = object_text(135);
    let begin = begin_blob(
        &node,
        21,
        &group,
        &operation_text(235),
        &blob,
        source_seq,
        8,
        8,
        1,
    )
    .await;
    assert_eq!(begin.status, 200);
    let upload_id = begin.json()["session"]["upload_id"]
        .as_str()
        .expect("upload id")
        .to_owned();
    let path = format!("/v1/blobs/{upload_id}/chunks/0");

    // 缺鉴权头 → 400。
    let no_headers = http_with(
        &node.address,
        "PUT",
        &path,
        &[],
        b"12345678",
        "application/octet-stream",
    )
    .await;
    assert_eq!(no_headers.status, 400);
    assert_eq!(no_headers.error_code(), "INVALID_REQUEST");
    // 签名覆盖 GET 却以 PUT 提交 → 401。
    let headers = request_headers(&node, 21, "GET", &path, "", b"12345678").await;
    let wrong_method = http_with(
        &node.address,
        "PUT",
        &path,
        &headers,
        b"12345678",
        "application/octet-stream",
    )
    .await;
    assert_eq!(wrong_method.status, 401);
    assert_eq!(wrong_method.error_code(), "INVALID_SIGNATURE");
    // 签名覆盖另一条路径 → 401。
    let other_path = format!("/v1/blobs/{}/chunks/0", object_text(136));
    let headers = request_headers(&node, 21, "PUT", &other_path, "", b"12345678").await;
    let wrong_path = http_with(
        &node.address,
        "PUT",
        &path,
        &headers,
        b"12345678",
        "application/octet-stream",
    )
    .await;
    assert_eq!(wrong_path.status, 401);
    // 签名覆盖另一份正文 → 401（正文摘要参与签名）。
    let headers = request_headers(&node, 21, "PUT", &path, "", b"87654321").await;
    let wrong_body = http_with(
        &node.address,
        "PUT",
        &path,
        &headers,
        b"12345678",
        "application/octet-stream",
    )
    .await;
    assert_eq!(wrong_body.status, 401);

    // 正确写入并发布。
    assert_eq!(put_chunk(&node, 21, &upload_id, 0, b"12345678").await.status, 200);
    let published = complete_blob(
        &node,
        21,
        &upload_id,
        1,
        8,
        &ciphertext_digest(&[b"12345678"]),
    )
    .await;
    assert_eq!(published.status, 200);
    // 已登记但不在群内的设备读取 → 403。
    register_device(&node, 23).await;
    let denied = read_chunk(&node, 23, &blob, 0).await;
    assert_eq!(denied.status, 403);
    assert_eq!(denied.error_code(), "HISTORY_NOT_AUTHORIZED");
    // 方法/路由边界：complete 端点只允许 POST，未知路由 404。
    let method = get_request(&node, 21, &format!("/v1/blobs/{upload_id}/complete"), "").await;
    assert_eq!(method.status, 405);
    let unknown = get_request(&node, 21, "/v1/nothing", "").await;
    assert_eq!(unknown.status, 404);
}
/// 分块正文上限对齐：客户端 256 KiB 明文块（密文 262_173 字节）能通过路由层与节点策略；
/// 同时 JSON DTO 路由仍保持 64 KiB 的正文上限，放宽只针对块路线。
#[tokio::test]
async fn chunk_body_limit_accepts_client_scale_chunks_but_keeps_dto_routes_bounded() {
    use chat_node_services::limits::{CLIENT_ATTACHMENT_CIPHERTEXT_CHUNK_BYTES, MAX_HTTP_BODY_BYTES};

    let (node, group) = setup(NodeAuthPolicy::default()).await;
    let source_seq = append_attachment_entry(&node, &group, 21).await;
    let blob = object_text(141);
    let chunk_len = CLIENT_ATTACHMENT_CIPHERTEXT_CHUNK_BYTES;
    assert_eq!(chunk_len, 262_173);
    let chunk = vec![0x5a_u8; chunk_len];

    // 单块附件：声明块大小 = 总长 = 客户端满密文块。
    let begin = begin_blob(
        &node,
        21,
        &group,
        &operation_text(241),
        &blob,
        source_seq,
        chunk_len as u64,
        chunk_len as u32,
        1,
    )
    .await;
    assert_eq!(
        begin.status,
        200,
        "begin: {:?}",
        String::from_utf8_lossy(&begin.body)
    );
    let upload_id = begin.json()["session"]["upload_id"]
        .as_str()
        .expect("upload id")
        .to_owned();

    // 262_173 字节的原始正文通过（旧上限 64 KiB 会在这里 413）。
    let uploaded = put_chunk(&node, 21, &upload_id, 0, &chunk).await;
    assert_eq!(
        uploaded.status,
        200,
        "chunk: {:?}",
        String::from_utf8_lossy(&uploaded.body)
    );
    assert_eq!(uploaded.json()["receipt"]["byte_size"], chunk_len as u64);
    let completed = complete_blob(
        &node,
        21,
        &upload_id,
        1,
        chunk_len as u64,
        &ciphertext_digest(&[&chunk]),
    )
    .await;
    assert_eq!(completed.status, 200);

    // JSON DTO 路由仍然受全局小上限保护：64 KiB + 1 的正文直接 413。
    let oversized_json = vec![b'x'; MAX_HTTP_BODY_BYTES + 1];
    let rejected = http(
        &node.address,
        "POST",
        "/v1/auth/challenge",
        &[],
        &oversized_json,
    )
    .await;
    assert_eq!(rejected.status, 413);
}
// ---------------------------------------------------------------- API-004 能力令牌

/// 申请一个能力令牌（设备鉴权 + 对象级授权都在服务端做）。
async fn issue_capability(
    node: &Node,
    seed: u8,
    object_id: &str,
    scope: &str,
    ttl_seconds: Option<u64>,
) -> HttpResponse {
    let body = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "object_id": object_id,
        "scope": scope,
        "ttl_seconds": ttl_seconds,
    }))
    .expect("capability json");
    let headers = request_headers(node, seed, "POST", "/v1/capabilities", "", &body).await;
    http(&node.address, "POST", "/v1/capabilities", &headers, &body).await
}

/// 带能力令牌的块上传（设备鉴权头 + 令牌头都要）。
async fn put_chunk_with_capability(
    node: &Node,
    seed: u8,
    upload_id: &str,
    index: u32,
    bytes: &[u8],
    token: &str,
) -> HttpResponse {
    let path = format!("/v1/blobs/{upload_id}/chunks/{index}");
    let mut headers = request_headers(node, seed, "PUT", &path, "", bytes).await;
    headers.push(("x-chat-capability".to_owned(), token.to_owned()));
    http_with(
        &node.address,
        "PUT",
        &path,
        &headers,
        bytes,
        "application/octet-stream",
    )
    .await
}

/// 带能力令牌的块读取。
async fn read_chunk_with_capability(
    node: &Node,
    seed: u8,
    blob_id: &str,
    index: u32,
    token: &str,
) -> HttpResponse {
    let path = format!("/v1/blobs/{blob_id}/chunks/{index}");
    let mut headers = request_headers(node, seed, "GET", &path, "", b"").await;
    headers.push(("x-chat-capability".to_owned(), token.to_owned()));
    http(&node.address, "GET", &path, &headers, b"").await
}

#[tokio::test]
async fn capability_tokens_authorize_blob_chunks_over_http() {
    let (node, group) = setup(NodeAuthPolicy::default()).await;
    let source_seq = append_attachment_entry(&node, &group, 21).await;
    let first: &[u8] = b"head";
    let second: &[u8] = b"tail";
    let blob = object_text(141);
    let begin = begin_blob(&node, 21, &group, &operation_text(241), &blob, source_seq, 8, 4, 2).await;
    assert_eq!(begin.status, 200);
    let upload_id = begin.json()["session"]["upload_id"]
        .as_str()
        .expect("upload id")
        .to_owned();

    // 上传令牌：绑定 (上传会话, 设备 21, blob:upload)。
    let issued = issue_capability(&node, 21, &upload_id, "blob:upload", Some(120)).await;
    assert_eq!(
        issued.status,
        200,
        "issue: {:?}",
        String::from_utf8_lossy(&issued.body)
    );
    let capability = issued.json()["capability"].clone();
    let upload_token = capability["token"].as_str().expect("token").to_owned();
    assert_eq!(capability["object_id"], upload_id);
    assert_eq!(capability["scope"], "blob:upload");
    assert_eq!(capability["device_id"], device_text(21));

    // 带令牌写块：通过。
    let uploaded = put_chunk_with_capability(&node, 21, &upload_id, 0, first, &upload_token).await;
    assert_eq!(uploaded.status, 200);
    // 同一个令牌写第二块也通过（同一对象、同一用途）。
    assert_eq!(
        put_chunk_with_capability(&node, 21, &upload_id, 1, second, &upload_token).await.status,
        200
    );
    let digest = ciphertext_digest(&[first, second]);
    assert_eq!(complete_blob(&node, 21, &upload_id, 2, 8, &digest).await.status, 200);

    // 下载令牌：绑定已发布对象与 blob:download。
    let download = issue_capability(&node, 21, &blob, "blob:download", None).await;
    assert_eq!(download.status, 200);
    let download_token = download.json()["capability"]["token"]
        .as_str()
        .expect("token")
        .to_owned();
    let chunk = read_chunk_with_capability(&node, 21, &blob, 1, &download_token).await;
    assert_eq!(chunk.status, 200);
    assert_eq!(chunk.body, second);

    let wrong_scope = read_chunk_with_capability(&node, 21, &blob, 0, &upload_token).await;
    assert_eq!(wrong_scope.status, 403);
    assert_eq!(wrong_scope.error_code(), "INVALID_CAPABILITY");
    // 下载令牌也不能写块。
    let wrong_scope = put_chunk_with_capability(&node, 21, &upload_id, 0, first, &download_token).await;
    assert_eq!(wrong_scope.status, 403);
    assert_eq!(wrong_scope.error_code(), "INVALID_CAPABILITY");
}

#[tokio::test]
async fn capability_tokens_are_bound_to_one_object_and_one_device() {
    let (node, group) = setup(NodeAuthPolicy::default()).await;
    let source_seq = append_attachment_entry(&node, &group, 21).await;
    let blob_a = object_text(142);
    let blob_b = object_text(143);
    let first: &[u8] = b"abcd";
    let second: &[u8] = b"efgh";
    let begin_a = begin_blob(&node, 21, &group, &operation_text(242), &blob_a, source_seq, 8, 4, 2).await;
    let upload_a = begin_a.json()["session"]["upload_id"].as_str().expect("id").to_owned();
    let begin_b = begin_blob(&node, 21, &group, &operation_text(243), &blob_b, source_seq, 8, 4, 2).await;
    let upload_b = begin_b.json()["session"]["upload_id"].as_str().expect("id").to_owned();

    let token_a = issue_capability(&node, 21, &upload_a, "blob:upload", Some(120))
        .await
        .json()["capability"]["token"]
        .as_str()
        .expect("token")
        .to_owned();

    // 跨对象重放：A 的令牌写 B 的会话 → 拒绝。
    let replay = put_chunk_with_capability(&node, 21, &upload_b, 0, first, &token_a).await;
    assert_eq!(replay.status, 403);
    assert_eq!(replay.error_code(), "INVALID_CAPABILITY");
    // 正确对象仍然可以。
    assert_eq!(
        put_chunk_with_capability(&node, 21, &upload_a, 0, second, &token_a).await.status,
        200
    );

    // 篡改令牌（改密文中的一位）→ 签名/编码校验失败。
    let mut tampered = token_a.clone().into_bytes();
    let position = tampered.len() - 2;
    tampered[position] = if tampered[position] == b"a"[0] { b"b"[0] } else { b"a"[0] };
    let tampered = String::from_utf8(tampered).expect("utf8");
    let forged = put_chunk_with_capability(&node, 21, &upload_a, 1, first, &tampered).await;
    assert!(
        forged.status == 401 || forged.status == 403,
        "forged token must be rejected, got {}",
        forged.status
    );

    // 别人的会话拿不到上传令牌（对象级授权先行）。
    let denied = issue_capability(&node, 22, &upload_a, "blob:upload", None).await;
    assert_eq!(denied.status, 403);
    assert_eq!(denied.error_code(), "PERMISSION_DENIED");
    // 不存在的对象也不发令牌。
    let missing = issue_capability(&node, 21, &object_text(144), "blob:download", None).await;
    assert_eq!(missing.status, 404);
    assert_eq!(missing.error_code(), "BLOB_NOT_FOUND");
    // 非法用途文本。
    let bad_scope = issue_capability(&node, 21, &upload_a, "blob:delete", None).await;
    assert_eq!(bad_scope.status, 400);
    assert_eq!(bad_scope.error_code(), "INVALID_REQUEST");
}

// ---------------------------------------------------------------- DM 包领域操作声明

/// 带领域声明的控制候选请求体（客户端先按含声明的形态签名）。
fn declared_entry_body(
    candidate: &GroupCandidate,
    signature: &str,
    declaration: Option<&CandidateDomainOperation>,
) -> Vec<u8> {
    let mut body = serde_json::json!({
        "schema_version": 1,
        "candidate": candidate,
        "author_signature": signature,
    });
    if let Some(declaration) = declaration {
        body["domainOperation"] = serde_json::to_value(declaration).expect("declaration json");
    }
    serde_json::to_vec(&body).expect("declared entry body")
}

/// 构造控制候选本身（不带签名）。
fn control_candidate_struct(
    group: &str,
    author_seed: u8,
    submission_seed: u8,
    payload: &[u8],
    expected_control_head: [u8; 32],
) -> GroupCandidate {
    GroupCandidate {
        protocol_major: 1,
        protocol_minor: 0,
        submission_id: submission_text(submission_seed),
        group_id: group.to_owned(),
        author_device_id: device_text(author_seed),
        kind: KIND_CONTROL.to_owned(),
        expected_control_head: hex_encode(&expected_control_head),
        policy_version: 1,
        mls_epoch: 0,
        encrypted_payload: hex_encode(payload),
        control_metadata: Some(CandidateControlMetadata {
            operation: "invite".into(),
            target_digest: hex_encode(&[0x33_u8; 32]),
        }),
    }
}

#[tokio::test]
async fn domain_operation_declaration_is_accepted_and_visible_in_the_log_api() {
    let (node, group) = setup(NodeAuthPolicy::default()).await;
    let mut candidate = control_candidate_struct(&group, 21, 251, b"sealed control commit", INITIAL_CONTROL_HEAD);
    candidate.control_metadata = Some(CandidateControlMetadata {
        operation: "set_role".into(),
        target_digest: hex_encode(&[0x33_u8; 32]),
    });
    let declaration = CandidateDomainOperation {
        operation: "transfer_ownership".into(),
        target_digest: hex_encode(&[0x5a_u8; 32]),
    };
    let unsigned = candidate
        .to_unsigned_with_domain(chat_protocol::MAX_CIPHERTEXT_BYTES, Some(&declaration))
        .expect("candidate converts");
    let signing = candidate_signing_bytes(&unsigned).expect("signing bytes");
    let signature: Signature = device(21).sign(signing.as_bytes());
    let body = declared_entry_body(&candidate, &hex_encode(&signature.to_bytes()), Some(&declaration));
    let path = format!("/v1/groups/{group}/entries");
    let headers = request_headers(&node, 21, "POST", &path, "", &body).await;
    let appended = http(&node.address, "POST", &path, &headers, &body).await;
    assert_eq!(
        appended.status,
        200,
        "declared candidate: {}",
        String::from_utf8_lossy(&appended.body)
    );
    assert_eq!(appended.json()["result"]["seq"], 1);

    // 日志接口暴露节点侧投影（文本 + 目标摘要 hex）。
    let log_path = format!("/v1/groups/{group}/log");
    let headers = request_headers(&node, 21, "GET", &log_path, "after=0&limit=10", b"").await;
    let page = http(&node.address, "GET", &format!("{log_path}?after=0&limit=10"), &headers, b"").await;
    assert_eq!(page.status, 200);
    let entry = &page.json()["page"]["entries"][0];
    assert_eq!(entry["domain_operation"], "transfer_ownership");
    assert_eq!(entry["domain_target_digest"], hex_encode(&[0x5a_u8; 32]));
}

#[tokio::test]
async fn illegal_domain_operation_declarations_are_rejected() {
    let (node, group) = setup(NodeAuthPolicy::default()).await;
    let mut candidate = control_candidate_struct(&group, 21, 252, b"sealed", INITIAL_CONTROL_HEAD);
    // 公开操作 invite + 声明 transfer_ownership：协议层不允许这种组合。
    candidate.control_metadata = Some(CandidateControlMetadata {
        operation: "invite".into(),
        target_digest: hex_encode(&[0x33_u8; 32]),
    });
    let declaration = CandidateDomainOperation {
        operation: "transfer_ownership".into(),
        target_digest: hex_encode(&[0x5a_u8; 32]),
    };
    assert_eq!(
        candidate
            .to_unsigned_with_domain(chat_protocol::MAX_CIPHERTEXT_BYTES, Some(&declaration))
            .expect_err("illegal combination")
            .code(),
        "INVALID_CANDIDATE"
    );
    // 未知枚举被拒。
    let unknown = CandidateDomainOperation {
        operation: "rotate_everything".into(),
        target_digest: hex_encode(&[0x5a_u8; 32]),
    };
    assert_eq!(
        candidate
            .to_unsigned_with_domain(chat_protocol::MAX_CIPHERTEXT_BYTES, Some(&unknown))
            .expect_err("unknown domain operation")
            .code(),
        "INVALID_CANDIDATE"
    );
    let _ = node;
}

#[tokio::test]
async fn legacy_entry_requests_without_a_declaration_still_work() {
    let (node, group) = setup(NodeAuthPolicy::default()).await;
    let candidate = control_candidate_struct(&group, 21, 253, b"legacy sealed", INITIAL_CONTROL_HEAD);
    let unsigned = candidate
        .to_unsigned(chat_protocol::MAX_CIPHERTEXT_BYTES)
        .expect("candidate converts");
    let signing = candidate_signing_bytes(&unsigned).expect("signing bytes");
    let signature: Signature = device(21).sign(signing.as_bytes());
    let body = entry_body(&candidate, &hex_encode(&signature.to_bytes()));
    let path = format!("/v1/groups/{group}/entries");
    let headers = request_headers(&node, 21, "POST", &path, "", &body).await;
    let appended = http(&node.address, "POST", &path, &headers, &body).await;
    assert_eq!(appended.status, 200);
    let log_path = format!("/v1/groups/{group}/log");
    let headers = request_headers(&node, 21, "GET", &log_path, "after=0&limit=10", b"").await;
    let page = http(&node.address, "GET", &format!("{log_path}?after=0&limit=10"), &headers, b"").await;
    assert_eq!(page.status, 200);
    assert!(page.json()["page"]["entries"][0]["domain_operation"].is_null());
}
