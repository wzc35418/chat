//! GROUPS-019：群邀请经节点投递（真 axum 节点 + 真双设备双库）。
//!
//! 四类用例：成功闭环、重复提交幂等、节点不可达状态不变、非法/超长 Welcome 拒绝。
//! 每组都在失败断言后检查“状态不变”。
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::group_node::{build_node_client, fetch_group_invite, submit_group_invite};
use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, GroupInviteBlob, GroupInviteMeta, accept_group_invite,
    create_group_with_welcome, group_snapshot,
};
use chat_core::service::{ConversationCore, IdentityAccountInput};
use chat_crypto::engine::MlsDevice;
use chat_domain::MemberRole;
use chat_node_api::build_router;
use chat_node_api::state::{AppState, FixedClock as NodeFixedClock};
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_node_services::group::{GroupCreation, GroupMemberSpec};
use chat_node_services::registration::{
    DeviceCredential, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest, RegistrationCapability,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store};
use ed25519_dalek::{Signature, Signer, SigningKey};

const NOW_S: u64 = 1_700_000_000;
const ACCOUNT: &str = "acct-invite";
const NODE_ID: &str = "node-invite-test";
const MASTER_KEY: [u8; 32] = [0x5a; 32];
const ALICE_DEVICE_SEED: [u8; 32] = [0x31; 32];
const BOB_DEVICE_SEED: [u8; 32] = [0x32; 32];
const SUPERVISOR_SEED: [u8; 32] = [0x11; 32];
const UNREACHABLE: &str = "http://127.0.0.1:1";

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
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "chat-core-invite-{tag}-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
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

/// 在库里建立持久身份（供节点提交用），返回设备 ID。
fn enroll_identity(database: &str, device_seed: [u8; 32]) -> String {
    let mut core = ConversationCore::open(database, &master_key_hex()).expect("open core");
    let view = core
        .save_identity_account(IdentityAccountInput {
            display_name: "invite".to_owned(),
            account_seed_hex: hex_of(&[0x22u8; 32]),
            device_seed_hex: hex_of(&device_seed),
            created_at_ms: NOW_S * 1_000,
        })
        .expect("save identity");
    core.close();
    view.device_id
}

// ---------------------------------------------------------------- 真节点

struct TestNode {
    address: String,
    node_public_key: [u8; 32],
}

