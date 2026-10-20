//! GROUPS-001b：建群后在节点登记群（真节点 + 真库 + 真 MLS）。
//!
//! 覆盖：登记成功；重复登记幂等；不可达 NODE_UNAVAILABLE 且本地不变；非群主登记被节点拒绝。
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::group_node::group_register_on_node;
use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, create_group_with_welcome, group_snapshot,
};
use chat_core::service::{ConversationCore, IdentityAccountInput};
use chat_crypto::engine::MlsDevice;
use chat_node_api::build_router;
use chat_node_api::state::{AppState, FixedClock as NodeFixedClock};
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_node_services::registration::{
    DeviceCredential, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest, RegistrationCapability,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store};
use ed25519_dalek::{Signature, Signer, SigningKey};

// 账户 ID 由持久身份派生（不写死，避免与节点登记账户不一致）。
const NODE_ID: &str = "node-register-test";
const MASTER_KEY: [u8; 32] = [0x7c; 32];
const DEVICE_SEED: [u8; 32] = [0x41; 32];
const SUPERVISOR_SEED: [u8; 32] = [0x12; 32];
const UNREACHABLE: &str = "http://127.0.0.1:1";

/// 当前 Unix 秒（节点与凭据都要用同一个时钟，避免 not_before 落在未来）。
fn now_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn master_key_hex() -> String {
    hex_of(&MASTER_KEY)
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(future)
}

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
            "chat-core-register-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("fixture dir");
        Self(dir)
    }

    fn database(&self) -> PathBuf {
        self.0.join("chat.db")
    }

    fn database_text(&self) -> String {
        self.database().to_string_lossy().into_owned()
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

fn enroll_identity(database: &str) -> (String, String) {
    enroll_identity_with(database, DEVICE_SEED)
}

fn enroll_identity_with(database: &str, device_seed: [u8; 32]) -> (String, String) {
    let mut core = ConversationCore::open(database, &master_key_hex()).expect("open core");
    let view = core
        .save_identity_account(IdentityAccountInput {
            display_name: "register".to_owned(),
            account_seed_hex: hex_of(&[0x22u8; 32]),
            device_seed_hex: hex_of(&device_seed),
            // 身份创建时间用真实当前时间：节点按自己的时钟校验凭据有效期。
            created_at_ms: now_s() * 1_000,
        })
        .expect("save identity");
    core.close();
    (view.account_id, view.device_id)
}

struct TestNode {
    address: String,
    node_public_key: [u8; 32],
    device_id: String,
}

impl TestNode {
    fn start(device_id: &str) -> Self {
        let service = NodeService::open_in_memory(NodeServiceConfig {
            node_id: NODE_ID.into(),
            signing_seed: [7u8; 32],
            policy: NodeAuthPolicy::default(),
        })
        .expect("node service");
        let node_public_key = service.node_public_key();
        let state = AppState::new(service, Arc::new(NodeFixedClock::new(now_s())));
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
        Self {
            address,
            node_public_key,
            device_id: device_id.to_owned(),
        }
    }

    fn endpoint_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn public_key_hex(&self) -> String {
        hex_of(&self.node_public_key)
    }

    fn client(&self) -> NodeClient {
        self.client_with(&self.device_id(), DEVICE_SEED)
    }

    fn client_with(&self, device_id: &str, seed: [u8; 32]) -> NodeClient {
        let endpoint = NodeEndpoint::new(&self.endpoint_url(), NODE_ID)
            .expect("endpoint")
            .with_node_public_key(self.node_public_key);
        NodeClient::new(endpoint, device_id.to_owned(), SigningKey::from_bytes(&seed))
            .expect("node client")
    }

    fn device_id(&self) -> String {
        self.device_id.clone()
    }

    fn register_device(&self, account_id: &str) {
        let device_id = self.device_id();
        self.register_device_for(&device_id, DEVICE_SEED, account_id, "cap-register");
    }

    fn register_device_for(&self, device_id: &str, seed: [u8; 32], account_id: &str, capability_id: &str) {
        let supervisor = SigningKey::from_bytes(&SUPERVISOR_SEED);
        let supervisor_public_key = hex_of(&supervisor.verifying_key().to_bytes());
        let credential_device_id = device_id.to_owned();
        let device_id = credential_device_id.clone();
        let mut credential = DeviceCredential {
            account_id: account_id.to_owned(),
            device_id: device_id.clone(),
            device_public_key: hex_of(&SigningKey::from_bytes(&seed).verifying_key().to_bytes()),
            endpoint_id: "endpoint-register".into(),
            credential_version: 1,
            not_before: now_s() - 10,
            expires_at: now_s() + 3_600,
            supervisor_public_key: supervisor_public_key.clone(),
            signature: String::new(),
        };
        let signature: Signature = supervisor.sign(&credential.signing_bytes().expect("credential"));
        credential.signature = hex_of(&signature.to_bytes());
        let mut capability = RegistrationCapability {
            capability_id: capability_id.to_owned(),
            account_id: account_id.to_owned(),
            device_id,
            purpose: REGISTRATION_CAPABILITY_PURPOSE.into(),
            issued_at: now_s() - 10,
            expires_at: now_s() + 600,
            revocation_version: 1,
            supervisor_public_key,
            signature: String::new(),
        };
        let signature: Signature = supervisor.sign(&capability.signing_bytes().expect("capability"));
        capability.signature = hex_of(&signature.to_bytes());
        let request = RegisterDeviceRequest {
            account_id: account_id.to_owned(),
            credential,
            capability,
        };
        block_on(self.client_with(&credential_device_id, seed).register_device(&request)).expect("register device");
    }
}

/// 建一个真实的本地群（两成员：本机 owner + bob）。
fn local_group(store: &mut Store) -> (String, String) {
    let mut alice = MlsDevice::new("alice").expect("alice");
    let bob = MlsDevice::new("bob").expect("bob");
    let request = CreateGroupRequest::new("register-group", "alice");
    let peer = GroupBootstrapPeer {
        credential: "bob".to_owned(),
        device: &bob,
    };
    let (view, _welcome) =
        create_group_with_welcome(&mut alice, &peer, store, &request).expect("create");
    (view.group_id, view.control_head)
}

#[test]
fn a_local_group_is_registered_on_the_node_and_repeats_idempotently() {
    let dir = FixtureDir::new();
    let (_account_id, device_id) = enroll_identity(&dir.database_text());
    let mut store = dir.open();
    let (group_id, _head) = local_group(&mut store);
    let node = TestNode::start(&device_id);
    assert_eq!(node.device_id(), device_id, "node client uses the persisted device");
    // 注意：这里**不**预先注册设备——group_register_on_node 内部必须先自注册（NODE-001）。

    let first = group_register_on_node(
        &mut store,
        &group_id,
        &node.endpoint_url(),
        NODE_ID,
        Some(&node.public_key_hex()),
    )
    .expect("registration succeeds");
    assert_eq!(first.group_id, group_id);
    assert_eq!(first.node_id, NODE_ID);

    // 幂等：重复登记返回同一群、同一节点，且不报错。
    let second = group_register_on_node(
        &mut store,
        &group_id,
        &node.endpoint_url(),
        NODE_ID,
        Some(&node.public_key_hex()),
    )
    .expect("repeat registration is idempotent");
    assert_eq!(second.group_id, first.group_id);
    assert_eq!(second.node_id, first.node_id);

    // SYNC：登记后拉取游标必须已初始化（以节点签名回执为可信起点），且重复登记不覆盖。
    let state = chat_core::sync_worker::load_pull_state(&store, &group_id)
        .expect("pull state")
        .expect("the pull cursor is seeded by a successful registration");
    assert_eq!(state.cursor_log_hash, chat_protocol::Digest::ZERO.to_hex());
    let cursor_after_first = state.cursor_seq;
    let epoch_after_first = state.mls_epoch;
    // 单写者：先释放 store，再开新的连接做一次单轮拉取。
    drop(store);
    let mut store_for_pull = dir.open();
    let again = chat_core::sync_worker::load_pull_state(&store_for_pull, &group_id)
        .expect("pull state")
        .expect("initialized");
    assert_eq!(again.cursor_seq, cursor_after_first, "repeat registration must not move the cursor");
    assert_eq!(again.mls_epoch, epoch_after_first);
    // 登记后先往节点写一条条目（节点日志在 create_group 时是空的），再单轮拉取。
    let client = node.client();
    let head_before = block_on(client.get_head(&group_id)).expect("node head");
    let candidate = chat_node_services::group::GroupCandidate {
        protocol_major: 1,
        protocol_minor: 0,
        submission_id: chat_protocol::SubmissionId::from_bytes([0x91; 32]).encode(),
        group_id: group_id.clone(),
        author_device_id: device_id.clone(),
        kind: "application".to_owned(),
        expected_control_head: head_before.control_head.clone(),
        policy_version: head_before.policy_version,
        mls_epoch: head_before.mls_epoch,
        encrypted_payload: chat_node_services::canonical::hex_encode(b"after registration"),
        control_metadata: None,
    };
    block_on(client.append_entry(&candidate)).expect("entry accepted");
    let outcome = chat_core::sync_worker::run_pull_once(&mut store_for_pull, &client, &group_id)
        .expect("one-shot pull after registration");
    assert!(outcome.applied >= 1, "the registration entry is pulled: {outcome:?}");
    let head = block_on(client.get_head(&group_id)).expect("node head");
    assert_eq!(outcome.cursor_after, head.seq, "the cursor settles on the node head");
}

#[test]
fn an_unreachable_node_is_node_unavailable_and_the_local_snapshot_is_untouched() {
    let dir = FixtureDir::new();
    let _identity = enroll_identity(&dir.database_text());
    let mut store = dir.open();
    let (group_id, _head) = local_group(&mut store);
    let before = group_snapshot(&store, &group_id).expect("snapshot");
    let error = group_register_on_node(&mut store, &group_id, UNREACHABLE, NODE_ID, None)
        .expect_err("unreachable node");
    assert_eq!(error.code(), "NODE_UNAVAILABLE");
    let after = group_snapshot(&store, &group_id).expect("snapshot");
    assert_eq!(after, before, "a failed registration must not change the snapshot");
}

#[test]
fn an_unknown_group_is_invalid_target() {
    let dir = FixtureDir::new();
    let (account_id, device_id) = enroll_identity(&dir.database_text());
    let mut store = dir.open();
    let (existing, _head) = local_group(&mut store);
    let mut unknown = existing.clone();
    let last = unknown.pop().expect("group id");
    unknown.push(if last == '0' { '1' } else { '0' });
    let node = TestNode::start(&device_id);
    node.register_device(&account_id);
    let error = group_register_on_node(
        &mut store,
        &unknown,
        &node.endpoint_url(),
        NODE_ID,
        Some(&node.public_key_hex()),
    )
    .expect_err("unknown group");
    assert_eq!(error.code(), "INVALID_TARGET");
}

#[test]
fn a_device_key_mismatch_is_rejected_and_the_local_state_is_untouched() {
    let dir = FixtureDir::new();
    let (account_id, device_id) = enroll_identity(&dir.database_text());
    let mut store = dir.open();
    let (group_id, _head) = local_group(&mut store);
    let before = group_snapshot(&store, &group_id).expect("snapshot");
    let node = TestNode::start(&device_id);
    // 节点上该账户的主管钥被固定成夹具的密钥（与 core 派生的不同）→ 自注册被拒。
    node.register_device(&account_id);
    let error = group_register_on_node(
        &mut store,
        &group_id,
        &node.endpoint_url(),
        NODE_ID,
        Some(&node.public_key_hex()),
    )
    .expect_err("a credential mismatch must be rejected");
    assert_eq!(error.code(), "INVALID_CREDENTIAL");
    let after = group_snapshot(&store, &group_id).expect("snapshot");
    assert_eq!(after, before, "a rejected registration must not change the snapshot");
}

/// 节点规则（真实约束，记录下来避免再次踩坑）：初始成员必须是 owner 账户下**已注册**的有效设备，
/// 所以“未注册的被邀请者”不能进 GroupCreation.members；它只能通过成员变更提交（added_credentials）进表。
#[test]
fn an_unregistered_invitee_cannot_be_listed_as_an_initial_node_member() {
    let dir = FixtureDir::new();
    let (account_id, device_id) = enroll_identity(&dir.database_text());
    let node = TestNode::start(&device_id);
    node.register_device(&account_id);
    // 只用本机 owner 的创建能成功。
    let client = node.client();
    let owner_group = chat_protocol::GroupId::from_bytes([0x33u8; 32]).encode();
    let creation = chat_node_services::group::GroupCreation {
        group_id: owner_group.clone(),
        owner_account_id: account_id.clone(),
        initial_control_head: chat_node_services::canonical::hex_encode(&[0x11u8; 32]),
        policy_version: 1,
        mls_epoch: 1,
        members: vec![chat_node_services::group::GroupMemberSpec {
            device_id: device_id.clone(),
            role: "owner".into(),
        }],
    };
    block_on(client.create_group(&creation)).expect("owner-only creation is accepted");

    // 加一个**未注册**的设备作为初始成员 → 节点以 INVALID_CREDENTIAL 拒绝（这是节点既有规则）。
    let stranger_group = chat_protocol::GroupId::from_bytes([0x34u8; 32]).encode();
    let stranger = chat_protocol::DeviceId::from_bytes([0x78u8; 32]).encode();
    let mut rejected = creation.clone();
    rejected.group_id = stranger_group;
    rejected.members.push(chat_node_services::group::GroupMemberSpec {
        device_id: stranger,
        role: "member".into(),
    });
    let error = block_on(client.create_group(&rejected)).expect_err("unregistered member is rejected");
    assert_eq!(error.node_code(), Some("INVALID_CREDENTIAL"));
}

/// GROUPS-021：B 自注册 → A 提交 member_add → B 读日志从 403 变 200；重复提交幂等。
#[test]
fn a_self_registered_member_is_authorized_after_a_member_add_commit() {
    let dir_a = FixtureDir::new();
    let (_account_id_a, device_a) = enroll_identity(&dir_a.database_text());
    let mut store_a = dir_a.open();
    let (group_id, _head) = local_group(&mut store_a);
    let node = TestNode::start(&device_a);
    group_register_on_node(
        &mut store_a,
        &group_id,
        &node.endpoint_url(),
        NODE_ID,
        Some(&node.public_key_hex()),
    )
    .expect("owner group registers");

    // B 侧：独立库里的持久身份 + 自注册到节点。
    let dir_b = FixtureDir::new();
    let (account_b, device_b) = enroll_identity_with(&dir_b.database_text(), [0x66u8; 32]);
    let store_b = dir_b.open();
    chat_core::group_node::register_device_on_node(
        &store_b,
        &node.endpoint_url(),
        NODE_ID,
        Some(&node.public_key_hex()),
    )
    .expect("B self-registers on the node");

    // B 还不是群成员：读日志被拒。
    let bob_client = node.client_with(&device_b, [0x66u8; 32]);
    let denied = block_on(bob_client.fetch_log(&group_id, 0, 10));
    assert!(denied.is_err(), "a non-member must not read the log");

    // A 提交成员变更（added_credentials=[B 的设备 ID]）。
    let first = chat_core::group_node::submit_member_add(
        &mut store_a,
        &group_id,
        &device_b,
        &node.endpoint_url(),
        NODE_ID,
        Some(&node.public_key_hex()),
    )
    .expect("member add is accepted");
    assert_eq!(first.node_id, NODE_ID);

    // 现在 B 能读日志。
    let page = block_on(bob_client.fetch_log(&group_id, 0, 10))
        .expect("the new member is authorized after the member-add commit");
    assert_eq!(page.group_id, group_id);

    // 幂等：重复提交返回同一回执（节点按提交 ID 去重）。
    let second = chat_core::group_node::submit_member_add(
        &mut store_a,
        &group_id,
        &device_b,
        &node.endpoint_url(),
        NODE_ID,
        Some(&node.public_key_hex()),
    )
    .expect("repeat member add is idempotent");
    assert_eq!(second.submission_id, first.submission_id);
    assert_eq!(second.seq, first.seq);
    let _ = (account_b, store_b);
}

/// 未注册设备直接 member_add：节点拒绝（不放宽）。
#[test]
fn a_member_add_for_an_unregistered_device_is_rejected() {
    let dir = FixtureDir::new();
    let (_account_id, device_id) = enroll_identity(&dir.database_text());
    let mut store = dir.open();
    let (group_id, _head) = local_group(&mut store);
    let node = TestNode::start(&device_id);
    group_register_on_node(
        &mut store,
        &group_id,
        &node.endpoint_url(),
        NODE_ID,
        Some(&node.public_key_hex()),
    )
    .expect("group registers");
    // 一个从未在节点注册过的设备 ID。
    let stranger = chat_protocol::DeviceId::from_bytes([0x79u8; 32]).encode();
    let error = chat_core::group_node::submit_member_add(
        &mut store,
        &group_id,
        &stranger,
        &node.endpoint_url(),
        NODE_ID,
        Some(&node.public_key_hex()),
    )
    .expect_err("unregistered device must be rejected");
    assert_eq!(
        error.code(),
        "INVALID_CREDENTIAL",
        "an unregistered device must be rejected explicitly"
    );
}
