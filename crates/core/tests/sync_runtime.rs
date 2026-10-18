//! SYR 包端到端：常驻同步运行时 + 真 axum 节点 + 真正式库。
//!
//! 覆盖：起循环后节点追加条目在超时内自动应用（applied_total=2 且游标前进）；stop() 后不再应用；
//! 节点断开 → state=backoff 且 last_error_code 稳定、恢复后继续应用；Resync 触发 catchup 且不跳过缺口。
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chat_core::node_submitter::NodeClientSubmitter;
use chat_core::send_orchestrator::{
    CryptoEngine, EncryptedPayload, SendOrchestrator, SendResult,
};
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
use chat_crypto::engine::MlsDevice;
use chat_protocol::{
    ControlHead, ConversationId, DeviceId, Digest, ErrorCode, GroupId, OperationId, SubmissionId,
    VerifiedHead,
};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store};
use chat_sync::clock::SystemClock;
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


// ---------------------------------------------------------------- OB 包：outbox 接进常驻循环

/// 真实 OpenMLS 引擎（每次加密都真实执行，用于确认“草稿真的被加密并发出”）。
struct MlsCrypto {
    device: MlsDevice,
}

impl CryptoEngine for MlsCrypto {
    fn epoch(&self, _conversation_id: &str) -> SendResult<u64> {
        self.device
            .epoch()
            .map_err(|_| chat_sync::SyncError::new(ErrorCode::StorageFailure, "mls epoch"))
    }

    fn encrypt(
        &mut self,
        _conversation_id: &str,
        _draft_id: &str,
        body: &str,
    ) -> SendResult<EncryptedPayload> {
        let ciphertext = self
            .device
            .encrypt(body)
            .map_err(|_| chat_sync::SyncError::new(ErrorCode::StorageFailure, "mls encrypt"))?;
        let mls_epoch = self.epoch("")?;
        Ok(EncryptedPayload {
            mls_epoch,
            ciphertext,
        })
    }
}

fn mls_engine() -> MlsCrypto {
    // 必须有真实 MLS 群（否则 epoch 不可用、prepare 会以 STORAGE_FAILURE 失败）。
    let (alice, _bob) = mls_pair();
    MlsCrypto { device: alice }
}

fn mls_pair() -> (MlsDevice, MlsDevice) {
    let mut alice = MlsDevice::new("Alice").expect("alice device");
    let mut bob = MlsDevice::new("Bob").expect("bob device");
    let key_package = bob.key_package().expect("bob key package");
    let welcome = alice
        .create_and_invite(&key_package, &bob)
        .expect("invite bob");
    bob.join(&welcome, &alice).expect("bob joins");
    (alice, bob)
}

