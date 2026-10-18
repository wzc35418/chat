//! GN 包端到端：真实节点（axum + node-services）+ 真实 OpenMLS + 真实节点回执。
//!
//! 覆盖：控制提交提交到节点拿到真回执 → 回执绑定校验通过后 epoch 前进且节点日志有该条目；
//! 重复提交幂等（不产生第二条日志）；伪造/错绑定回执被拒绝且 epoch 不变；
//! 节点不可达显式 NODE_UNAVAILABLE 且本地状态不变；没有 pending 时提交显式失败。
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::group_node::{
    ControlReceipt, accept_pending_control_with_receipt, submit_pending_control,
};
use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, GroupView, create_group, group_snapshot, set_role,
};
use chat_crypto::engine::MlsDevice;
use chat_domain::MemberRole;
use chat_node_api::build_router;
use chat_node_api::state::{AppState, FixedClock as NodeFixedClock};
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_node_services::canonical::hex_encode;
use chat_node_services::group::{GroupCreation, GroupMemberSpec, LogEntry};
use chat_node_services::registration::{
    DeviceCredential, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest, RegistrationCapability,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use chat_protocol::{DeviceId, GroupId};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store};
use ed25519_dalek::{Signature, Signer, SigningKey};

const NOW_S: u64 = 1_700_000_000;
const ACCOUNT: &str = "acct-core-gn";
const NODE_ID: &str = "node-core-gn";
const MASTER_KEY: [u8; 32] = [0x5a; 32];
const DEVICE_SEED: u8 = 21;
const INITIAL_CONTROL_HEAD: [u8; 32] = [0x11; 32];

fn device_id_text(seed: u8) -> String {
    DeviceId::from_bytes([seed; 32]).encode()
}

fn device_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

// ---------------------------------------------------------------- 真实节点夹具

struct TestNode {
    address: String,
    node_public_key: [u8; 32],
    supervisor: SigningKey,
}

impl TestNode {
    fn endpoint(&self) -> NodeEndpoint {
        NodeEndpoint::new(&format!("http://{}", self.address), NODE_ID)
            .expect("endpoint")
            .with_node_public_key(self.node_public_key)
    }

    fn client(&self, device_id: &str, key: &SigningKey) -> NodeClient {
        NodeClient::new(self.endpoint(), device_id, key.clone()).expect("node client")
    }

    fn registration_request(
        &self,
        device_id: &str,
        key: &SigningKey,
        capability_id: &str,
    ) -> RegisterDeviceRequest {
        let supervisor_public_key = hex_encode(&self.supervisor.verifying_key().to_bytes());
        let mut credential = DeviceCredential {
            account_id: ACCOUNT.into(),
            device_id: device_id.to_owned(),
            device_public_key: hex_encode(&key.verifying_key().to_bytes()),
            endpoint_id: "endpoint-gn".into(),
            credential_version: 1,
            not_before: NOW_S - 10,
            expires_at: NOW_S + 3_600,
            supervisor_public_key: supervisor_public_key.clone(),
            signature: String::new(),
        };
        let signature: Signature = self
            .supervisor
            .sign(&credential.signing_bytes().expect("credential bytes"));
        credential.signature = hex_encode(&signature.to_bytes());
        let mut capability = RegistrationCapability {
            capability_id: capability_id.to_owned(),
            account_id: ACCOUNT.into(),
            device_id: device_id.to_owned(),
            purpose: REGISTRATION_CAPABILITY_PURPOSE.into(),
            issued_at: NOW_S - 10,
            expires_at: NOW_S + 600,
            revocation_version: 1,
            supervisor_public_key,
            signature: String::new(),
        };
        let signature: Signature = self
            .supervisor
            .sign(&capability.signing_bytes().expect("capability bytes"));
        capability.signature = hex_encode(&signature.to_bytes());
        RegisterDeviceRequest {
            account_id: ACCOUNT.into(),
            credential,
            capability,
        }
    }
}

/// 在独立线程 + 独立运行时上起真实 node-api（测试线程随后会同步阻塞，不能共用运行时）。
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
        supervisor: SigningKey::from_bytes(&[11_u8; 32]),
    }
}