impl TestNode {
    fn start() -> Self {
        let service = NodeService::open_in_memory(NodeServiceConfig {
            node_id: NODE_ID.into(),
            signing_seed: [7u8; 32],
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
        Self {
            address,
            node_public_key,
        }
    }

    fn endpoint_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn public_key_hex(&self) -> String {
        hex_of(&self.node_public_key)
    }

    fn client(&self, device_id: &str, device_seed: [u8; 32]) -> NodeClient {
        let endpoint = NodeEndpoint::new(&self.endpoint_url(), NODE_ID)
            .expect("endpoint")
            .with_node_public_key(self.node_public_key);
        NodeClient::new(endpoint, device_id, SigningKey::from_bytes(&device_seed))
            .expect("node client")
    }

    /// 用一个客户端完成 注册 + 建群（同设备复用同一个客户端，避免挑战/会话状态互踩）。
    fn register_and_group(
        &self,
        device_id: &str,
        device_seed: [u8; 32],
        capability_id: &str,
        group: Option<(&str, &str, u64, &str)>,
    ) {
        let supervisor = SigningKey::from_bytes(&SUPERVISOR_SEED);
        let supervisor_public_key = hex_of(&supervisor.verifying_key().to_bytes());
        let mut credential = DeviceCredential {
            account_id: ACCOUNT.into(),
            device_id: device_id.to_owned(),
            device_public_key: hex_of(&SigningKey::from_bytes(&device_seed).verifying_key().to_bytes()),
            endpoint_id: format!("endpoint-{capability_id}"),
            credential_version: 1,
            not_before: NOW_S - 10,
            expires_at: NOW_S + 3_600,
            supervisor_public_key: supervisor_public_key.clone(),
            signature: String::new(),
        };
        let signature: Signature = supervisor.sign(&credential.signing_bytes().expect("credential"));
        credential.signature = hex_of(&signature.to_bytes());
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
        let signature: Signature = supervisor.sign(&capability.signing_bytes().expect("capability"));
        capability.signature = hex_of(&signature.to_bytes());
        let request = RegisterDeviceRequest {
            account_id: ACCOUNT.into(),
            credential,
            capability,
        };
        let client = self.client(device_id, device_seed);
        block_on(client.register_device(&request)).expect("register");
        if let Some((group_id, control_head, epoch, invitee_device)) = group {
            let creation = GroupCreation {
                group_id: group_id.to_owned(),
                owner_account_id: ACCOUNT.into(),
                initial_control_head: control_head.to_owned(),
                policy_version: 1,
                mls_epoch: epoch,
                members: vec![
                    GroupMemberSpec {
                        device_id: device_id.to_owned(),
                        role: "owner".into(),
                    },
                    GroupMemberSpec {
                        device_id: invitee_device.to_owned(),
                        role: "member".into(),
                    },
                ],
            };
            block_on(client.create_group(&creation)).expect("group registers");
        }
    }

    /// 在节点登记群（调用前所有成员都必须已注册，否则节点以 INVALID_CREDENTIAL 拒绝）。
    fn create_group(&self, group_id: &str, control_head: &str, epoch: u64, owner: &str, invitee: &str) {
        let creation = GroupCreation {
            group_id: group_id.to_owned(),
            owner_account_id: ACCOUNT.into(),
            initial_control_head: control_head.to_owned(),
            policy_version: 1,
            mls_epoch: epoch,
            members: vec![
                GroupMemberSpec {
                    device_id: owner.to_owned(),
                    role: "owner".into(),
                },
                GroupMemberSpec {
                    device_id: invitee.to_owned(),
                    role: "member".into(),
                },
            ],
        };
        let client = self.client(owner, ALICE_DEVICE_SEED);
        block_on(client.create_group(&creation)).expect("group registers");
    }

    fn entry_count(&self, group_id: &str, device_id: &str, seed: [u8; 32]) -> usize {
        let page = block_on(self.client(device_id, seed).fetch_log(group_id, 0, 50))
            .expect("node log");
        page.entries.len()
    }
}

// ---------------------------------------------------------------- 双设备双库夹具

struct Fixture {
    #[allow(dead_code)]
    dir_a: FixtureDir,
    #[allow(dead_code)]
    dir_b: FixtureDir,
    store_a: Store,
    store_b: Store,
    alice: MlsDevice,
    bob: MlsDevice,
    group_id: String,
    mls_key: String,
    epoch: u64,
    control_head: String,
    alice_device: String,
    bob_device: String,
}

impl Fixture {
    fn new() -> Self {
        let dir_a = FixtureDir::new("a");
        let dir_b = FixtureDir::new("b");
        let alice_device = enroll_identity(&dir_a.database_text(), ALICE_DEVICE_SEED);
        let bob_device = enroll_identity(&dir_b.database_text(), BOB_DEVICE_SEED);
        let store_a = dir_a.open();
        let store_b = dir_b.open();
        let alice = MlsDevice::new("alice").expect("alice");
        let bob = MlsDevice::new("bob").expect("bob");
        // 群与 Welcome 只在 welcome_bytes() 里创建一次：bob 的 KeyPackage 只能用一次。
        Self {
            dir_a,
            dir_b,
            store_a,
            store_b,
            alice,
            bob,
            group_id: String::new(),
            mls_key: String::new(),
            epoch: 0,
            control_head: String::new(),
            alice_device,
            bob_device,
        }
    }

    fn meta(&self) -> GroupInviteMeta {
        GroupInviteMeta {
            group_id: self.group_id.clone(),
            mls_group_id: self.mls_key.clone(),
            name: "invite-group".to_owned(),
            inviter_credential: "alice".to_owned(),
            owner_credential: "alice".to_owned(),
            invitee_credential: "bob".to_owned(),
            members: vec!["alice".to_owned(), "bob".to_owned()],
            epoch: self.epoch,
        }
    }

    /// 重新建群并返回欢迎字节（邀请的 payload）。
    fn welcome_bytes(&mut self) -> Vec<u8> {
        let peer = GroupBootstrapPeer {
            credential: "bob".to_owned(),
            device: &self.bob,
        };
        let request = CreateGroupRequest::new("invite-group", "alice");
        let (view, welcome) =
            create_group_with_welcome(&mut self.alice, &peer, &mut self.store_a, &request)
                .expect("create");
        self.group_id = view.group_id;
        self.mls_key = view.mls_group_key;
        self.epoch = view.epoch;
        self.control_head = view.control_head;
        welcome
    }
}

#[test]
fn invite_is_delivered_through_the_node_and_accepted_by_the_peer() {
    let mut fixture = Fixture::new();
    let welcome = fixture.welcome_bytes();
    let meta = fixture.meta();
    let blob = GroupInviteBlob::new(meta.clone(), welcome).expect("blob");
    let node = TestNode::start();
    node.register_and_group(&fixture.alice_device, ALICE_DEVICE_SEED, "cap-alice", None);
    node.register_and_group(&fixture.bob_device, BOB_DEVICE_SEED, "cap-bob", None);
    node.create_group(
        &fixture.group_id,
        &fixture.control_head,
        fixture.epoch,
        &fixture.alice_device,
        &fixture.bob_device,
    );

    // A：提交邀请（真回执）。
    let client_a = build_node_client(
        &fixture.store_a,
        &node.endpoint_url(),
        NODE_ID,
        Some(&node.public_key_hex()),
    )
    .expect("client a");
    let receipt = submit_group_invite(&client_a, &fixture.group_id, &blob).expect("submit");
    assert!(!receipt.submission_id.is_empty());
    assert_eq!(receipt.node_id, NODE_ID);
    assert_eq!(node.entry_count(&fixture.group_id, &fixture.alice_device, ALICE_DEVICE_SEED), 1);

    // B：从群日志取回并接受。
    let client_b = build_node_client(
        &fixture.store_b,
        &node.endpoint_url(),
        NODE_ID,
        Some(&node.public_key_hex()),
    )
    .expect("client b");
    let fetched = fetch_group_invite(&client_b, &fixture.group_id)
        .expect("fetch");
    let fetched = fetched.expect("the invite must be found");
    assert_eq!(fetched.meta, meta, "metadata round-trips byte for byte");
    let view = accept_group_invite(&mut fixture.bob, &mut fixture.store_b, &fetched.welcome, &fetched.meta)
        .expect("accept");
    assert_eq!(view.group_id, fixture.group_id);
    assert_eq!(view.epoch, fixture.epoch);
    assert_eq!(view.role, MemberRole::Member);
    let mut members: Vec<String> = view.members.iter().map(|m| m.credential.clone()).collect();
    members.sort();
    assert_eq!(members, vec!["alice".to_owned(), "bob".to_owned()]);
    // 快照落库一致。
    let snapshot = group_snapshot(&fixture.store_b, &fixture.group_id).expect("snapshot");
    assert_eq!(snapshot.epoch, fixture.epoch);
}

#[test]
fn submitting_the_same_invite_twice_is_idempotent() {
    let mut fixture = Fixture::new();
    let welcome = fixture.welcome_bytes();
    let blob = GroupInviteBlob::new(fixture.meta(), welcome).expect("blob");
    let node = TestNode::start();
    node.register_and_group(&fixture.alice_device, ALICE_DEVICE_SEED, "cap-alice", None);
    node.register_and_group(&fixture.bob_device, BOB_DEVICE_SEED, "cap-bob", None);
    node.create_group(
        &fixture.group_id,
        &fixture.control_head,
        fixture.epoch,
        &fixture.alice_device,
        &fixture.bob_device,
    );
    let client = build_node_client(
        &fixture.store_a,
        &node.endpoint_url(),
        NODE_ID,
        Some(&node.public_key_hex()),
    )
    .expect("client");
    let first = submit_group_invite(&client, &fixture.group_id, &blob).expect("first submit");
    let second = submit_group_invite(&client, &fixture.group_id, &blob).expect("second submit");
    assert_eq!(first.submission_id, second.submission_id, "same deterministic submission id");
    assert_eq!(first.seq, second.seq, "the node returns the original receipt");
    assert_eq!(first.log_hash, second.log_hash);
    assert_eq!(
        node.entry_count(&fixture.group_id, &fixture.alice_device, ALICE_DEVICE_SEED),
        1,
        "a repeated submission must not add a second log entry"
    );
}

#[test]
fn an_unreachable_node_is_explicit_and_leaves_the_snapshot_unchanged() {
    let mut fixture = Fixture::new();
    let welcome = fixture.welcome_bytes();
    let blob = GroupInviteBlob::new(fixture.meta(), welcome).expect("blob");
    let before = group_snapshot(&fixture.store_a, &fixture.group_id).expect("snapshot");
    let client = build_node_client(&fixture.store_a, UNREACHABLE, NODE_ID, None)
        .expect("client builds even for an unreachable endpoint");
    let error = submit_group_invite(&client, &fixture.group_id, &blob)
        .expect_err("unreachable node must fail");
    assert_eq!(error.code(), "NODE_UNAVAILABLE");
    let after = group_snapshot(&fixture.store_a, &fixture.group_id).expect("snapshot");
    assert_eq!(after, before, "a failed submission must not change the group snapshot");
}

#[test]
fn invalid_or_oversized_welcomes_are_rejected_before_any_submission() {
    let mut fixture = Fixture::new();
    let welcome = fixture.welcome_bytes();
    let meta = fixture.meta();
    let node = TestNode::start();
    node.register_and_group(&fixture.alice_device, ALICE_DEVICE_SEED, "cap-alice", None);
    node.register_and_group(&fixture.bob_device, BOB_DEVICE_SEED, "cap-bob", None);
    node.create_group(
        &fixture.group_id,
        &fixture.control_head,
        fixture.epoch,
        &fixture.alice_device,
        &fixture.bob_device,
    );
    let before = group_snapshot(&fixture.store_a, &fixture.group_id).expect("snapshot");

    // 超长 Welcome（>64 KiB）→ CONTENT_TOO_LARGE。
    let oversized = vec![0u8; 64 * 1024 + 1];
    let error = GroupInviteBlob::new(meta.clone(), oversized).expect_err("oversized welcome");
    assert_eq!(error.code(), "CONTENT_TOO_LARGE");

    // 空 Welcome → INVALID_INPUT。
    let error = GroupInviteBlob::new(meta.clone(), Vec::new()).expect_err("empty welcome");
    assert_eq!(error.code(), "INVALID_INPUT");

    // 自相矛盾的元数据（被邀请者不在成员里）→ INVALID_INPUT。
    let mut broken = meta.clone();
    broken.members = vec!["alice".to_owned()];
    let error = GroupInviteBlob::new(broken, welcome.clone()).expect_err("invitee missing");
    assert_eq!(error.code(), "INVALID_INPUT");

    // 被邀请者就是群主 → INVALID_INPUT。
    let mut self_invite = meta.clone();
    self_invite.invitee_credential = "alice".to_owned();
    let error = GroupInviteBlob::new(self_invite, welcome.clone()).expect_err("self invite");
    assert_eq!(error.code(), "INVALID_INPUT");

    // 以上都发生在提交之前：节点日志仍为空，快照不变。
    assert_eq!(
        node.entry_count(&fixture.group_id, &fixture.alice_device, ALICE_DEVICE_SEED),
        0,
        "a rejected invite must never reach the node"
    );
    let after = group_snapshot(&fixture.store_a, &fixture.group_id).expect("snapshot");
    assert_eq!(after, before);
}

