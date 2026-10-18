//! 测试支撑：真实节点服务 + 真实设备注册/鉴权/群创建（不绕过任何安全校验）。
//!
//! 这里只封装“客户端行为”：签发挑战、用设备私钥签名、构造协议规范 ID 文本。
//! 服务端逻辑全部走公开 API，测试不直接写库（除 group.rs 内的白盒单元测试）。
#![allow(dead_code)]

use chat_node_services::auth::{
    ChallengePurpose, RequestMeta, SignedAuth, request_signing_bytes,
};
use chat_node_services::canonical::{hex_decode, hex_encode, sha256};
use chat_node_services::error::NodeError;
use chat_node_services::group::{
    CandidateControlMetadata, GroupCandidate, GroupCreation, GroupMemberSpec, SignedCandidate,
    SignedGroupCreation, KIND_APPLICATION, KIND_CONTROL, ROLE_OWNER,
};
use chat_node_services::mailbox::MailboxItem;
use chat_node_services::registration::{
    DeviceCredential, DeviceRegistration, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest,
    RegistrationCapability,
};
use chat_node_services::services::{
    AuthenticatedDevice, NodeAuthPolicy, NodeService, NodeServiceConfig,
};
use chat_protocol::{DeviceId, GroupId, SubmissionId, candidate_signing_bytes};
use ed25519_dalek::{Signature, Signer, SigningKey};
use std::path::Path;

/// 固定测试时间。
pub const NOW: u64 = 1_700_000_000;
/// 初始控制头（客户端自定的起点摘要）。
pub const INITIAL_CONTROL_HEAD: [u8; 32] = [0x11; 32];
/// 候选密文上限（与节点默认策略一致）。
pub const MAX_CIPHERTEXT: usize = chat_node_services::limits::MAX_GROUP_CANDIDATE_CIPHERTEXT_BYTES;

/// 设备标识使用协议规范文本，因此同一 seed 永远得到同一 DeviceId。
pub fn device_text(seed: u8) -> String {
    DeviceId::from_bytes([seed; 32]).encode()
}

/// 群标识使用协议规范文本。
pub fn group_text(seed: u8) -> String {
    GroupId::from_bytes([seed; 32]).encode()
}

/// 提交标识使用协议规范文本。
pub fn submission_text(seed: u8) -> String {
    SubmissionId::from_bytes([seed; 32]).encode()
}

/// 一台设备的测试夹具。
#[derive(Clone)]
pub struct TestDevice {
    pub seed: u8,
    pub device_id: String,
    pub key: SigningKey,
}

pub struct Harness {
    pub service: NodeService,
    pub supervisor: SigningKey,
    pub account_id: String,
    pub node_id: String,
}

impl Harness {
    /// 内存节点库 + 默认策略。
    pub fn in_memory() -> Self {
        Self::in_memory_with(NodeAuthPolicy::default())
    }

    pub fn in_memory_with(policy: NodeAuthPolicy) -> Self {
        let config = NodeServiceConfig {
            node_id: "node-group-test".into(),
            signing_seed: [7_u8; 32],
            policy,
        };
        Self {
            service: NodeService::open_in_memory(config).expect("node opens"),
            supervisor: SigningKey::from_bytes(&[11_u8; 32]),
            account_id: "acct-a".into(),
            node_id: "node-group-test".into(),
        }
    }

    /// 文件节点库（用于重启/并发/崩溃证据）。
    pub fn open(path: &Path, policy: NodeAuthPolicy) -> Self {
        let config = NodeServiceConfig {
            node_id: "node-group-test".into(),
            signing_seed: [7_u8; 32],
            policy,
        };
        Self {
            service: NodeService::open(path, config).expect("node file opens"),
            supervisor: SigningKey::from_bytes(&[11_u8; 32]),
            account_id: "acct-a".into(),
            node_id: "node-group-test".into(),
        }
    }

    /// 用另一个账户主管钥登记另一账户的设备。
    pub fn switch_account(&mut self, supervisor_seed: u8, account_id: &str) {
        self.supervisor = SigningKey::from_bytes(&[supervisor_seed; 32]);
        self.account_id = account_id.to_owned();
    }

    /// 设备和标识。
    pub fn device(seed: u8) -> TestDevice {
        TestDevice {
            seed,
            device_id: device_text(seed),
            key: SigningKey::from_bytes(&[seed; 32]),
        }
    }

