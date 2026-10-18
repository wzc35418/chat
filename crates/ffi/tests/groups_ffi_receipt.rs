//! 群治理经**真实节点回执**的 FFI 端到端用例（真 axum 节点 + 真 OpenMLS + 真临时加密库）。
//!
//! 覆盖：建群 → 改角色产生 pending 控制提交 → 经 FFI 提交到节点拿到真回执（seq >= 1）→
//! 用回执接受后 epoch 前进且快照一致；伪造回执被拒且 epoch/pending 不变；节点不可达 NODE_UNAVAILABLE。
//!
//! 说明：本实现里 `rename` 是**纯策略层**操作（不产生 MLS 提交），因此“pending 控制提交”用
//! `group_set_role` 产生（SelfUpdate），与 group_node 端到端用例的取法一致。

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_ffi::groups_ffi::{ControlReceiptFfi, GroupFfi, GroupViewFfi};
use chat_ffi::local_store::FfiError;
use chat_core::service::{ConversationCore, IdentityAccountInput, IdentityAccountView};
use chat_node_api::build_router;
use chat_node_api::state::{AppState, FixedClock as NodeFixedClock};
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_node_services::canonical::hex_encode;
use chat_node_services::group::{GroupCreation, GroupMemberSpec};
use chat_node_services::registration::{
    DeviceCredential, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest, RegistrationCapability,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use ed25519_dalek::{Signature, Signer, SigningKey};

const NOW_S: u64 = 1_700_000_000;
const ACCOUNT: &str = "acct-ffi-gn";
const NODE_ID: &str = "node-ffi-gn";
const MASTER_KEY: [u8; 32] = [0x5a; 32];
const DEVICE_SEED: [u8; 32] = [0x33; 32];
const ACCOUNT_SEED: [u8; 32] = [0x22; 32];
const SUPERVISOR_SEED: [u8; 32] = [0x11; 32];
const UNREACHABLE_ENDPOINT: &str = "http://127.0.0.1:1";

struct FixtureDir(PathBuf);

impl FixtureDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "chat-ffi-gn-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> String {
        self.0.join("chat.db").to_string_lossy().into_owned()
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn master_key_hex() -> String {
    MASTER_KEY.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn device_key() -> SigningKey {
    SigningKey::from_bytes(&DEVICE_SEED)
}

fn supervisor_key() -> SigningKey {
    SigningKey::from_bytes(&SUPERVISOR_SEED)
}

fn acct(seed: &[u8; 32]) -> String {
    seed.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 用**已知种子**在库里建立持久身份（这样测试也能向节点登记同一台设备）。
fn enroll_identity(database: &str, master_key_hex: &str) -> IdentityAccountView {
    let mut core = ConversationCore::open(database, master_key_hex).expect("open core");
    let view = core
        .save_identity_account(IdentityAccountInput {
            display_name: "FFI 节点回执".to_owned(),
            account_seed_hex: acct(&ACCOUNT_SEED),
            device_seed_hex: acct(&DEVICE_SEED),
            created_at_ms: NOW_S * 1_000,
        })
        .expect("save identity");
    core.close();
    view
}

fn code(error: FfiError) -> String {
    match error {
        FfiError::Store { code, .. } => code,
    }
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(future)
}

// ---------------------------------------------------------------- 真实节点

struct TestNode {
    address: String,
    node_public_key: [u8; 32],
}

impl TestNode {
    fn endpoint(&self) -> NodeEndpoint {
        NodeEndpoint::new(&format!("http://{}", self.address), NODE_ID)
            .expect("endpoint")
            .with_node_public_key(self.node_public_key)
    }

    fn endpoint_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn public_key_hex(&self) -> String {
        hex_encode(&self.node_public_key)
    }

    fn client(&self, device_id: &str) -> NodeClient {
        NodeClient::new(self.endpoint(), device_id, device_key()).expect("node client")
    }

    fn registration_request(&self, device_id: &str) -> RegisterDeviceRequest {
        let supervisor = supervisor_key();
        let supervisor_public_key = hex_encode(&supervisor.verifying_key().to_bytes());
        let mut credential = DeviceCredential {
            account_id: ACCOUNT.into(),
            device_id: device_id.to_owned(),
            device_public_key: hex_encode(&device_key().verifying_key().to_bytes()),
            endpoint_id: "endpoint-ffi-gn".into(),
            credential_version: 1,
            not_before: NOW_S - 10,
            expires_at: NOW_S + 3_600,
            supervisor_public_key: supervisor_public_key.clone(),
            signature: String::new(),
        };
        let signature: Signature = supervisor.sign(&credential.signing_bytes().expect("credential"));
        credential.signature = hex_encode(&signature.to_bytes());
        let mut capability = RegistrationCapability {
            capability_id: "cap-ffi-gn".into(),
            account_id: ACCOUNT.into(),
            device_id: device_id.to_owned(),
            purpose: REGISTRATION_CAPABILITY_PURPOSE.into(),
            issued_at: NOW_S - 10,
            expires_at: NOW_S + 600,
            revocation_version: 1,
            supervisor_public_key,
            signature: String::new(),
        };
        let signature: Signature = supervisor.sign(&capability.signing_bytes().expect("capability"));
        capability.signature = hex_encode(&signature.to_bytes());
        RegisterDeviceRequest {
            account_id: ACCOUNT.into(),
            credential,
            capability,
        }
    }
}

fn start_node() -> TestNode {
    let service = NodeService::open_in_memory(NodeServiceConfig {
        node_id: NODE_ID.into(),
        signing_seed: [7_u8; 32],
        policy: NodeAuthPolicy::default(),
    })
    .expect("node service opens");
    let node_public_key = service.node_public_key();
    let state = AppState::new(service, Arc::new(NodeFixedClock::new(NOW_S)));
    let router = build_router(state);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    listener.set_nonblocking(true).expect("nonblocking listener");
    let address = listener.local_addr().expect("local addr").to_string();
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(async move {
            let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
                return;
            };
            let _ = axum::serve(listener, router).await;
        });
    });
    TestNode {
        address,
        node_public_key,
    }
}

