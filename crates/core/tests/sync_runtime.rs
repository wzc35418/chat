//! SYR 包端到端：常驻同步运行时 + 真 axum 节点 + 真正式库。
//!
//! 覆盖：起循环后节点追加条目在超时内自动应用（applied_total=2 且游标前进）；stop() 后不再应用；
//! 节点断开 → state=backoff 且 last_error_code 稳定、恢复后继续应用；Resync 触发 catchup 且不跳过缺口。
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chat_core::sync_runtime::{
    SyncRuntime, SyncRuntimeConfig, SyncRuntimePolicy, SyncState,
};
use chat_core::sync_worker::{
    initialize_pull_state, load_applied_entries, load_pull_state,
};
use chat_node_api::build_router;
use chat_node_api::state::{AppState, FixedClock as NodeFixedClock};
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_node_services::canonical::hex_encode;
use chat_node_services::group::{GroupCandidate, GroupCreation, GroupMemberSpec, KIND_APPLICATION};
use chat_node_services::registration::{
    DeviceCredential, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest, RegistrationCapability,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use chat_protocol::{ControlHead, DeviceId, Digest, GroupId, SubmissionId, VerifiedHead};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store};
use chat_sync::pull::PullPolicy;
use ed25519_dalek::{Signature, Signer, SigningKey};

const NOW_S: u64 = 1_700_000_000;
const ACCOUNT: &str = "acct-core-syr";
const NODE_ID: &str = "node-core-syr";
const NODE_SEED: [u8; 32] = [7_u8; 32];
const MASTER_KEY: [u8; 32] = [0x5a; 32];
const DEVICE_SEED: u8 = 41;
const INITIAL_CONTROL_HEAD: [u8; 32] = [0x11; 32];
const WAIT_TIMEOUT: Duration = Duration::from_secs(20);

fn device_id_text(seed: u8) -> String {
    DeviceId::from_bytes([seed; 32]).encode()
}

fn device_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn submission_text(seed: u8) -> String {
    SubmissionId::from_bytes([seed; 32]).encode()
}

fn master_key_hex() -> String {
    hex_encode(&MASTER_KEY)
}

/// 让异步客户端调用能在同步测试里跑（测试线程自己的运行时；节点在别的线程上）。
fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(future)
}

/// 轮询直到条件成立或超时。
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

