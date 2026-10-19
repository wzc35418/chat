//! SYNC-002 补全：拉到的条目真正 apply 进 MLS 并落库（真节点 + 真双设备两库）。
//!
//! 覆盖：对端应用消息被拉取后解密并写成正式消息行（可读、幂等）；重复拉取不重复写；
//! 本机不在该群的页只前移游标并计 skipped；控制提交应用后 epoch 前进。

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, GroupView, create_group_with_welcome,
    group_join_from_welcome, group_snapshot,
};
use chat_core::sync_worker::{initialize_pull_state, run_pull_once_reporting_at};
use chat_crypto::control::{ControlOperation, WelcomeAcceptance};
use chat_crypto::engine::MlsDevice;
use chat_node_api::build_router;
use chat_node_api::state::{AppState, FixedClock as NodeFixedClock};
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_node_services::canonical::{decode_hex_bytes, hex_encode};
use chat_node_services::group::{
    CandidateControlMetadata, GroupCandidate, GroupCreation, GroupMemberSpec, KIND_APPLICATION,
    KIND_CONTROL,
};
use chat_node_services::registration::{
    DeviceCredential, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest, RegistrationCapability,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use chat_protocol::{ControlHead, DeviceId, GroupId, SubmissionId, VerifiedHead};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store};
use ed25519_dalek::{Signature, Signer, SigningKey};

