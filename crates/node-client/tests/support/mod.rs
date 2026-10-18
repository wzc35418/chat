//! 测试支撑：真 TCP 的 node-api 服务 + 真账户/设备凭据（不绕过节点任何安全校验）。
#![allow(dead_code)]

use std::sync::Arc;

use chat_node_api::build_router;
use chat_node_api::state::{AppState, FixedClock};
use chat_node_client::NodeEndpoint;
use chat_node_services::canonical::hex_encode;
use chat_node_services::group::{
    GroupCandidate, GroupCreation, GroupMemberSpec, KIND_APPLICATION,
};
use chat_node_services::mailbox::MailboxItem;
use chat_node_services::registration::{
    DeviceCredential, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest, RegistrationCapability,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use chat_protocol::{DeviceId, GroupId, SubmissionId};
use ed25519_dalek::{Signature, Signer, SigningKey};

/// 固定测试时间。
pub const NOW: u64 = 1_700_000_000;
/// 客户端自定的初始控制头摘要。
pub const INITIAL_CONTROL_HEAD: [u8; 32] = [0x11; 32];

/// 协议规范设备标识。
pub fn device_text(seed: u8) -> String {
    DeviceId::from_bytes([seed; 32]).encode()
}

/// 协议规范群标识。
pub fn group_text(seed: u8) -> String {
    GroupId::from_bytes([seed; 32]).encode()
}

/// 协议规范提交标识。
pub fn submission_text(seed: u8) -> String {
    SubmissionId::from_bytes([seed; 32]).encode()
}

/// 一台测试设备的密钥与标识。
#[derive(Clone)]
pub struct TestDevice {
    pub device_id: String,
    pub key: SigningKey,
}

impl TestDevice {
    pub fn new(seed: u8) -> Self {
        Self {
            device_id: device_text(seed),
            key: SigningKey::from_bytes(&[seed; 32]),
        }
    }

    /// 该设备模拟真实传输失败时可能发生的事：用另一把私钥冒充同一设备。
    pub fn with_impostor_key(seed: u8, key_seed: u8) -> Self {
        Self {
            device_id: device_text(seed),
            key: SigningKey::from_bytes(&[key_seed; 32]),
        }
    }
}

/// 在进程内起的真实节点（axum + 真 TCP 随机端口）。
pub struct TestNode {
    pub address: String,
    pub clock: Arc<FixedClock>,
    pub node_id: String,
    pub node_public_key: [u8; 32],
    pub supervisor: SigningKey,
    pub account_id: String,
}

impl TestNode {
    /// 固定节点公钥的端点（客户端应据此验证每个挑战与节点产物）。
    pub fn endpoint(&self) -> NodeEndpoint {
        NodeEndpoint::new(&format!("http://{}", self.address), &self.node_id)
            .expect("endpoint")
            .with_node_public_key(self.node_public_key)
    }

    /// 未固定节点公钥的端点（只绑定 node_id）。
    pub fn unpinned_endpoint(&self, node_id: &str) -> NodeEndpoint {
        NodeEndpoint::new(&format!("http://{}", self.address), node_id).expect("endpoint")
    }
}

/// 起一个真实的 node-api 服务；返回地址、时钟与节点身份。
pub async fn start_node(policy: NodeAuthPolicy) -> TestNode {
    let service = NodeService::open_in_memory(NodeServiceConfig {
        node_id: "node-client-test".into(),
        signing_seed: [7_u8; 32],
        policy,
    })
    .expect("node service opens");
    let node_public_key = service.node_public_key();
    let clock = Arc::new(FixedClock::new(NOW));
    let state = AppState::new(service, clock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("local addr").to_string();
    tokio::spawn(async move {
        let _ = axum::serve(listener, build_router(state)).await;
    });
    TestNode {
        address,
        clock,
        node_id: "node-client-test".into(),
        node_public_key,
        supervisor: SigningKey::from_bytes(&[11_u8; 32]),
        account_id: "acct-client".into(),
    }
}

/// 账户主管钥签发的设备凭据。
pub fn credential(node: &TestNode, device: &TestDevice, now: u64) -> DeviceCredential {
    let mut credential = DeviceCredential {
        account_id: node.account_id.clone(),
        device_id: device.device_id.clone(),
        device_public_key: hex_encode(&device.key.verifying_key().to_bytes()),
        endpoint_id: format!("endpoint-{}", &device.device_id[..device.device_id.len().min(24)]),
        credential_version: 1,
        not_before: now.saturating_sub(10),
        expires_at: now + 3_600,
        supervisor_public_key: hex_encode(&node.supervisor.verifying_key().to_bytes()),
        signature: String::new(),
    };
    let signature: Signature = node.supervisor.sign(&credential.signing_bytes().expect("bytes"));
    credential.signature = hex_encode(&signature.to_bytes());
    credential
}

/// 账户主管钥签发的一次性注册能力。
pub fn capability(
    node: &TestNode,
    capability_id: &str,
    device: &TestDevice,
    now: u64,
) -> RegistrationCapability {
    let mut capability = RegistrationCapability {
        capability_id: capability_id.to_owned(),
        account_id: node.account_id.clone(),
        device_id: device.device_id.clone(),
        purpose: REGISTRATION_CAPABILITY_PURPOSE.into(),
        issued_at: now.saturating_sub(10),
        expires_at: now + 600,
        revocation_version: 1,
        supervisor_public_key: hex_encode(&node.supervisor.verifying_key().to_bytes()),
        signature: String::new(),
    };
    let signature: Signature = node.supervisor.sign(&capability.signing_bytes().expect("bytes"));
    capability.signature = hex_encode(&signature.to_bytes());
    capability
}

/// 设备注册请求（只用公开材料）。
pub fn registration_request(
    node: &TestNode,
    device: &TestDevice,
    capability_id: &str,
    now: u64,
) -> RegisterDeviceRequest {
    RegisterDeviceRequest {
        account_id: node.account_id.clone(),
        credential: credential(node, device, now),
        capability: capability(node, capability_id, device, now),
    }
}

/// 群创建请求（创建设备稍后用客户端签名）。
pub fn group_creation(group_id: &str, members: &[(&TestDevice, &str)]) -> GroupCreation {
    GroupCreation {
        group_id: group_id.to_owned(),
        owner_account_id: "acct-client".into(),
        initial_control_head: hex_encode(&INITIAL_CONTROL_HEAD),
        policy_version: 1,
        mls_epoch: 0,
        members: members
            .iter()
            .map(|(device, role)| GroupMemberSpec {
                device_id: device.device_id.clone(),
                role: (*role).to_owned(),
            })
            .collect(),
    }
}

/// 应用候选（客户端稍后用作者设备签名）。
pub fn application_candidate(
    group_id: &str,
    author: &TestDevice,
    submission_seed: u8,
    payload: &[u8],
) -> GroupCandidate {
    GroupCandidate {
        protocol_major: 1,
        protocol_minor: 0,
        submission_id: submission_text(submission_seed),
        group_id: group_id.to_owned(),
        author_device_id: author.device_id.clone(),
        kind: KIND_APPLICATION.to_owned(),
        expected_control_head: hex_encode(&INITIAL_CONTROL_HEAD),
        policy_version: 1,
        mls_epoch: 0,
        encrypted_payload: hex_encode(payload),
        control_metadata: None,
    }
}

/// 密文信箱条目（指向作者已接受的日志条目）。
pub fn mailbox_item(
    item_id: &str,
    group_id: &str,
    target: &TestDevice,
    source_seq: u64,
    payload: &[u8],
    now: u64,
) -> MailboxItem {
    MailboxItem {
        item_id: item_id.to_owned(),
        group_id: group_id.to_owned(),
        target_device_id: target.device_id.clone(),
        source_seq,
        expires_at: now + 600,
        ciphertext_envelope: hex_encode(payload),
    }
}