// ---------------------------------------------------------------- 临时目录 / 库

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
            "chat-core-sync-runtime-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn path(&self) -> PathBuf {
        self.0.join("chat.db")
    }

    fn open_store(&self) -> Store {
        open_store(
            StoreOptions::new(self.path(), KeyProvenance::Raw32),
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

// ---------------------------------------------------------------- 节点夹具

fn registration_request(
    supervisor: &SigningKey,
    device_id: &str,
    key: &SigningKey,
    capability_id: &str,
) -> RegisterDeviceRequest {
    let supervisor_public_key = hex_encode(&supervisor.verifying_key().to_bytes());
    let mut credential = DeviceCredential {
        account_id: ACCOUNT.into(),
        device_id: device_id.to_owned(),
        device_public_key: hex_encode(&key.verifying_key().to_bytes()),
        endpoint_id: "endpoint-syr".into(),
        credential_version: 1,
        not_before: NOW_S - 10,
        expires_at: NOW_S + 3_600,
        supervisor_public_key: supervisor_public_key.clone(),
        signature: String::new(),
    };
    let signature: Signature = supervisor.sign(&credential.signing_bytes().expect("credential"));
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
    let signature: Signature = supervisor.sign(&capability.signing_bytes().expect("capability"));
    capability.signature = hex_encode(&signature.to_bytes());
    RegisterDeviceRequest {
        account_id: ACCOUNT.into(),
        credential,
        capability,
    }
}

fn group_creation(group_id: &str) -> GroupCreation {
    GroupCreation {
        group_id: group_id.to_owned(),
        owner_account_id: ACCOUNT.into(),
        initial_control_head: hex_encode(&INITIAL_CONTROL_HEAD),
        policy_version: 1,
        mls_epoch: 0,
        members: vec![GroupMemberSpec {
            device_id: device_id_text(DEVICE_SEED),
            role: "owner".into(),
        }],
    }
}

/// 在指定端口上可反复启停的真实节点（同一个库、同一个身份）。
struct FileNode {
    _dir: FixtureDir,
    port: u16,
    db_path: PathBuf,
    public_key: [u8; 32],
    supervisor: SigningKey,
}

impl FileNode {
    fn new(dir: FixtureDir) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve port");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        let public_key = SigningKey::from_bytes(&NODE_SEED)
            .verifying_key()
            .to_bytes();
        let db_path = dir.0.join("node.db");
        Self {
            _dir: dir,
            port,
            db_path,
            public_key,
            supervisor: SigningKey::from_bytes(&[11_u8; 32]),
        }
    }

    fn address(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    fn endpoint(&self) -> NodeEndpoint {
        NodeEndpoint::new(&format!("http://{}", self.address()), NODE_ID)
            .expect("endpoint")
            .with_node_public_key(self.public_key)
    }

    fn client(&self) -> NodeClient {
        NodeClient::new(
            self.endpoint(),
            device_id_text(DEVICE_SEED),
            device_key(DEVICE_SEED),
        )
        .expect("client")
    }

    /// 起服务；返回的 guard 负责优雅关闭（同一端口可再次启动，身份与数据都在库里）。
    fn start_server(&self) -> ServerGuard {
        let service = NodeService::open(
            &self.db_path,
            NodeServiceConfig {
                node_id: NODE_ID.into(),
                signing_seed: NODE_SEED,
                policy: NodeAuthPolicy::default(),
            },
        )
        .expect("node service opens");
        let state = AppState::new(service, Arc::new(NodeFixedClock::new(NOW_S)));
        let router = build_router(state);
        let listener =
            std::net::TcpListener::bind(self.address()).expect("bind the reserved port");
        listener.set_nonblocking(true).expect("nonblocking listener");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let handle = std::thread::spawn(move || {
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
                let _ = axum::serve(listener, router)
                    .with_graceful_shutdown(async move {
                        let _ = shutdown_rx.await;
                    })
                    .await;
            });
        });
        ServerGuard {
            shutdown: Some(shutdown_tx),
            handle: Some(handle),
        }
    }
}