/// 提交端口替身：模拟“节点不可达”的**稳定结果**（节点不可达在三态里就是 Unknown，
/// 绝不重发）。真节点仍然用于拉取，因此这仍是“真节点 + 真库”的端到端环境。
struct UnknownSubmitter {
    submits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    queries: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl UnknownSubmitter {
    fn new() -> Self {
        Self {
            submits: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            queries: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

}

impl chat_core::send_orchestrator::NodeSubmitter for UnknownSubmitter {
    fn submit(
        &self,
        _attempt: &chat_core::send_orchestrator::SubmitAttempt,
    ) -> SendResult<chat_core::send_orchestrator::SubmitOutcome> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        Ok(chat_core::send_orchestrator::SubmitOutcome::Unknown {
            detail: "node is unreachable".to_owned(),
        })
    }

    fn query(
        &self,
        _submission_id: &str,
    ) -> SendResult<chat_core::send_orchestrator::SubmitOutcome> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        Ok(chat_core::send_orchestrator::SubmitOutcome::Unknown {
            detail: "node is unreachable".to_owned(),
        })
    }
}

fn conversation() -> String {
    ConversationId::from_bytes([0x11; 32]).encode()
}

fn draft_operation(seed: u8) -> String {
    OperationId::from_bytes([seed; 32]).encode()
}

/// 在正式库里写一条草稿（只做意图，不加密、不调度）；返回 draft_id。
fn create_draft(dir: &FixtureDir, node: &FileNode, group_id: &str, seed: u8) -> String {
    let store = dir.open_store();
    let submitter = NodeClientSubmitter::new(node.client(), group_id.to_owned())
        .expect("fixture submitter");
    let orchestrator = SendOrchestrator::new(store, submitter, mls_engine(), SystemClock, "fixture")
        .expect("fixture orchestrator");
    let mut orchestrator = orchestrator;
    let draft_id = orchestrator
        .begin_send(&conversation(), &draft_operation(seed), "outbox 自动发送消息")
        .expect("begin send");
    let store = orchestrator.into_store();
    drop(store);
    draft_id
}

fn draft_state(dir: &FixtureDir, draft_id: &str) -> Option<String> {
    let store = dir.open_store();
    let state = store
        .read_draft(draft_id)
        .expect("read draft")
        .map(|draft| draft.state);
    drop(store);
    state
}

fn attempt_count(dir: &FixtureDir) -> i64 {
    let store = dir.open_store();
    let connection = store.connection();
    {
        let mut statement = connection
            .prepare("SELECT COUNT(*) FROM message_attempts")
            .expect("prepare count");
        let mut rows = statement.query([]).expect("query count");
        match rows.next().expect("row") {
            Some(row) => row.get::<_, i64>(0).expect("count"),
            None => 0,
        }
    }
}

/// 起一个已注册设备 + 群 + 拉取游标的测试世界。
fn world() -> (FixtureDir, FileNode, ServerGuard, String, NodeClient) {
    let dir = FixtureDir::new();
    let node = FileNode::new(FixtureDir::new());
    let server = node.start_server();
    let setup_client = node.client();
    block_on(setup_client.register_device(&registration_request(
        &node.supervisor,
        &device_id_text(DEVICE_SEED),
        &device_key(DEVICE_SEED),
        "cap-syr-outbox",
    )))
    .expect("device registers");
    let group_id = GroupId::from_bytes([61_u8; 32]).encode();
    block_on(setup_client.create_group(&group_creation(&group_id))).expect("group registers");
    {
        let mut store = dir.open_store();
        initialize_pull_state(&mut store, &group_id, genesis(&group_id)).expect("pull state");
    }
    (dir, node, server, group_id, setup_client)
}

fn outbox_config(
    dir: &FixtureDir,
    node: &FileNode,
    group_id: &str,
    submitter: Option<NodeClientSubmitter>,
) -> SyncRuntimeConfig {
    let mut config = SyncRuntimeConfig::new(
        dir.path(),
        &master_key_hex(),
        node.client(),
        group_id.to_owned(),
    )
    .expect("config")
    .with_policy(fast_policy());
    // 页间隔压到 10ms：测试里会在运行时运行期间追加条目，不能被“未到时机”挡住。
    config.pull_policy = PullPolicy {
        page_interval_ms: 10,
        ..generous_pull_policy()
    };
    if let Some(submitter) = submitter {
        config = config.with_outbox(Box::new(mls_engine()), Box::new(submitter));
    }
    config
}

#[test]
fn outbox_runtime_sends_drafts_without_manual_dispatch() {
    let (dir, node, server, group_id, _setup) = world();
    let first = create_draft(&dir, &node, &group_id, 11);
    let second = create_draft(&dir, &node, &group_id, 12);
    assert_eq!(attempt_count(&dir), 0, "草稿只有意图，没有加密尝试");

    let submitter = NodeClientSubmitter::new(node.client(), group_id.clone()).expect("submitter");
    let config = outbox_config(&dir, &node, &group_id, Some(submitter));
    let runtime = SyncRuntime::start(config).expect("runtime starts");
    let handle = runtime.status_handle();
    // 两个草稿 → 两条 prepare + 两条 send = 4 次 accepted（无需任何手动调度）。
    wait_for("outbox accepted 4 tasks", || {
        handle.view().outbox_accepted_total >= 4
    });
    let status = handle.view();
    assert!(status.outbox_last_error_code.is_none(), "unexpected: {status:?}");
    assert_eq!(status.outbox_unknown_total, 0);
    assert!(status.outbox_next_attempt_at_ms > 0);
    let store = runtime.stop().expect("clean stop");
    drop(store);
    assert_eq!(draft_state(&dir, &first).as_deref(), Some("accepted"));
    assert_eq!(draft_state(&dir, &second).as_deref(), Some("accepted"));
    assert_eq!(attempt_count(&dir), 2, "每条草稿恰好一次加密尝试");
    server.stop();
}

#[test]
fn unreachable_submitter_backs_off_outbox_but_keeps_pulling() {
    let (dir, node, server, group_id, setup_client) = world();
    create_draft(&dir, &node, &group_id, 21);
    // 提交端口模拟“节点不可达”：结果只能是 Unknown（绝不重发）；拉取仍走真节点。
    let submitter = UnknownSubmitter::new();
    let mut config = outbox_config(&dir, &node, &group_id, None);
    config = config.without_outbox();
    config = config.with_outbox(Box::new(mls_engine()), Box::new(submitter));
    let runtime = SyncRuntime::start(config).expect("runtime starts");
    let handle = runtime.status_handle();
    wait_for("outbox marks the send unknown", || {
        handle.view().outbox_unknown_total >= 1
    });
    let after_unknown = handle.view();
    assert_eq!(after_unknown.outbox_accepted_total, 1, "prepare 成功、send 未知");
    let _ = &setup_client;
    // 拉取侧**不受影响**：循环继续跑轮次、没有把 outbox 的错误变成拉取错误、也没有停。
    let rounds_before = handle.view().rounds;
    wait_for("pull keeps making rounds", || {
        handle.view().rounds >= rounds_before + 2
    });
    let live = handle.view();
    assert!(!live.is_stopped(), "outbox 失败不得让运行时停止: {live:?}");
    assert!(
        live.last_error_code.is_none(),
        "outbox 的错误不能污染拉取错误码: {live:?}"
    );
    // outbox 自己的 next_attempt 在推进（独立退避），与拉取各自的 next_attempt 互不影响。
    assert!(
        live.outbox_next_attempt_at_ms >= after_unknown.outbox_next_attempt_at_ms,
        "outbox next attempt must keep moving"
    );
    assert!(live.rounds >= 2);
    let store = runtime.stop().expect("clean stop");
    drop(store);
    // 草稿仍是 queued（结果未知，绝不重发、绝不乐观落 accepted）。
    assert_eq!(
        draft_state(&dir, &node_draft_id(&dir)).as_deref(),
        Some("queued")
    );
    server.stop();
}

/// 测试辅助：库里唯一草稿的 draft_id（上面用例只写一条）。
fn node_draft_id(dir: &FixtureDir) -> String {
    let store = dir.open_store();
    let connection = store.connection();
    {
        let mut statement = connection
            .prepare("SELECT draft_id FROM drafts ORDER BY rowid ASC LIMIT 1")
            .expect("prepare draft id");
        let mut rows = statement.query([]).expect("query draft id");
        match rows.next().expect("row") {
            Some(row) => row.get::<_, String>(0).expect("draft id"),
            None => panic!("draft exists"),
        }
    }
}

#[test]
fn disabled_outbox_leaves_drafts_pending() {
    let (dir, node, server, group_id, _setup) = world();
    let draft = create_draft(&dir, &node, &group_id, 31);
    let submitter = NodeClientSubmitter::new(node.client(), group_id.clone()).expect("submitter");
    let config = outbox_config(&dir, &node, &group_id, Some(submitter)).without_outbox();
    let runtime = SyncRuntime::start(config).expect("runtime starts");
    let handle = runtime.status_handle();
    wait_for("several pull rounds", || handle.view().rounds >= 3);
    let status = handle.view();
    assert_eq!(status.outbox_accepted_total, 0, "outbox 关闭时零任务");
    assert_eq!(status.outbox_unknown_total, 0);
    assert!(status.outbox_last_error_code.is_none(), "关闭不是错误: {status:?}");
    let store = runtime.stop().expect("clean stop");
    drop(store);
    assert_eq!(draft_state(&dir, &draft).as_deref(), Some("queued"));
    assert_eq!(attempt_count(&dir), 0, "关闭时既不加密也不提交");
    server.stop();
}

#[test]
fn stop_halts_outbox_at_a_task_boundary() {
    let (dir, node, server, group_id, _setup) = world();
    create_draft(&dir, &node, &group_id, 41);
    let submitter = UnknownSubmitter::new();
    let mut config = outbox_config(&dir, &node, &group_id, None);
    config = config.with_outbox(Box::new(mls_engine()), Box::new(submitter));
    let runtime = SyncRuntime::start(config).expect("runtime starts");
    let handle = runtime.status_handle();
    wait_for("outbox ran once", || handle.view().outbox_unknown_total >= 1);
    let before_stop = 1;
    assert_eq!(before_stop, 1, "只提交过一次（未知结果不重发）");
    let store = runtime.stop().expect("clean stop");
    drop(store);
    let stopped = handle.view();
    assert!(stopped.is_stopped());
    assert_eq!(stopped.stopped_reason, Some("stop_requested"));
    // 停止之后不再有任何提交（未知任务本就在对账窗口里，这里再确认一次）。
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(before_stop, 1, "只提交过一次（未知结果不重发）");
    assert!(handle.view().is_stopped());
    server.stop();
}