// ---------------------------------------------------------------- 存储夹具

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
            "chat-core-group-node-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn open(&self) -> Store {
        open_store(
            StoreOptions::new(self.0.join("chat.db"), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(MASTER_KEY),
        )
        .expect("open formal store")
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// 让异步客户端调用能在同步测试里跑（测试线程自己的运行时；节点在别的线程上）。
fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(future)
}

// ---------------------------------------------------------------- 群夹具

struct Harness {
    dir: FixtureDir,
    store: Store,
    alice: MlsDevice,
    bob: MlsDevice,
    node: TestNode,
    client: NodeClient,
    view: GroupView,
}

impl Harness {
    /// 建真实 MLS 双人群（alice=owner, bob=member，建群后无 pending），并在节点登记该群。
    fn new() -> Self {
        let dir = FixtureDir::new();
        let mut store = dir.open();
        let mut alice = MlsDevice::new("alice").expect("alice");
        let bob = MlsDevice::new("bob").expect("bob");
        let request = CreateGroupRequest::new("节点回执测试群", "alice");
        let peer = GroupBootstrapPeer {
            credential: "bob".to_owned(),
            device: &bob,
        };
        let view = create_group(&mut alice, &peer, &mut store, &request).expect("create group");
        assert_eq!(view.epoch, 1);
        assert!(view.pending.is_none(), "bootstrap group has no pending commit");

        let node = start_node();
        let device_id = device_id_text(DEVICE_SEED);
        let key = device_key(DEVICE_SEED);
        let client = node.client(&device_id, &key);
        block_on(client.register_device(&node.registration_request(&device_id, &key, "cap-gn")))
            .expect("device registers");

        // 节点侧登记同一个群（成员用节点设备标识；与 MLS 凭据是两套命名空间，见 groups.md）。
        let creation = GroupCreation {
            group_id: view.group_id.clone(),
            owner_account_id: ACCOUNT.into(),
            initial_control_head: hex_encode(&INITIAL_CONTROL_HEAD),
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
            store,
            alice,
            bob,
            node,
            client,
            view,
        }
    }

    fn group_id(&self) -> String {
        self.view.group_id.clone()
    }

    fn snapshot(&self) -> GroupView {
        group_snapshot(&self.store, &self.group_id()).expect("snapshot")
    }

    /// 让群主改 bob 的角色 → 产生真实 MLS 控制提交（pending，epoch 不前进）。
    fn prepare_role_change(&mut self) -> GroupView {
        let group_id = self.group_id();
        let view = set_role(
            &mut self.alice,
            &mut self.store,
            &group_id,
            "alice",
            "bob",
            MemberRole::Admin,
        )
        .expect("owner prepares the role change");
        assert_eq!(view.epoch, 1, "preparing a commit does not advance the epoch");
        let pending = view.pending.as_ref().expect("pending control commit");
        assert_eq!(pending.operation, "self_update");
        assert_eq!(pending.base_epoch, 1);
        assert_eq!(pending.target_epoch, 2);
        view
    }

    /// 节点上该群的完整日志（含候选原文与回执）。
    fn node_log(&self) -> Vec<LogEntry> {
        let page = block_on(self.client.fetch_log(&self.group_id(), 0, 50)).expect("node log");
        page.entries
    }

    fn node_head(&self) -> chat_node_services::group::SignedLogHead {
        block_on(self.client.get_head(&self.group_id())).expect("node head")
    }
}

// ---------------------------------------------------------------- 用例

