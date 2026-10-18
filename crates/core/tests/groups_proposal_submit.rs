//! GROUPS-013（投递与提交侧权限）：真节点 + 真双设备两库。
//!
//! 覆盖：成员 alice prepare → 提交到真实节点（日志有条目、回执真实）→ 群主 bob 从日志拉取 →
//! process → commit（pending）→ alice 合并后离群且不能再加密；提交侧权限（成员无提交权、无提案拒绝）；
//! 节点不可达时状态不变。

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, GroupView, PeerEntryInput, PeerEntryOutcome,
    ProposalEntryInput, create_group_with_welcome, group_commit_proposals, group_join_from_welcome,
    group_prepare_leave, group_process_peer_entry, group_process_proposal, group_snapshot,
    group_submit_proposal,
};
use chat_core::service::{ConversationCore, IdentityAccountInput};
use chat_crypto::control::{CommitExpectation, WelcomeAcceptance};
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
const ACCOUNT: &str = "acct-core-propsub";
const NODE_ID: &str = "node-core-propsub";
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
            "chat-core-propsub-{}-{nanos}-{counter}",
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

fn master_key_hex() -> String {
    MASTER_KEY.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("hex"))
        .collect()
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(future)
}

/// 在库里建立持久身份（供节点提交用），返回设备 ID。
fn enroll_identity(database: &str) -> String {
    let mut core = ConversationCore::open(database, &master_key_hex()).expect("open core");
    let view = core
        .save_identity_account(IdentityAccountInput {
            display_name: "提案投递".to_owned(),
            account_seed_hex: hex_of(&ACCOUNT_SEED),
            device_seed_hex: hex_of(&DEVICE_SEED),
            created_at_ms: NOW_S * 1_000,
        })
        .expect("save identity");
    core.close();
    view.device_id
}

// ---------------------------------------------------------------- 真实节点

struct TestNode {
    address: String,
    node_public_key: [u8; 32],
    device_id: String,
}

impl TestNode {
    /// 起节点、登记设备、登记群（群用本机持久身份的设备作为 owner）。
    fn start(group_id: &str, control_head: &str, mls_epoch: u64, device_id: &str) -> Self {
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
        let node = Self {
            address,
            node_public_key,
            device_id: device_id.to_owned(),
        };
        block_on(node.client().register_device(&node.registration_request())).expect("register");
        let creation = GroupCreation {
            group_id: group_id.to_owned(),
            owner_account_id: ACCOUNT.into(),
            initial_control_head: control_head.to_owned(),
            policy_version: 1,
            mls_epoch,
            members: vec![GroupMemberSpec {
                device_id: device_id.to_owned(),
                role: "owner".into(),
            }],
        };
        block_on(node.client().create_group(&creation)).expect("group registers on the node");
        node
    }

    fn endpoint_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn public_key_hex(&self) -> String {
        hex_of(&self.node_public_key)
    }

    fn client(&self) -> NodeClient {
        let endpoint = NodeEndpoint::new(&self.endpoint_url(), NODE_ID)
            .expect("endpoint")
            .with_node_public_key(self.node_public_key);
        NodeClient::new(
            endpoint,
            self.device_id.clone(),
            SigningKey::from_bytes(&DEVICE_SEED),
        )
        .expect("node client")
    }

    fn registration_request(&self) -> RegisterDeviceRequest {
        let supervisor = SigningKey::from_bytes(&SUPERVISOR_SEED);
        let supervisor_public_key = hex_of(&supervisor.verifying_key().to_bytes());
        let mut credential = DeviceCredential {
            account_id: ACCOUNT.into(),
            device_id: self.device_id.clone(),
            device_public_key: hex_of(&SigningKey::from_bytes(&DEVICE_SEED).verifying_key().to_bytes()),
            endpoint_id: "endpoint-propsub".into(),
            credential_version: 1,
            not_before: NOW_S - 10,
            expires_at: NOW_S + 3_600,
            supervisor_public_key: supervisor_public_key.clone(),
            signature: String::new(),
        };
        let signature: Signature = supervisor.sign(&credential.signing_bytes().expect("credential"));
        credential.signature = hex_of(&signature.to_bytes());
        let mut capability = RegistrationCapability {
            capability_id: "cap-propsub".into(),
            account_id: ACCOUNT.into(),
            device_id: self.device_id.clone(),
            purpose: REGISTRATION_CAPABILITY_PURPOSE.into(),
            issued_at: NOW_S - 10,
            expires_at: NOW_S + 600,
            revocation_version: 1,
            supervisor_public_key,
            signature: String::new(),
        };
        let signature: Signature = supervisor.sign(&capability.signing_bytes().expect("capability"));
        capability.signature = hex_of(&signature.to_bytes());
        RegisterDeviceRequest {
            account_id: ACCOUNT.into(),
            credential,
            capability,
        }
    }