    fn credential(&self, device: &TestDevice, now: u64) -> DeviceCredential {
        let mut credential = DeviceCredential {
            account_id: self.account_id.clone(),
            device_id: device.device_id.clone(),
            device_public_key: hex_encode(&device.key.verifying_key().to_bytes()),
            endpoint_id: format!("endpoint-{}", device.seed),
            credential_version: 1,
            not_before: now.saturating_sub(10),
            expires_at: now + 3_600,
            supervisor_public_key: hex_encode(&self.supervisor.verifying_key().to_bytes()),
            signature: String::new(),
        };
        let signature: Signature = self.supervisor.sign(&credential.signing_bytes().unwrap());
        credential.signature = hex_encode(&signature.to_bytes());
        credential
    }

    fn capability(&self, device: &TestDevice, now: u64) -> RegistrationCapability {
        let mut capability = RegistrationCapability {
            capability_id: format!("cap-{}", device.seed),
            account_id: self.account_id.clone(),
            device_id: device.device_id.clone(),
            purpose: REGISTRATION_CAPABILITY_PURPOSE.into(),
            issued_at: now.saturating_sub(10),
            expires_at: now + 600,
            revocation_version: 1,
            supervisor_public_key: hex_encode(&self.supervisor.verifying_key().to_bytes()),
            signature: String::new(),
        };
        let signature: Signature = self.supervisor.sign(&capability.signing_bytes().unwrap());
        capability.signature = hex_encode(&signature.to_bytes());
        capability
    }

    /// 走完整 R-02 流程登记一台设备。
    pub fn register(&mut self, device: &TestDevice, now: u64) -> Result<DeviceRegistration, NodeError> {
        let request = RegisterDeviceRequest {
            account_id: self.account_id.clone(),
            credential: self.credential(device, now),
            capability: self.capability(device, now),
        };
        let body_hash = sha256(&serde_json::to_vec(&request).unwrap());
        let meta = RequestMeta::canonical("POST", "/v1/devices", "").unwrap();
        let challenge = self
            .service
            .issue_auth_challenge(ChallengePurpose::DeviceRegistration, now)?;
        let nonce = hex_decode::<32>("nonce", &challenge.nonce).unwrap();
        let signed = request_signing_bytes(
            &device.device_id,
            &meta,
            &body_hash,
            &nonce,
            self.service.node_id(),
            ChallengePurpose::DeviceRegistration,
        );
        let signature: Signature = device.key.sign(&signed);
        let auth = SignedAuth {
            challenge_id: challenge.challenge_id,
            device_id: device.device_id.clone(),
            nonce: challenge.nonce,
            signature: hex_encode(&signature.to_bytes()),
        };
        let proof = self.service.verify_device_signature(
            &meta,
            &body_hash,
            &auth,
            &device.key.verifying_key().to_bytes(),
            ChallengePurpose::DeviceRegistration,
            now,
        )?;
        self.service.register_device(&request, &proof, now)
    }

    /// 走完整 API-003 鉴权，返回已登记设备身份。
    pub fn authenticate(&mut self, device: &TestDevice, now: u64) -> Result<AuthenticatedDevice, NodeError> {
        let meta = RequestMeta::canonical("POST", "/v1/groups", "").unwrap();
        let body_hash = sha256(b"authenticated call");
        let challenge = self
            .service
            .issue_auth_challenge(ChallengePurpose::RequestAuth, now)?;
        let nonce = hex_decode::<32>("nonce", &challenge.nonce).unwrap();
        let signed = request_signing_bytes(
            &device.device_id,
            &meta,
            &body_hash,
            &nonce,
            self.service.node_id(),
            ChallengePurpose::RequestAuth,
        );
        let signature: Signature = device.key.sign(&signed);
        let auth = SignedAuth {
            challenge_id: challenge.challenge_id,
            device_id: device.device_id.clone(),
            nonce: challenge.nonce,
            signature: hex_encode(&signature.to_bytes()),
        };
        self.service.authenticate_request(
            &meta,
            &body_hash,
            &auth,
            ChallengePurpose::RequestAuth,
            now,
        )
    }

    /// 登记 + 鉴权，返回 actor。
    pub fn registered_actor(&mut self, device: &TestDevice, now: u64) -> AuthenticatedDevice {
        self.register(device, now).expect("device registers");
        self.authenticate(device, now).expect("device authenticates")
    }