#[test]
fn control_commit_is_submitted_then_accepted_with_a_real_node_receipt() {
    let mut harness = Harness::new();
    let before = harness.prepare_role_change();
    let pending_commit = harness
        .alice
        .pending_control()
        .expect("pending query")
        .expect("pending commit")
        .commit;

    // 1) 提交到节点：拿到真实回执。
    let receipt = submit_pending_control(
        &harness.alice,
        &harness.store,
        &harness.group_id(),
        &harness.client,
    )
    .expect("the node accepts the control commit");
    assert_eq!(receipt.seq(), 1, "first node log entry");
    assert_eq!(receipt.receipt.group_id, harness.group_id());
    assert_eq!(receipt.candidate.kind, "control");
    assert_eq!(receipt.candidate.encrypted_payload, hex_encode(&pending_commit));
    assert_eq!(receipt.commit_bytes().expect("commit bytes"), pending_commit);
    assert_eq!(receipt.head.node_id, NODE_ID);

    // 节点日志确实有这条控制条目（候选原文 + 回执绑定）。
    let log = harness.node_log();
    assert_eq!(log.len(), 1, "exactly one control entry on the node");
    assert_eq!(log[0].candidate.candidate.kind, "control");
    assert_eq!(
        log[0].candidate.candidate.submission_id,
        receipt.submission_id()
    );
    assert_eq!(log[0].receipt.seq, receipt.seq());
    assert_eq!(log[0].receipt.log_hash, receipt.log_hash());
    let head = harness.node_head();
    assert_eq!(head.seq, receipt.seq());
    assert_eq!(head.log_hash, receipt.log_hash());

    // 2) 用回执接受：epoch 前进到 target_epoch，pending 清空，角色生效。
    let group_id = harness.group_id();
    let accepted = accept_pending_control_with_receipt(
        &mut harness.alice,
        &mut harness.store,
        &group_id,
        &receipt,
    )
    .expect("the bound receipt is accepted");
    assert_eq!(accepted.epoch, before.pending.as_ref().expect("pending").target_epoch);
    assert_eq!(accepted.epoch, 2);
    assert!(accepted.pending.is_none(), "pending is consumed by the merge");
    let bob = accepted
        .members
        .iter()
        .find(|member| member.credential == "bob")
        .expect("bob is a member");
    assert_eq!(bob.role, MemberRole::Admin);
    // 持久快照同步前进（重开库也不回退）。
    assert_eq!(harness.snapshot().epoch, 2);
    assert!(harness.snapshot().pending.is_none());
    // 节点日志没有被第二次写入。
    assert_eq!(harness.node_log().len(), 1);
}

#[test]
fn duplicate_submit_is_idempotent_and_appends_once() {
    let mut harness = Harness::new();
    harness.prepare_role_change();
    let group_id = harness.group_id();
    let first = submit_pending_control(
        &harness.alice,
        &harness.store,
        &group_id,
        &harness.client,
    )
    .expect("first submit");
    let second = submit_pending_control(
        &harness.alice,
        &harness.store,
        &group_id,
        &harness.client,
    )
    .expect("retry is idempotent");
    assert_eq!(first.submission_id(), second.submission_id());
    assert_eq!(first.seq(), second.seq());
    assert_eq!(first.log_hash(), second.log_hash());
    assert_eq!(first.receipt.candidate_hash, second.receipt.candidate_hash);
    assert_eq!(harness.node_log().len(), 1, "no second log entry");
    // 两份回执都能用于接受（内容相同）。
    let accepted = accept_pending_control_with_receipt(
        &mut harness.alice,
        &mut harness.store,
        &group_id,
        &second,
    )
    .expect("accepted with the retried receipt");
    assert_eq!(accepted.epoch, 2);
}