    /// 节点日志里的 (submission_id, 提案字节)。
    fn entries(&self, group_id: &str) -> Vec<(String, Vec<u8>)> {
        let page = block_on(self.client().fetch_log(group_id, 0, 50)).expect("node log");
        page.entries
            .into_iter()
            .map(|entry| {
                (
                    entry.receipt.submission_id,
                    decode_hex(&entry.candidate.candidate.encrypted_payload),
                )
            })
            .collect()
    }
}

// ---------------------------------------------------------------- 双设备双库

struct Fixture {
    dir_a: FixtureDir,
    dir_b: FixtureDir,
    store_a: Store,
    store_b: Store,
    alice: MlsDevice,
    bob: MlsDevice,
    group_id: String,
    mls_key: String,
    view_a: GroupView,
    view_b: GroupView,
    device_id: String,
}

impl Fixture {
    fn new() -> Self {
        let dir_a = FixtureDir::new();
        let dir_b = FixtureDir::new();
        let device_id = enroll_identity(&dir_a.database_text());
        let mut store_a = dir_a.open();
        let mut store_b = dir_b.open();
        let mut alice = MlsDevice::new("alice").expect("alice");
        let mut bob = MlsDevice::new("bob").expect("bob");
        // bob 建群（群主），alice 用 Welcome 加入（成员）。
        let request = CreateGroupRequest::new("提案投递群", "bob");
        let peer = GroupBootstrapPeer {
            credential: "alice".to_owned(),
            device: &alice,
        };
        let (view_b, welcome) =
            create_group_with_welcome(&mut bob, &peer, &mut store_b, &request).expect("create");
        let acceptance = WelcomeAcceptance {
            group_id: view_b.mls_group_key.clone(),
            expected_epoch: view_b.epoch,
            members: vec!["alice".to_owned(), "bob".to_owned()],
        };
        let view_a = group_join_from_welcome(
            &mut alice,
            &mut store_a,
            "提案投递群",
            &welcome,
            &acceptance,
            "alice",
            "bob",
        )
        .expect("alice joins");
        assert_eq!(view_a.group_id, view_b.group_id);
        Self {
            dir_a,
            dir_b,
            store_a,
            store_b,
            alice,
            bob,
            group_id: view_b.group_id.clone(),
            mls_key: view_b.mls_group_key.clone(),
            view_a,
            view_b,
            device_id,
        }
    }
}

#[test]
fn member_submits_proposal_and_owner_commits_it() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id.clone();
    assert_eq!(fixture.view_b.role, MemberRole::Owner);
    assert_eq!(fixture.view_a.role, MemberRole::Member);
    let node = TestNode::start(
        &group_id,
        &fixture.view_a.control_head,
        fixture.view_a.epoch,
        &fixture.device_id,
    );

    // 1) 成员发起自移除提案。
    let proposal = group_prepare_leave(&mut fixture.alice, &mut fixture.store_a, &group_id)
        .expect("prepare self-removal");
    assert_eq!(proposal.base_epoch, 1);
    assert!(!proposal.proposal.is_empty());

    // 2) 提交到真实节点：真实回执 + 节点日志有条目（内容就是提案字节）。
    let receipt = group_submit_proposal(
        &mut fixture.alice,
        &mut fixture.store_a,
        &group_id,
        &node.endpoint_url(),
        NODE_ID,
        Some(&node.public_key_hex()),
    )
    .expect("submit the proposal");
    assert!(receipt.seq >= 1);
    assert!(receipt.submission_id.starts_with("sub:1:"));
    assert_eq!(receipt.log_hash.len(), 64);
    assert_eq!(receipt.node_id, NODE_ID);
    let entries = node.entries(&group_id);
    assert_eq!(entries.len(), 1, "exactly one proposal entry on the node");
    assert_eq!(entries[0].0, receipt.submission_id);
    assert_eq!(entries[0].1, proposal.proposal);

    // 3) 群主从节点日志拉取提案、处理入队。
    let outcome = group_process_proposal(
        &mut fixture.bob,
        &mut fixture.store_b,
        &ProposalEntryInput::new(group_id.clone(), 1, "alice", entries[0].1.clone()),
    )
    .expect("process the proposal");
    assert_eq!(outcome.kind, "remove");
    assert_eq!(outcome.targets, vec!["alice".to_owned()]);
    assert_eq!(outcome.proposals_pending, 1);

    // 4) 群主打包成 pending 提交（epoch 不推进）。
    let committed = group_commit_proposals(&mut fixture.bob, &mut fixture.store_b, &group_id)
        .expect("commit the proposal");
    assert_eq!(committed.epoch, 1);
    let pending = committed.pending.as_ref().expect("pending control commit");
    assert_eq!(pending.operation, "remove_members");
    assert_eq!(pending.target_epoch, 2);

    // 5) alice 合并该提交：真正离群（self_removed），此后不能加密。
    let commit = fixture
        .bob
        .pending_control()
        .expect("pending query")
        .expect("pending commit")
        .commit;
    let mut input = PeerEntryInput::new(group_id.clone(), 1, "bob", commit);
    input.removed_members = vec!["alice".to_owned()];
    input.expectation = Some(CommitExpectation {
        added: 0,
        removed: 1,
        updated: 0,
    });
    let outcome = group_process_peer_entry(&mut fixture.alice, &mut fixture.store_a, &input)
        .expect("merge the removal commit");
    match outcome {
        PeerEntryOutcome::Commit {
            self_removed,
            new_epoch,
            view,
            ..
        } => {
            assert!(self_removed);
            assert_eq!(new_epoch, 2);
            assert!(view.members.iter().all(|member| !member.is_local));
        }
        other => panic!("expected a merged commit, got {other:?}"),
    }
    assert!(
        fixture.alice.encrypt("gone").is_err(),
        "a removed device can no longer encrypt"
    );
    let _ = (&fixture.dir_a, &fixture.dir_b, fixture.mls_key.clone());
}