// ---------------------------------------------------------------- 用例夹具

struct Harness {
    dir: FixtureDir,
    node: TestNode,
    ffi: Arc<GroupFfi>,
    group_id: String,
    device_id: String,
}

impl Harness {
    /// 建持久身份（已知种子，便于向节点登记同一设备）→ 起真节点并登记设备/群 → 打开 FFI。
    fn new() -> Self {
        let dir = FixtureDir::new();
        let identity = enroll_identity(&dir.database(), &master_key_hex());
        let device_id = identity.device_id;

        let node = start_node();
        let client = node.client(&device_id);
        block_on(client.register_device(&node.registration_request(&device_id)))
            .expect("device registers on the node");

        let ffi = GroupFfi::open(dir.database(), master_key_hex(), Some(NODE_ID.to_owned()))
            .expect("open group ffi");
        // 空的 local_credential → 使用库里的持久身份设备 ID（与节点登记的设备一致）。
        let view = ffi
            .group_create(
                "FFI 回执测试群".to_owned(),
                String::new(),
                "bob".to_owned(),
                Vec::new(),
            )
            .expect("create group");
        assert_eq!(view.members[0].credential, device_id);

        let creation = GroupCreation {
            group_id: view.group_id.clone(),
            owner_account_id: ACCOUNT.into(),
            initial_control_head: hex_encode(&[0_u8; 32]),
            policy_version: 1,
            mls_epoch: 0,
            members: vec![GroupMemberSpec {
                device_id: device_id.clone(),
                role: "owner".into(),
            }],
        };
        block_on(client.create_group(&creation)).expect("group registers on the node");

        Self {
            dir,
            node,
            ffi,
            group_id: view.group_id,
            device_id,
        }
    }

    fn snapshot(&self) -> GroupViewFfi {
        self.ffi
            .group_snapshot(self.group_id.clone())
            .expect("snapshot")
    }

    /// 群主改 bob 的角色 → 真实 MLS 控制提交（pending，epoch 不前进）。
    fn prepare_role_change(&self) -> GroupViewFfi {
        let view = self
            .ffi
            .group_set_role(
                self.group_id.clone(),
                self.device_id.clone(),
                "bob".to_owned(),
                "admin".to_owned(),
            )
            .expect("prepare role change");
        assert_eq!(view.epoch, 1, "preparing a commit does not advance the epoch");
        let pending = view.pending.as_ref().expect("pending control commit");
        assert_eq!(pending.operation, "self_update");
        assert_eq!(pending.base_epoch, 1);
        assert_eq!(pending.target_epoch, 2);
        view
    }

    fn submit(&self) -> ControlReceiptFfi {
        self.ffi
            .group_submit_control(
                self.group_id.clone(),
                self.node.endpoint_url(),
                NODE_ID.to_owned(),
                Some(self.node.public_key_hex()),
            )
            .expect("the node accepts the control commit")
    }
}