#[test]
fn misbound_or_forged_receipts_are_rejected_without_advancing_epoch() {
    let mut harness = Harness::new();
    harness.prepare_role_change();
    let group_id = harness.group_id();
    let genuine = submit_pending_control(
        &harness.alice,
        &harness.store,
        &group_id,
        &harness.client,
    )
    .expect("submit");

    let mut forged_seq = genuine.clone();
    forged_seq.receipt.seq += 1;
    let mut forged_log_hash = genuine.clone();
    forged_log_hash.receipt.log_hash = "00".repeat(32);
    let mut forged_payload = genuine.clone();
    forged_payload.candidate.encrypted_payload = hex_encode(b"another commit");
    let mut forged_group = genuine.clone();
    forged_group.candidate.group_id = "grp:1:".to_owned() + &"33".repeat(32);
    let mut forged_author = genuine.clone();
    forged_author.receipt.author_device_id = device_id_text(99);
    let mut forged_node = genuine.clone();
    forged_node.receipt.node_public_key = "22".repeat(32);
    let mut forged_kind = genuine.clone();
    forged_kind.candidate.kind = "application".to_owned();

    let cases: [(&str, ControlReceipt, &str); 7] = [
        // seq 被节点回执签名覆盖，篡改首先表现为签名失效（判定顺序：签名 → 链摘要）。",
        ("seq", forged_seq, "INVALID_SIGNATURE"),
        // 节点回执的 verify 同时复核日志链摘要，因此改 log_hash 也是签名/链校验失败。
        ("log_hash", forged_log_hash, "INVALID_SIGNATURE"),
        ("payload", forged_payload, "INVALID_CANDIDATE"),
        ("group", forged_group, "INVALID_RECEIPT_SCOPE"),
        ("author", forged_author, "PERMISSION_DENIED"),
        ("node", forged_node, "INVALID_RECEIPT_SCOPE"),
        ("kind", forged_kind, "INVALID_CANDIDATE"),
    ];
    for (name, forged, expected) in cases {
        let error = accept_pending_control_with_receipt(
            &mut harness.alice,
            &mut harness.store,
            &group_id,
            &forged,
        )
        .expect_err("forged receipt must be rejected");
        assert_eq!(error.code(), expected, "case {name}");
        // 每次拒绝后本地状态完全不动。
        let view = harness.snapshot();
        assert_eq!(view.epoch, 1, "epoch must not advance (case {name})");
        assert!(view.pending.is_some(), "pending must stay (case {name})");
    }
    // 只有真正绑定的回执能推进。
    let accepted = accept_pending_control_with_receipt(
        &mut harness.alice,
        &mut harness.store,
        &group_id,
        &genuine,
    )
    .expect("the genuine receipt is accepted after the rejections");
    assert_eq!(accepted.epoch, 2);
}

#[test]
fn unreachable_node_fails_explicitly_and_leaves_local_state_unchanged() {
    let mut harness = Harness::new();
    harness.prepare_role_change();
    let group_id = harness.group_id();
    // 同设备身份、但指向一个必然连不上的本地端口。
    let unreachable = NodeClient::new(
        NodeEndpoint::new("http://127.0.0.1:1", NODE_ID)
            .expect("endpoint")
            .with_node_public_key(harness.node.node_public_key),
        device_id_text(DEVICE_SEED),
        device_key(DEVICE_SEED),
    )
    .expect("client");
    let error = submit_pending_control(&harness.alice, &harness.store, &group_id, &unreachable)
        .expect_err("an unreachable node cannot accept the submission");
    assert_eq!(error.code(), "NODE_UNAVAILABLE");
    // 本地状态不变：epoch 与 pending 都没动，而且真节点仍然可用。
    let view = harness.snapshot();
    assert_eq!(view.epoch, 1);
    assert!(view.pending.is_some());
    assert!(harness.node_log().is_empty(), "nothing was appended");
    let receipt = submit_pending_control(
        &harness.alice,
        &harness.store,
        &group_id,
        &harness.client,
    )
    .expect("the real node still works");
    let accepted = accept_pending_control_with_receipt(
        &mut harness.alice,
        &mut harness.store,
        &group_id,
        &receipt,
    )
    .expect("accepted afterwards");
    assert_eq!(accepted.epoch, 2);
}

#[test]
fn submit_without_pending_control_fails_explicitly() {
    let harness = Harness::new();
    let error = submit_pending_control(
        &harness.alice,
        &harness.store,
        &harness.group_id(),
        &harness.client,
    )
    .expect_err("no pending commit means nothing to submit");
    assert_eq!(error.code(), "PENDING_MISMATCH");
    assert!(harness.node_log().is_empty());
    // 非法群标识在触网之前就被拒绝。
    let error = submit_pending_control(&harness.alice, &harness.store, "not-a-group", &harness.client)
        .expect_err("non canonical group id");
    assert_eq!(error.code(), "INVALID_ID");
    let _ = &harness.bob;
    let _ = &harness.dir;
    let _ = GroupId::parse(&harness.group_id()).expect("canonical group id");
}