#[test]
fn commit_side_permissions_are_enforced() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id.clone();

    // 没有待提交提案时打包 → NO_PENDING_PROPOSALS。
    let error = group_commit_proposals(&mut fixture.bob, &mut fixture.store_b, &group_id)
        .expect_err("no pending proposals");
    assert_eq!(error.code(), "NO_PENDING_PROPOSALS");

    // 成员（alice，提案人）自己打包自己的移除提案 → PERMISSION_DENIED。
    let proposal = group_prepare_leave(&mut fixture.alice, &mut fixture.store_a, &group_id)
        .expect("prepare self-removal");
    let error = group_commit_proposals(&mut fixture.alice, &mut fixture.store_a, &group_id)
        .expect_err("a member cannot commit proposals");
    assert_eq!(error.code(), "PERMISSION_DENIED");

    // 群主收下他人提案后可以打包（他人提案 + 群主权限）。
    group_process_proposal(
        &mut fixture.bob,
        &mut fixture.store_b,
        &ProposalEntryInput::new(group_id.clone(), 1, "alice", proposal.proposal.clone()),
    )
    .expect("owner processes the proposal");
    let committed = group_commit_proposals(&mut fixture.bob, &mut fixture.store_b, &group_id)
        .expect("owner commits the proposal");
    assert!(committed.pending.is_some());
    // 打包后待提交提案被清空 → 再打包显式失败。
    let error = group_commit_proposals(&mut fixture.bob, &mut fixture.store_b, &group_id)
        .expect_err("already committed");
    assert!(!error.code().is_empty());
}

#[test]
fn submitting_without_a_proposal_or_reachable_node_changes_nothing() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id.clone();

    // 没有待提交提案 → NO_PENDING_PROPOSALS（不触网）。
    let error = group_submit_proposal(
        &mut fixture.alice,
        &mut fixture.store_a,
        &group_id,
        UNREACHABLE_ENDPOINT,
        NODE_ID,
        None,
    )
    .expect_err("no proposal to submit");
    assert_eq!(error.code(), "NO_PENDING_PROPOSALS");

    // 有提案但节点不可达 → NODE_UNAVAILABLE，本地状态不变。
    let before = group_snapshot(&fixture.store_a, &group_id).expect("snapshot");
    group_prepare_leave(&mut fixture.alice, &mut fixture.store_a, &group_id)
        .expect("prepare self-removal");
    let prepared = group_snapshot(&fixture.store_a, &group_id).expect("snapshot");
    assert_eq!(prepared.epoch, before.epoch);
    let error = group_submit_proposal(
        &mut fixture.alice,
        &mut fixture.store_a,
        &group_id,
        UNREACHABLE_ENDPOINT,
        NODE_ID,
        None,
    )
    .expect_err("unreachable node");
    assert_eq!(error.code(), "NODE_UNAVAILABLE");
    assert_eq!(
        group_snapshot(&fixture.store_a, &group_id).expect("snapshot"),
        prepared
    );
}