const NOW_S: u64 = 1_700_000_000;
const NOW_MS: i64 = 1_700_000_000_000;
const ACCOUNT: &str = "acct-core-syncapply";
const NODE_ID: &str = "node-core-syncapply";
const MASTER_KEY: [u8; 32] = [0x5a; 32];
const BOB_SEED: u8 = 41;
const ALICE_SEED: u8 = 42;
const SUPERVISOR_SEED: [u8; 32] = [0x11; 32];

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
            "chat-core-sync-apply-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> PathBuf {
        self.0.join("chat.db")
    }

    fn open(&self) -> Store {
        open_store(
            StoreOptions::new(self.database(), KeyProvenance::Raw32),
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

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(future)
}

fn device_id_text(seed: u8) -> String {
    DeviceId::from_bytes([seed; 32]).encode()
}

fn device_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn submission_text(seed: u8) -> String {
    SubmissionId::from_bytes([seed; 32]).encode()
}

fn decode_hex32(text: &str) -> [u8; 32] {
    let bytes = decode_hex_bytes("digest", text, 64).expect("hex digest");
    <[u8; 32]>::try_from(bytes.as_slice()).expect("32 bytes")
}

// ---------------------------------------------------------------- 真实节点

struct TestNode {
    address: String,
    node_public_key: [u8; 32],
}

impl TestNode {
    fn endpoint_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn client(&self, device_id: &str, seed: u8) -> NodeClient {
        let endpoint = NodeEndpoint::new(&self.endpoint_url(), NODE_ID)
            .expect("endpoint")
            .with_node_public_key(self.node_public_key);
        NodeClient::new(endpoint, device_id, device_key(seed)).expect("node client")
    }

    fn registration_request(&self, device_id: &str, seed: u8) -> RegisterDeviceRequest {
        let supervisor = SigningKey::from_bytes(&SUPERVISOR_SEED);
        let supervisor_public_key = hex_encode(&supervisor.verifying_key().to_bytes());
        let mut credential = DeviceCredential {
            account_id: ACCOUNT.into(),
            device_id: device_id.to_owned(),
            device_public_key: hex_encode(&device_key(seed).verifying_key().to_bytes()),
            endpoint_id: "endpoint-syncapply".into(),
            credential_version: 1,
            not_before: NOW_S - 10,
            expires_at: NOW_S + 3_600,
            supervisor_public_key: supervisor_public_key.clone(),
            signature: String::new(),
        };
        let signature: Signature = supervisor.sign(&credential.signing_bytes().expect("credential"));
        credential.signature = hex_encode(&signature.to_bytes());
        let mut capability = RegistrationCapability {
            capability_id: format!("cap-syncapply-{seed}"),
            account_id: ACCOUNT.into(),
            device_id: device_id.to_owned(),
            purpose: REGISTRATION_CAPABILITY_PURPOSE.into(),
            issued_at: NOW_S - 10,
            expires_at: NOW_S + 600,
            revocation_version: 1,
            supervisor_public_key,
            signature: String::new(),
        };
        let signature = supervisor.sign(&capability.signing_bytes().expect("capability"));
        capability.signature = hex_encode(&signature.to_bytes());
        RegisterDeviceRequest {
            account_id: ACCOUNT.into(),
            credential,
            capability,
        }
    }
}

/// 起节点并按给定群状态登记两台中继设备（bob = owner，alice = member）。
fn start_node(group_id: &str, control_head: &str, mls_epoch: u64) -> TestNode {
    let service = NodeService::open_in_memory(NodeServiceConfig {
        node_id: NODE_ID.into(),
        signing_seed: [7_u8; 32],
        policy: NodeAuthPolicy::default(),
    })
    .expect("node service");
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
    let node = TestNode {
        address,
        node_public_key,
    };
    for seed in [BOB_SEED, ALICE_SEED] {
        let device_id = device_id_text(seed);
        block_on(
            node.client(&device_id, seed)
                .register_device(&node.registration_request(&device_id, seed)),
        )
        .expect("device registers");
    }
    let creation = GroupCreation {
        group_id: group_id.to_owned(),
        owner_account_id: ACCOUNT.into(),
        initial_control_head: control_head.to_owned(),
        policy_version: 1,
        mls_epoch,
        members: vec![
            GroupMemberSpec {
                device_id: device_id_text(BOB_SEED),
                role: "owner".into(),
            },
            GroupMemberSpec {
                device_id: device_id_text(ALICE_SEED),
                role: "member".into(),
            },
        ],
    };
    block_on(node.client(&device_id_text(BOB_SEED), BOB_SEED).create_group(&creation))
        .expect("group registers on the node");
    node
}

// ---------------------------------------------------------------- 双设备双库夹具

struct Fixture {
    alice_dir: FixtureDir,
    store_b: Store,
    node: TestNode,
    group_id: String,
    view_b: GroupView,
}

impl Fixture {
    /// bob（本机、store_b 的成员）建群，alice（另一台设备）用 Welcome 加入；
    /// MLS 凭据直接使用设备 ID 文本，因此节点日志里的 author_device_id 就是 MLS 认证作者。
    fn new() -> Self {
        let bob_dir = FixtureDir::new();
        let alice_dir = FixtureDir::new();
        let mut store_b = bob_dir.open();
        let mut store_a = alice_dir.open();
        let mut bob = MlsDevice::new(&device_id_text(BOB_SEED)).expect("bob device");
        let mut alice = MlsDevice::new(&device_id_text(ALICE_SEED)).expect("alice device");
        let request = CreateGroupRequest::new("同步应用群", &device_id_text(BOB_SEED));
        let peer = GroupBootstrapPeer {
            credential: device_id_text(ALICE_SEED),
            device: &alice,
        };
        let (view_b, welcome) =
            create_group_with_welcome(&mut bob, &peer, &mut store_b, &request).expect("create");
        let acceptance = WelcomeAcceptance {
            group_id: view_b.mls_group_key.clone(),
            expected_epoch: view_b.epoch,
            members: vec![device_id_text(BOB_SEED), device_id_text(ALICE_SEED)],
        };
        let _view_a = group_join_from_welcome(
            &mut alice,
            &mut store_a,
            "同步应用群",
            &welcome,
            &acceptance,
            &device_id_text(ALICE_SEED),
            &device_id_text(BOB_SEED),
        )
        .expect("alice joins");
        let node = start_node(&view_b.group_id, &view_b.control_head, view_b.epoch);
        // bob 的拉取游标从建群回执的已验证 head 起步（序号 0）。
        let genesis = VerifiedHead::genesis(
            GroupId::parse(&view_b.group_id).expect("group id"),
            ControlHead::from_bytes(decode_hex32(&view_b.control_head)),
            1,
            view_b.epoch,
        );
        initialize_pull_state(&mut store_b, &view_b.group_id, genesis).expect("pull state");
        Self {
            alice_dir,
            store_b,
            node,
            group_id: view_b.group_id.clone(),
            view_b,
        }
    }

    fn node_head(&self) -> chat_node_services::group::SignedLogHead {
        block_on(self.node.client(&device_id_text(BOB_SEED), BOB_SEED).get_head(&self.group_id))
            .expect("node head")
    }

    fn alice_device(&self) -> MlsDevice {
        let store = self.alice_dir.open();
        MlsDevice::import_from_store(&store, &self.view_b.mls_group_key)
            .expect("alice device from her own store")
    }

    /// alice 用真实 MLS 加密一条应用消息并提交到节点日志。
    fn append_application(&mut self, seed: u8, text: &str) {
        let mut alice = self.alice_device();
        let ciphertext = alice.encrypt(text).expect("encrypt");
        let head = self.node_head();
        let candidate = GroupCandidate {
            protocol_major: 1,
            protocol_minor: 0,
            submission_id: submission_text(seed),
            group_id: self.group_id.clone(),
            author_device_id: device_id_text(ALICE_SEED),
            kind: KIND_APPLICATION.to_owned(),
            expected_control_head: head.control_head.clone(),
            policy_version: head.policy_version,
            mls_epoch: head.mls_epoch,
            encrypted_payload: hex_encode(&ciphertext),
            control_metadata: None,
        };
        block_on(self.node.client(&device_id_text(ALICE_SEED), ALICE_SEED).append_entry(&candidate))
            .expect("application entry accepted");
    }

    /// alice 提交一个真实 SelfUpdate 控制提交（对端应用后 epoch +1）。
    fn append_self_update_commit(&mut self, seed: u8) {
        let mut alice = self.alice_device();
        let mut store_a = self.alice_dir.open();
        let pending = alice
            .prepare_control(ControlOperation::SelfUpdate, &mut store_a)
            .expect("prepare self update");
        let commit = pending.commit.clone();
        let head = self.node_head();
        let candidate = GroupCandidate {
            protocol_major: 1,
            protocol_minor: 0,
            submission_id: submission_text(seed),
            group_id: self.group_id.clone(),
            author_device_id: device_id_text(ALICE_SEED),
            kind: KIND_CONTROL.to_owned(),
            expected_control_head: head.control_head.clone(),
            policy_version: head.policy_version,
            mls_epoch: head.mls_epoch,
            encrypted_payload: hex_encode(&commit),
            control_metadata: Some(CandidateControlMetadata {
                operation: "set_role".to_owned(),
                target_digest: hex_encode(&[0x33_u8; 32]),
            }),
        };
        block_on(self.node.client(&device_id_text(ALICE_SEED), ALICE_SEED).append_entry(&candidate))
            .expect("control entry accepted");
    }
}

#[test]
fn pull_applies_the_application_message_into_mls_and_the_store() {
    let mut fixture = Fixture::new();
    fixture.append_application(61, "来自 alice 的第一条消息");

    let (outcome, report) = run_pull_once_reporting_at(
        &mut fixture.store_b,
        &fixture.node.client(&device_id_text(BOB_SEED), BOB_SEED),
        &fixture.group_id,
        NOW_MS,
    )
    .expect("pull applies the entry");
    assert_eq!(report.applied, 1, "one application entry applied");
    assert_eq!(report.stale, 0);
    assert_eq!(report.unsupported, 0);
    assert!(!report.needs_catchup);
    assert_eq!(outcome.applied, 1);
    assert_eq!(outcome.cursor_after, 1);
    assert!(!outcome.resync_required);

    // 消息真的落库了：存在性 + 会话摘要里的预览就是解密后的正文。
    let message_id = chat_core::sync_worker::applied_message_id(&fixture.group_id, 1);
    assert!(
        fixture.store_b.message_exists(&message_id).expect("message exists"),
        "the applied message must be persisted"
    );
    let summary = fixture
        .store_b
        .session_summary(&fixture.group_id)
        .expect("session summary")
        .expect("session row exists");
    assert_eq!(summary.preview, "来自 alice 的第一条消息");
}

#[test]
fn repeated_pulls_do_not_apply_the_same_entry_twice() {
    let mut fixture = Fixture::new();
    fixture.append_application(62, "只应用一次");
    let client = fixture.node.client(&device_id_text(BOB_SEED), BOB_SEED);
    let first = run_pull_once_reporting_at(&mut fixture.store_b, &client, &fixture.group_id, NOW_MS)
        .expect("first pull");
    assert_eq!(first.1.applied, 1);
    // 同一页再来一次：游标已到 head → 不重复 apply、不重复写。
    let second = run_pull_once_reporting_at(
        &mut fixture.store_b,
        &client,
        &fixture.group_id,
        NOW_MS + 2_000,
    )
    .expect("second pull");
    assert_eq!(second.1.applied, 0, "nothing new to apply");
    assert_eq!(second.0.applied, 0);
    assert_eq!(second.0.cursor_after, 1, "cursor is unchanged");
    let message_id = chat_core::sync_worker::applied_message_id(&fixture.group_id, 1);
    assert!(fixture.store_b.message_exists(&message_id).expect("exists"));
}

#[test]
fn a_page_for_a_group_the_local_store_is_not_in_is_skipped_but_advances() {
    // 只起节点、不建本地群：整页按 UNSUPPORTED 跳过，游标照常前进（与旧的“只落库”行为一致）。
    let dir = FixtureDir::new();
    let mut store = dir.open();
    let group_id = GroupId::from_bytes([77_u8; 32]).encode();
    let node = start_node(&group_id, &hex_encode(&[0x11_u8; 32]), 0);
    let genesis = VerifiedHead::genesis(
        GroupId::parse(&group_id).expect("group id"),
        ControlHead::from_bytes([0x11_u8; 32]),
        1,
        0,
    );
    initialize_pull_state(&mut store, &group_id, genesis).expect("pull state");
    // 追加两条“不是 MLS 消息”的应用条目（本机也不在该群）。
    for seed in [71_u8, 72] {
        let head = block_on(
            node.client(&device_id_text(BOB_SEED), BOB_SEED).get_head(&group_id),
        )
        .expect("head");
        let candidate = GroupCandidate {
            protocol_major: 1,
            protocol_minor: 0,
            submission_id: submission_text(seed),
            group_id: group_id.clone(),
            author_device_id: device_id_text(BOB_SEED),
            kind: KIND_APPLICATION.to_owned(),
            expected_control_head: head.control_head.clone(),
            policy_version: head.policy_version,
            mls_epoch: head.mls_epoch,
            encrypted_payload: hex_encode(b"not an mls message"),
            control_metadata: None,
        };
        block_on(node.client(&device_id_text(BOB_SEED), BOB_SEED).append_entry(&candidate))
            .expect("entry accepted");
    }
    let client = node.client(&device_id_text(BOB_SEED), BOB_SEED);
    let (outcome, report) =
        run_pull_once_reporting_at(&mut store, &client, &group_id, NOW_MS).expect("pull");
    assert_eq!(report.applied, 0);
    assert_eq!(report.unsupported, 2, "two entries skipped as unsupported");
    assert_eq!(report.stale, 0);
    assert!(!report.needs_catchup);
    assert_eq!(outcome.cursor_after, 2, "cursor still advances");
}

#[test]
fn applying_a_control_commit_advances_the_epoch() {
    let mut fixture = Fixture::new();
    let before = fixture.view_b.epoch;
    fixture.append_self_update_commit(81);
    let client = fixture.node.client(&device_id_text(BOB_SEED), BOB_SEED);
    let (outcome, report) =
        run_pull_once_reporting_at(&mut fixture.store_b, &client, &fixture.group_id, NOW_MS)
            .expect("pull applies the commit");
    assert_eq!(report.applied, 1, "the control commit is applied");
    assert_eq!(outcome.cursor_after, 1);
    let snapshot = group_snapshot(&fixture.store_b, &fixture.group_id).expect("snapshot");
    assert_eq!(
        snapshot.epoch,
        before + 1,
        "the applied commit advances the snapshot epoch by exactly one"
    );
}