#[test]
fn submit_and_accept_with_a_real_node_receipt() {
    let harness = Harness::new();
    let prepared = harness.prepare_role_change();

    // 1) 提交到真节点：拿到真回执。
    let receipt = harness.submit();
    assert!(receipt.receipt.seq >= 1, "node assigns a log sequence from 1");
    assert_eq!(receipt.receipt.group_id, harness.group_id);
    assert!(receipt.receipt.submission_id.starts_with("sub:1:"));
    assert_eq!(receipt.candidate.kind, "control");
    assert_eq!(receipt.candidate.operation.as_deref(), Some("set_role"));
    assert_eq!(receipt.head.node_id, NODE_ID);
    assert_eq!(receipt.head.group_id, harness.group_id);
    // 候选正文非空（= 本机 pending 的 Commit 十六进制；逐字节比对在 group_node 侧完成）。
    assert!(prepared.pending.is_some());
    assert!(!receipt.candidate.encrypted_payload.is_empty());

    // 2) 用真回执接受：epoch 前进、pending 清空、角色生效、快照一致。
    let accepted = harness
        .ffi
        .group_accept_with_receipt(harness.group_id.clone(), receipt.clone())
        .expect("accept with the real receipt");
    assert_eq!(accepted.epoch, 2);
    assert!(accepted.pending.is_none());
    let bob = accepted
        .members
        .iter()
        .find(|member| member.credential == "bob")
        .expect("bob is a member");
    assert_eq!(bob.role, "admin");
    assert_eq!(harness.snapshot(), accepted);

    // 3) 再次接受同一份回执：没有 pending → 显式拒绝，状态不变。
    let error = harness
        .ffi
        .group_accept_with_receipt(harness.group_id.clone(), receipt)
        .expect_err("no pending commit any more");
    assert_eq!(code(error), "PENDING_MISMATCH");
    assert_eq!(harness.snapshot(), accepted);
}

#[test]
fn forged_receipts_are_rejected_without_advancing_the_epoch() {
    let harness = Harness::new();
    let before = harness.prepare_role_change();
    let receipt = harness.submit();

    // 1) 改日志链摘要 → 回执内部（链摘要 + 节点签名）不再自洽（节点验签先失败）。
    let mut tampered_log = receipt.clone();
    tampered_log.receipt.log_hash = flip_last_hex(&tampered_log.receipt.log_hash);
    let error = harness
        .ffi
        .group_accept_with_receipt(harness.group_id.clone(), tampered_log)
        .expect_err("tampered log hash");
    assert_eq!(code(error), "INVALID_SIGNATURE");
    assert_eq!(harness.snapshot(), before);

    // 2) 改节点签名 → 节点验签失败。
    let mut tampered_signature = receipt.clone();
    tampered_signature.receipt.signature = flip_last_hex(&tampered_signature.receipt.signature);
    let error = harness
        .ffi
        .group_accept_with_receipt(harness.group_id.clone(), tampered_signature)
        .expect_err("tampered signature");
    assert_eq!(code(error), "INVALID_SIGNATURE");
    assert_eq!(harness.snapshot(), before);

    // 3) 换密文（拿别的提交的正文）→ 与本机 pending 的 Commit 不一致。
    let mut tampered_payload = receipt.clone();
    tampered_payload.candidate.encrypted_payload =
        flip_last_hex(&tampered_payload.candidate.encrypted_payload);
    let error = harness
        .ffi
        .group_accept_with_receipt(harness.group_id.clone(), tampered_payload)
        .expect_err("tampered payload");
    assert_eq!(code(error), "INVALID_CANDIDATE");
    assert_eq!(harness.snapshot(), before);

    // 4) 真回执仍然能接受（失败不是破坏性的）。
    let accepted = harness
        .ffi
        .group_accept_with_receipt(harness.group_id.clone(), receipt)
        .expect("the real receipt still works");
    assert_eq!(accepted.epoch, 2);
    assert!(accepted.pending.is_none());
}

#[test]
fn unreachable_node_fails_explicitly_and_leaves_state() {
    let harness = Harness::new();
    let before = harness.prepare_role_change();
    let error = harness
        .ffi
        .group_submit_control(
            harness.group_id.clone(),
            UNREACHABLE_ENDPOINT.to_owned(),
            NODE_ID.to_owned(),
            None,
        )
        .expect_err("unreachable node");
    assert_eq!(code(error), "NODE_UNAVAILABLE");
    // 本地状态完全不变：epoch 仍 1、pending 仍在。
    let after = harness.snapshot();
    assert_eq!(after, before);
    assert!(after.pending.is_some());
    // 换成真节点仍可提交 + 接受（失败不是破坏性的）。
    let receipt = harness.submit();
    let accepted = harness
        .ffi
        .group_accept_with_receipt(harness.group_id.clone(), receipt)
        .expect("real node works after the failure");
    assert_eq!(accepted.epoch, 2);
    let _ = harness.dir.database();
}

fn flip_last_hex(value: &str) -> String {
    let mut chars: Vec<char> = value.chars().collect();
    let last = chars.pop().expect("non-empty hex");
    let flipped = if last == '0' { '1' } else { '0' };
    chars.push(flipped);
    chars.into_iter().collect()
}