    /// 构造设备签名的群创建请求。
    pub fn signed_creation(
        &self,
        creator: &TestDevice,
        group_id: &str,
        member_seeds: &[(u8, &str)],
    ) -> SignedGroupCreation {
        let creation = GroupCreation {
            group_id: group_id.to_owned(),
            owner_account_id: self.account_id.clone(),
            initial_control_head: hex_encode(&INITIAL_CONTROL_HEAD),
            policy_version: 1,
            mls_epoch: 0,
            members: member_seeds
                .iter()
                .map(|(seed, role)| GroupMemberSpec {
                    device_id: device_text(*seed),
                    role: (*role).to_owned(),
                })
                .collect(),
        };
        let signature: Signature = creator.key.sign(&creation.signing_bytes().unwrap());
        SignedGroupCreation {
            creation,
            device_signature: hex_encode(&signature.to_bytes()),
        }
    }

    /// 单成员群（创建者既是唯一群主）。
    pub fn single_member_group(
        &self,
        creator: &TestDevice,
        group_id: &str,
    ) -> SignedGroupCreation {
        self.signed_creation(creator, group_id, &[(creator.seed, ROLE_OWNER)])
    }

    /// 构造作者签名的候选。
    #[allow(clippy::too_many_arguments)]
    pub fn signed_candidate(
        &self,
        author: &TestDevice,
        group_id: &str,
        submission_seed: u8,
        payload: &[u8],
        kind: &str,
        expected_control_head: [u8; 32],
        control_metadata: Option<(&str, [u8; 32])>,
    ) -> SignedCandidate {
        self.signed_candidate_full(
            author,
            group_id,
            submission_seed,
            payload,
            kind,
            expected_control_head,
            control_metadata,
            1,
            0,
        )
    }

    /// 显式声明 policy_version / mls_epoch 的候选（用于 STALE_* 拒绝用例）。
    #[allow(clippy::too_many_arguments)]
    pub fn signed_candidate_full(
        &self,
        author: &TestDevice,
        group_id: &str,
        submission_seed: u8,
        payload: &[u8],
        kind: &str,
        expected_control_head: [u8; 32],
        control_metadata: Option<(&str, [u8; 32])>,
        policy_version: u64,
        mls_epoch: u64,
    ) -> SignedCandidate {
        let candidate = GroupCandidate {
            protocol_major: 1,
            protocol_minor: 0,
            submission_id: submission_text(submission_seed),
            group_id: group_id.to_owned(),
            author_device_id: author.device_id.clone(),
            kind: kind.to_owned(),
            expected_control_head: hex_encode(&expected_control_head),
            policy_version,
            mls_epoch,
            encrypted_payload: hex_encode(payload),
            control_metadata: control_metadata.map(|(operation, target)| CandidateControlMetadata {
                operation: operation.to_owned(),
                target_digest: hex_encode(&target),
            }),
        };
        // 客户端按协议上限构造；节点策略上限（更小）由服务层显式拒绝。
        let unsigned = candidate
            .to_unsigned(chat_protocol::MAX_CIPHERTEXT_BYTES)
            .expect("candidate converts");
        let bytes = candidate_signing_bytes(&unsigned).expect("candidate signs");
        let signature: Signature = author.key.sign(bytes.as_bytes());
        SignedCandidate {
            candidate,
            author_signature: hex_encode(&signature.to_bytes()),
        }
    }

    /// 应用候选（无控制元数据）。
    pub fn application_candidate(
        &self,
        author: &TestDevice,
        group_id: &str,
        submission_seed: u8,
        payload: &[u8],
    ) -> SignedCandidate {
        self.signed_candidate(
            author,
            group_id,
            submission_seed,
            payload,
            KIND_APPLICATION,
            INITIAL_CONTROL_HEAD,
            None,
        )
    }

    /// 控制候选。
    pub fn control_candidate(
        &self,
        author: &TestDevice,
        group_id: &str,
        submission_seed: u8,
        expected_control_head: [u8; 32],
        payload: &[u8],
    ) -> SignedCandidate {
        self.signed_candidate(
            author,
            group_id,
            submission_seed,
            payload,
            KIND_CONTROL,
            expected_control_head,
            Some(("invite", [0x33; 32])),
        )
    }

    /// 构造一条指向已接受条目的信箱条目。
    pub fn mailbox_item(
        &self,
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
            expires_at: now + 3_600,
            ciphertext_envelope: hex_encode(payload),
        }
    }

    /// 节点固定公钥（客户端据此验证回执/head）。
    pub fn node_public_key(&self) -> [u8; 32] {
        self.service.node_public_key()
    }
}

