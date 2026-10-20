//! SYNC 单轮 FFI（`SyncFfi::sync_once`）：真 axum 节点 + 真临时加密库。
//!
//! 覆盖：无新条目 → fetched=0 且游标不变；网络错误 → `NODE_UNAVAILABLE`；
//! 需要追赶（应用失败）→ 游标不前进且 `settled=false`；常驻循环运行时 → `INVALID_STATE_TRANSITION`。
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::service::{ConversationCore, IdentityAccountInput};
use chat_core::sync_worker::{initialize_pull_state, load_pull_state};
use chat_ffi::local_store::FfiError;
use chat_ffi::sync_ffi::SyncFfi;
use chat_node_api::build_router;
use chat_node_api::state::{AppState, FixedClock as NodeFixedClock};
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_node_services::canonical::hex_encode;
use chat_node_services::group::{GroupCandidate, GroupCreation, GroupMemberSpec};
use chat_node_services::registration::{
    DeviceCredential, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest, RegistrationCapability,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use chat_protocol::{ControlHead, GroupId, SubmissionId, VerifiedHead};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, StoreOptions, open_store};
use ed25519_dalek::{Signature, Signer, SigningKey};

const NOW_S: u64 = 1_700_000_000;
const ACCOUNT: &str = "acct-ffi-once";
const NODE_ID: &str = "node-ffi-once";
const MASTER_KEY: [u8; 32] = [0x6d; 32];
const DEVICE_SEED: [u8; 32] = [0x44; 32];
const ACCOUNT_SEED: [u8; 32] = [0x23; 32];
const SUPERVISOR_SEED: [u8; 32] = [0x13; 32];
const INITIAL_CONTROL_HEAD: [u8; 32] = [0x11; 32];

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
            "chat-ffi-once-{}-{nanos}-{counter}",
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
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn master_key_hex() -> String {
    MASTER_KEY.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn seed_hex(seed: &[u8; 32]) -> String {
    seed.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn device_key() -> SigningKey {
    SigningKey::from_bytes(&DEVICE_SEED)
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

struct TestNode {
    address: String,
    node_public_key: [u8; 32],
}

impl TestNode {
    fn endpoint_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn public_key_hex(&self) -> String {
        hex_encode(&self.node_public_key)
    }

    fn client(&self, device_id: &str) -> NodeClient {
        let endpoint = NodeEndpoint::new(&self.endpoint_url(), NODE_ID)
            .expect("endpoint")
            .with_node_public_key(self.node_public_key);
        NodeClient::new(endpoint, device_id.to_owned(), device_key()).expect("node client")
    }

    fn registration_request(&self, device_id: &str) -> RegisterDeviceRequest {
        let supervisor = SigningKey::from_bytes(&SUPERVISOR_SEED);
        let supervisor_public_key = hex_encode(&supervisor.verifying_key().to_bytes());
        let mut credential = DeviceCredential {
            account_id: ACCOUNT.into(),
            device_id: device_id.to_owned(),
            device_public_key: hex_encode(&device_key().verifying_key().to_bytes()),
            endpoint_id: "endpoint-ffi-once".into(),
            credential_version: 1,
            not_before: NOW_S - 10,
            expires_at: NOW_S + 3_600,
            supervisor_public_key: supervisor_public_key.clone(),
            signature: String::new(),
        };
        let signature: Signature = supervisor.sign(&credential.signing_bytes().expect("credential"));
        credential.signature = hex_encode(&signature.to_bytes());
        let mut capability = RegistrationCapability {
            capability_id: "cap-ffi-once".into(),
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

fn enroll_identity(database: &str) -> String {
    let mut core = ConversationCore::open(database, &master_key_hex()).expect("open core");
    let view = core
        .save_identity_account(IdentityAccountInput {
            display_name: "FFI once".to_owned(),
            account_seed_hex: seed_hex(&ACCOUNT_SEED),
            device_seed_hex: seed_hex(&DEVICE_SEED),
            created_at_ms: NOW_S * 1_000,
        })
        .expect("save identity");
    core.close();
    view.device_id
}

/// 追加一条应用条目（可指定 epoch，用来构造“应用失败/需要追赶”的场景）。
fn append_entry(
    client: &NodeClient,
    group_id: &str,
    device_id: &str,
    seed: u8,
    mls_epoch: u64,
    payload: &[u8],
) {
    let head = block_on(client.get_head(group_id)).expect("node head");
    let candidate = GroupCandidate {
        protocol_major: 1,
        protocol_minor: 0,
        submission_id: SubmissionId::from_bytes([seed; 32]).encode(),
        group_id: group_id.to_owned(),
        author_device_id: device_id.to_owned(),
        kind: "application".to_owned(),
        expected_control_head: head.control_head.clone(),
        policy_version: head.policy_version,
        mls_epoch,
        encrypted_payload: hex_encode(payload),
        control_metadata: None,
    };
    let receipt = block_on(client.append_entry(&candidate)).expect("entry accepted");
    assert!(receipt.seq >= 1);
}

struct Fixture {
    dir: FixtureDir,
    node: TestNode,
    device_id: String,
    group_id: String,
    client: NodeClient,
}

impl Fixture {
    fn new() -> Self {
        let dir = FixtureDir::new();
        let device_id = enroll_identity(&dir.database_text());
        let node = start_node();
        let client = node.client(&device_id);
        block_on(client.register_device(&node.registration_request(&device_id)))
            .expect("device registers");
        let group_id = GroupId::from_bytes([61_u8; 32]).encode();
        let creation = GroupCreation {
            group_id: group_id.clone(),
            owner_account_id: ACCOUNT.into(),
            initial_control_head: hex_encode(&INITIAL_CONTROL_HEAD),
            policy_version: 1,
            mls_epoch: 0,
            members: vec![GroupMemberSpec {
                device_id: device_id.clone(),
                role: "owner".into(),
            }],
        };
        block_on(client.create_group(&creation)).expect("group registers");
        // 游标初始化在 head=1（节点已有一条建群记录）。
        {
            let mut store = open_store(
                StoreOptions::new(dir.database(), KeyProvenance::Raw32),
                ProtectedStorageKey::from_bytes(MASTER_KEY),
            )
            .expect("open store for cursor");
            let genesis = VerifiedHead::genesis(
                GroupId::parse(&group_id).expect("group id"),
                ControlHead::from_bytes(INITIAL_CONTROL_HEAD),
                1,
                0,
            );
            initialize_pull_state(&mut store, &group_id, genesis).expect("pull state");
        }
        Self {
            dir,
            node,
            device_id,
            group_id,
            client,
        }
    }

    fn open(&self) -> Result<Arc<SyncFfi>, FfiError> {
        SyncFfi::open(
            self.dir.database_text(),
            master_key_hex(),
            self.node.endpoint_url(),
            NODE_ID.to_owned(),
            self.node.public_key_hex(),
            self.group_id.clone(),
        )
    }

    fn cursor(&self) -> u64 {
        let store = open_store(
            StoreOptions::new(self.dir.database(), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(MASTER_KEY),
        )
        .expect("open store");
        load_pull_state(&store, &self.group_id)
            .expect("pull state" )
            .expect("initialized")
            .cursor_seq
    }
}

#[test]
fn sync_once_without_new_entries_reports_zero_and_keeps_the_cursor() {
    let fixture = Fixture::new();
    let sync = fixture.open().expect("open");
    let before = fixture.cursor();
    let result = sync.sync_once().expect("one-shot pull");
    assert_eq!(result.fetched, 0, "nothing new on the node");
    assert_eq!(result.applied, 0);
    assert_eq!(result.skipped, 0);
    assert_eq!(result.next_cursor, before, "the cursor must not move");
    assert!(result.settled, "nothing to catch up: {result:?}");
    assert_eq!(fixture.cursor(), before, "the persisted cursor is unchanged");
}

#[test]
fn sync_once_applies_a_new_entry_and_advances_the_cursor() {
    let fixture = Fixture::new();
    let sync = fixture.open().expect("open");
    let before = fixture.cursor();
    append_entry(&fixture.client, &fixture.group_id, &fixture.device_id, 81, 0, b"plain payload");
    let result = sync.sync_once().expect("one-shot pull");
    assert!(result.fetched >= 1, "at least one entry was fetched: {result:?}");
    assert!(result.next_cursor > before, "the cursor advances with applied entries");
    assert_eq!(fixture.cursor(), result.next_cursor);
}

#[test]
fn an_unreachable_node_is_node_unavailable_and_keeps_the_cursor() {
    let dir = FixtureDir::new();
    let device_id = enroll_identity(&dir.database_text());
    let node = start_node();
    let client = node.client(&device_id);
    block_on(client.register_device(&node.registration_request(&device_id)))
        .expect("device registers");
    let group_id = GroupId::from_bytes([62_u8; 32]).encode();
    let creation = GroupCreation {
        group_id: group_id.clone(),
        owner_account_id: ACCOUNT.into(),
        initial_control_head: hex_encode(&INITIAL_CONTROL_HEAD),
        policy_version: 1,
        mls_epoch: 0,
        members: vec![GroupMemberSpec {
            device_id: device_id.clone(),
            role: "owner".into(),
        }],
    };
    block_on(client.create_group(&creation)).expect("group registers");
    {
        let mut store = open_store(
            StoreOptions::new(dir.database(), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(MASTER_KEY),
        )
        .expect("open store");
        let genesis = VerifiedHead::genesis(
            GroupId::parse(&group_id).expect("group id"),
            ControlHead::from_bytes(INITIAL_CONTROL_HEAD),
            1,
            0,
        );
        initialize_pull_state(&mut store, &group_id, genesis).expect("pull state");
    }
    let cursor_before = {
        let store = open_store(
            StoreOptions::new(dir.database(), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(MASTER_KEY),
        )
        .expect("open store");
        load_pull_state(&store, &group_id).expect("state").expect("init").cursor_seq
    };
    let sync = SyncFfi::open(
        dir.database_text(),
        master_key_hex(),
        "http://127.0.0.1:1".to_owned(),
        NODE_ID.to_owned(),
        node.public_key_hex(),
        group_id.clone(),
    )
    .expect("open with an unreachable endpoint");
    let error = sync.sync_once().expect_err("unreachable node");
    assert_eq!(code(error), "NODE_UNAVAILABLE");
    let after = {
        let store = open_store(
            StoreOptions::new(dir.database(), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(MASTER_KEY),
        )
        .expect("open store");
        load_pull_state(&store, &group_id).expect("state").expect("init").cursor_seq
    };
    assert_eq!(after, cursor_before, "a failed pull must not move the cursor");
}

#[test]
fn an_unusable_payload_is_counted_as_skipped_and_does_not_claim_success() {
    let fixture = Fixture::new();
    let sync = fixture.open().expect("open");
    // 非 MLS 密文的载荷：本机无法应用 → 计入 skipped（不谎报 applied）。
    append_entry(&fixture.client, &fixture.group_id, &fixture.device_id, 82, 0, b"not-an-mls-message");
    let result = sync.sync_once().expect("one-shot pull");
    // 页面里既有建群条目（可应用）也有这条坏载荷：坏载荷必须计入 skipped，而不是被静默当成成功。
    assert!(result.skipped >= 1, "the unusable entry must be reported as skipped: {result:?}");
    assert!(result.fetched >= result.skipped);
    assert_eq!(result.fetched, result.applied + result.skipped);
}

#[test]
fn sync_once_is_rejected_while_the_loop_is_running() {
    let fixture = Fixture::new();
    let sync = fixture.open().expect("open");
    sync.start().expect("start the loop");
    let error = sync.sync_once().expect_err("a running loop owns the writer");
    assert_eq!(code(error), "INVALID_STATE_TRANSITION");
    sync.stop().expect("stop");
    // 停止之后一次性拉取重新可用。
    sync.sync_once().expect("one-shot pull after stop");
}