struct ServerGuard {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ServerGuard {
    fn stop(mut self) {
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// 追加一条应用密文条目（作者是本机设备），返回节点回执。
fn append_entry(client: &NodeClient, group_id: &str, submission_seed: u8, payload: &[u8]) {
    let head = block_on(client.get_head(group_id)).expect("node head");
    let candidate = GroupCandidate {
        protocol_major: 1,
        protocol_minor: 0,
        submission_id: submission_text(submission_seed),
        group_id: group_id.to_owned(),
        author_device_id: device_id_text(DEVICE_SEED),
        kind: KIND_APPLICATION.to_owned(),
        expected_control_head: head.control_head.clone(),
        policy_version: head.policy_version,
        mls_epoch: head.mls_epoch,
        encrypted_payload: hex_encode(payload),
        control_metadata: None,
    };
    let receipt = block_on(client.append_entry(&candidate)).expect("entry accepted");
    assert!(receipt.seq >= 1);
}

fn genesis(group_id: &str) -> VerifiedHead {
    VerifiedHead::genesis(
        GroupId::parse(group_id).expect("group id"),
        ControlHead::from_bytes(INITIAL_CONTROL_HEAD),
        1,
        0,
    )
}

fn fast_policy() -> SyncRuntimePolicy {
    SyncRuntimePolicy {
        round_wait_min_ms: 20,
        round_wait_max_ms: 500,
        max_failures: 32,
    }
}

fn generous_pull_policy() -> PullPolicy {
    PullPolicy {
        failure_budget: 32,
        ..PullPolicy::DEFAULT
    }
}

// ---------------------------------------------------------------- 用例

#[test]
fn runtime_applies_appended_entries_within_the_deadline() {
    let dir = FixtureDir::new();
    let node = FileNode::new(FixtureDir::new());
    let server = node.start_server();
    let setup_client = node.client();
    block_on(setup_client.register_device(&registration_request(
        &node.supervisor,
        &device_id_text(DEVICE_SEED),
        &device_key(DEVICE_SEED),
        "cap-syr-1",
    )))
    .expect("device registers");
    let group_id = GroupId::from_bytes([51_u8; 32]).encode();
    block_on(setup_client.create_group(&group_creation(&group_id))).expect("group registers");
    {
        let mut store = dir.open_store();
        initialize_pull_state(&mut store, &group_id, genesis(&group_id)).expect("pull state");
    }
    append_entry(&setup_client, &group_id, 71, b"auto first");
    append_entry(&setup_client, &group_id, 72, b"auto second");

    let config = SyncRuntimeConfig::new(
        dir.path(),
        &master_key_hex(),
        node.client(),
        &group_id,
    )
    .expect("config")
    .with_policy(fast_policy());
    let mut config = config;
    config.pull_policy = generous_pull_policy();
    let runtime = SyncRuntime::start(config).expect("runtime starts");
    let handle = runtime.status_handle();
    wait_for("applied_total == 2", || handle.view().applied_total == 2);
    let status = handle.view();
    assert!(status.applied_total >= 2);
    assert!(status.last_error_code.is_none(), "unexpected error: {status:?}");
    assert!(!status.resync_required);

    // 停止后拿回库句柄，验证游标与条目都落盘。
    let store = runtime.stop().expect("clean stop");
    let state = load_pull_state(&store, &group_id).expect("state").expect("init");
    assert_eq!(state.cursor_seq, 2, "cursor advanced to the node head");
    assert_eq!(state.applied_entries, 2);
    let entries = load_applied_entries(&store, &group_id).expect("entries");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].receipt.seq, 1);
    drop(store);
    server.stop();
}

#[test]
fn stop_halts_further_application() {
    let dir = FixtureDir::new();
    let node = FileNode::new(FixtureDir::new());
    let server = node.start_server();
    let setup_client = node.client();
    block_on(setup_client.register_device(&registration_request(
        &node.supervisor,
        &device_id_text(DEVICE_SEED),
        &device_key(DEVICE_SEED),
        "cap-syr-2",
    )))
    .expect("device registers");
    let group_id = GroupId::from_bytes([52_u8; 32]).encode();
    block_on(setup_client.create_group(&group_creation(&group_id))).expect("group registers");
    {
        let mut store = dir.open_store();
        initialize_pull_state(&mut store, &group_id, genesis(&group_id)).expect("pull state");
    }
    append_entry(&setup_client, &group_id, 73, b"before stop");

    let mut config = SyncRuntimeConfig::new(
        dir.path(),
        &master_key_hex(),
        node.client(),
        &group_id,
    )
    .expect("config")
    .with_policy(fast_policy());
    config.pull_policy = generous_pull_policy();
    let runtime = SyncRuntime::start(config).expect("runtime starts");
    let handle = runtime.status_handle();
    wait_for("first entry applied", || handle.view().applied_total == 1);
    let store = runtime.stop().expect("clean stop");
    let stopped = handle.view();
    assert_eq!(stopped.state, SyncState::Stopped);
    assert_eq!(stopped.applied_total, 1);

    // 停止之后再追加：循环已 join，不会再有人拉。
    append_entry(&setup_client, &group_id, 74, b"after stop");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(handle.view().applied_total, 1, "no application after stop");
    let state = load_pull_state(&store, &group_id).expect("state").expect("init");
    assert_eq!(state.cursor_seq, 1);
    assert_eq!(
        load_applied_entries(&store, &group_id).expect("entries").len(),
        1
    );
    drop(store);
    server.stop();
}

#[test]
fn disconnected_node_backs_off_with_a_stable_code_and_then_recovers() {
    let dir = FixtureDir::new();
    let node = FileNode::new(FixtureDir::new());
    let group_id = GroupId::from_bytes([53_u8; 32]).encode();
    {
        // 先在线完成注册与建群（同一库、同一身份），再断开。
        let server = node.start_server();
        let setup_client = node.client();
        block_on(setup_client.register_device(&registration_request(
            &node.supervisor,
            &device_id_text(DEVICE_SEED),
            &device_key(DEVICE_SEED),
            "cap-syr-3",
        )))
        .expect("device registers");
        block_on(setup_client.create_group(&group_creation(&group_id)))
            .expect("group registers");
        {
            let mut store = dir.open_store();
            initialize_pull_state(&mut store, &group_id, genesis(&group_id)).expect("pull state");
        }
        server.stop();
    }

    let mut config = SyncRuntimeConfig::new(
        dir.path(),
        &master_key_hex(),
        node.client(),
        &group_id,
    )
    .expect("config")
    .with_policy(fast_policy());
    config.pull_policy = generous_pull_policy();
    let runtime = SyncRuntime::start(config).expect("runtime starts");
    let handle = runtime.status_handle();
    wait_for("backoff after disconnect", || {
        let status = handle.view();
        status.last_error_code == Some("NODE_UNAVAILABLE") && status.state == SyncState::Backoff
    });

    // 节点恢复（同一端口、同一库）：循环应自动继续并应用新条目。
    let server = node.start_server();
    let setup_client = node.client();
    append_entry(&setup_client, &group_id, 75, b"after reconnect one");
    append_entry(&setup_client, &group_id, 76, b"after reconnect two");
    wait_for("recovery applies the entries", || {
        handle.view().applied_total == 2
    });
    let status = handle.view();
    assert!(status.last_error_code.is_none());
    let store = runtime.stop().expect("clean stop");
    let state = load_pull_state(&store, &group_id).expect("state").expect("init");
    assert_eq!(state.cursor_seq, 2);
    assert_eq!(
        load_applied_entries(&store, &group_id).expect("entries").len(),
        2
    );
    drop(store);
    server.stop();
}

#[test]
fn resync_runs_catchup_and_never_skips_the_gap() {
    let dir = FixtureDir::new();
    let node = FileNode::new(FixtureDir::new());
    let server = node.start_server();
    let setup_client = node.client();
    block_on(setup_client.register_device(&registration_request(
        &node.supervisor,
        &device_id_text(DEVICE_SEED),
        &device_key(DEVICE_SEED),
        "cap-syr-4",
    )))
    .expect("device registers");
    let group_id = GroupId::from_bytes([54_u8; 32]).encode();
    block_on(setup_client.create_group(&group_creation(&group_id))).expect("group registers");
    {
        // 本地游标声称到 seq 3，而节点只有 1 条：决策核心会要求 Resync，追赶计划返回冲突。
        let mut store = dir.open_store();
        let ahead = VerifiedHead {
            group_id: GroupId::parse(&group_id).expect("group"),
            control_head: ControlHead::from_bytes(INITIAL_CONTROL_HEAD),
            policy_version: 1,
            mls_epoch: 0,
            log_sequence: 3,
            log_hash: Digest::from_bytes([0x33; 32]),
        };
        initialize_pull_state(&mut store, &group_id, ahead).expect("pull state");
    }
    append_entry(&setup_client, &group_id, 77, b"node has this");

    let mut config = SyncRuntimeConfig::new(
        dir.path(),
        &master_key_hex(),
        node.client(),
        &group_id,
    )
    .expect("config")
    .with_policy(fast_policy());
    config.pull_policy = generous_pull_policy();
    let runtime = SyncRuntime::start(config).expect("runtime starts");
    let handle = runtime.status_handle();
    wait_for("resync is reported", || handle.view().resync_required);
    wait_for("catchup conflict stops the loop", || {
        let status = handle.view();
        status.state == SyncState::Stopped && status.last_error_code == Some("LOG_CONFLICT")
    });
    let status = handle.view();
    assert_eq!(status.applied_total, 0, "a gap is never skipped");
    assert!(status.resync_required);
    let store = runtime.stop().expect("clean stop");
    let state = load_pull_state(&store, &group_id).expect("state").expect("init");
    assert_eq!(state.cursor_seq, 3, "cursor is not silently rolled back");
    assert!(load_applied_entries(&store, &group_id).expect("entries").is_empty());
    drop(store);
    server.stop();
}
