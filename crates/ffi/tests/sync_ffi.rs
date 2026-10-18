//! 常驻同步 FFI（SyncFfi）端到端用例：真 axum 节点 + 真临时加密库，全程经 FFI。
//!
//! 覆盖：open 不自动启动（idle）→ start 后追加条目被应用（applied_total 增长）→ stop 后不再增长 →
//! 游标未初始化时 open 显式失败 → shutdown 幂等且之后所有调用 CORE_CLOSED。

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chat_core::service::{ConversationCore, IdentityAccountInput};
use chat_core::sync_worker::initialize_pull_state;
use chat_ffi::local_store::FfiError;
use chat_ffi::sync_ffi::SyncFfi;
use chat_node_api::build_router;
use chat_node_api::state::{AppState, FixedClock as NodeFixedClock};
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_node_services::canonical::hex_encode;
use chat_node_services::group::{GroupCreation, GroupMemberSpec, GroupCandidate};
use chat_node_services::registration::{
    DeviceCredential, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest, RegistrationCapability,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use chat_protocol::{ControlHead, GroupId, SubmissionId, VerifiedHead};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, StoreOptions, open_store};
use ed25519_dalek::{Signature, Signer, SigningKey};

const NOW_S: u64 = 1_700_000_000;
const ACCOUNT: &str = "acct-ffi-sync";
const NODE_ID: &str = "node-ffi-sync";
const MASTER_KEY: [u8; 32] = [0x5a; 32];
const DEVICE_SEED: [u8; 32] = [0x33; 32];
const ACCOUNT_SEED: [u8; 32] = [0x22; 32];
const SUPERVISOR_SEED: [u8; 32] = [0x11; 32];
const INITIAL_CONTROL_HEAD: [u8; 32] = [0x11; 32];
const WAIT_TIMEOUT: Duration = Duration::from_secs(20);

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
            "chat-ffi-sync-{}-{nanos}-{counter}",
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

fn wait_for(what: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
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
        let supervisor = SigningKey::from_bytes(&SUPERVISOR_SEED);
        let supervisor_public_key = hex_encode(&supervisor.verifying_key().to_bytes());
        let mut credential = DeviceCredential {
            account_id: ACCOUNT.into(),
            device_id: device_id.to_owned(),
            device_public_key: hex_encode(&device_key().verifying_key().to_bytes()),
            endpoint_id: "endpoint-ffi-sync".into(),
            credential_version: 1,
            not_before: NOW_S - 10,
            expires_at: NOW_S + 3_600,
            supervisor_public_key: supervisor_public_key.clone(),
            signature: String::new(),
        };
        let signature: Signature = supervisor.sign(&credential.signing_bytes().expect("credential"));
        credential.signature = hex_encode(&signature.to_bytes());
        let mut capability = RegistrationCapability {
            capability_id: "cap-ffi-sync".into(),
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

/// 追加一条应用密文条目（作者是本机设备）。
fn append_entry(client: &NodeClient, group_id: &str, device_id: &str, seed: u8, payload: &[u8]) {
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
        mls_epoch: head.mls_epoch,
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
    other_group_id: String,
    client: NodeClient,
}

impl Fixture {
    fn new() -> Self {
        let dir = FixtureDir::new();
        let device_id = enroll_identity(&dir.database_text());
        let node = start_node();
        let client = node.client(&device_id);
        block_on(client.register_device(&node.registration_request(&device_id)))
            .expect("device registers on the node");

        let group_id = GroupId::from_bytes([51_u8; 32]).encode();
        let other_group_id = GroupId::from_bytes([52_u8; 32]).encode();
        for registered in [&group_id, &other_group_id] {
            let creation = GroupCreation {
                group_id: registered.clone(),
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
        }
        // 只有 group_id 初始化了本地拉取游标（other_group_id 故意不初始化）。
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
            other_group_id,
            client,
        }
    }

    fn open(&self, group_id: &str) -> Result<Arc<SyncFfi>, FfiError> {
        SyncFfi::open(
            self.dir.database_text(),
            master_key_hex(),
            self.node.endpoint_url(),
            NODE_ID.to_owned(),
            self.node.public_key_hex(),
            group_id.to_owned(),
        )
    }
}

/// 用**已知种子**在库里建立持久身份，返回设备 ID。
fn enroll_identity(database: &str) -> String {
    let mut core = ConversationCore::open(database, &master_key_hex()).expect("open core");
    let view = core
        .save_identity_account(IdentityAccountInput {
            display_name: "FFI 同步".to_owned(),
            account_seed_hex: seed_hex(&ACCOUNT_SEED),
            device_seed_hex: seed_hex(&DEVICE_SEED),
            created_at_ms: NOW_S * 1_000,
        })
        .expect("save identity");
    core.close();
    view.device_id
}

#[test]
fn open_is_idle_until_start_and_status_tracks_applied_entries() {
    let fixture = Fixture::new();
    let sync = fixture.open(&fixture.group_id).expect("open sync ffi");
    // open 不自动启动：状态是 idle，累计为 0。
    let idle = sync.status().expect("status");
    assert_eq!(idle.state, "idle");
    assert_eq!(idle.applied_total, 0);
    assert!(idle.last_error_code.is_none());
    assert!(idle.stopped_reason.is_none());
    assert!(sync.is_open());
    assert_eq!(sync.group_id(), fixture.group_id);

    append_entry(&fixture.client, &fixture.group_id, &fixture.device_id, 71, b"auto first");
    append_entry(&fixture.client, &fixture.group_id, &fixture.device_id, 72, b"auto second");

    let started = sync.start().expect("start");
    assert!(started.applied_total <= 2);
    wait_for("applied_total == 2", || {
        sync.status().map(|status| status.applied_total >= 2).unwrap_or(false)
    });
    let running = sync.status().expect("status");
    assert!(running.applied_total >= 2);
    assert!(running.last_error_code.is_none(), "unexpected error: {running:?}");
    assert!(!running.resync_required);
    assert!(running.rounds >= 1);

    // 停止：状态 stopped，原因 stop_requested。
    let stopped = sync.stop().expect("stop");
    assert_eq!(stopped.state, "stopped");
    assert_eq!(stopped.stopped_reason.as_deref(), Some("stop_requested"));
    let applied_at_stop = stopped.applied_total;

    // 停止之后节点继续追加：累计不再增长。
    append_entry(&fixture.client, &fixture.group_id, &fixture.device_id, 73, b"after stop");
    std::thread::sleep(Duration::from_millis(400));
    let after = sync.status().expect("status after stop");
    assert_eq!(after.applied_total, applied_at_stop);
    assert_eq!(after.state, "stopped");
}

#[test]
fn uninitialized_cursor_fails_explicitly_on_open() {
    let fixture = Fixture::new();
    let error = match fixture.open(&fixture.other_group_id) {
        Ok(_) => panic!("open must fail when the pull cursor is not initialized"),
        Err(error) => error,
    };
    assert_eq!(code(error), "INVALID_STATE_TRANSITION");
}

#[test]
fn shutdown_is_idempotent_and_closes_the_object() {
    let fixture = Fixture::new();
    let sync = fixture.open(&fixture.group_id).expect("open sync ffi");
    sync.start().expect("start");
    sync.shutdown();
    assert!(!sync.is_open());
    // 幂等：再关一次不 panic、不改变什么。
    sync.shutdown();
    assert!(!sync.is_open());
    assert_eq!(code(sync.status().expect_err("closed")), "CORE_CLOSED");
    assert_eq!(code(sync.start().expect_err("closed")), "CORE_CLOSED");
    assert_eq!(code(sync.stop().expect_err("closed")), "CORE_CLOSED");
    // 底层库已释放：另一个写者能重新打开。
    let reopened = open_store(
        StoreOptions::new(fixture.dir.database(), KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes(MASTER_KEY),
    );
    assert!(reopened.is_ok(), "the store must be released after shutdown");
    let _ = &fixture.device_id;
}
