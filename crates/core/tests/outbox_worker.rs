//! SYNC-002：outbox 批执行器端到端测试（真 axum 节点 + 真加密库 + 真 outbox 租约）。
//!
//! 覆盖：三个草稿一轮全 accepted、其中一个被节点拒绝不影响其它、节点不可达→全 unknown 且
//! 零重发、重复执行零提交、任务被其它 worker 持有→deferred、stop 在任务边界打断。
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::node_submitter::{NodeClientSubmitter, SubmitterMetrics};
use chat_core::outbox_worker::{
    OUTBOX_BATCH_LIMIT, OutboxStopReason, run_outbox_once, run_outbox_once_at,
};
use chat_core::send_orchestrator::{CryptoEngine, EncryptedPayload, SendOrchestrator, SendResult};
use chat_crypto::engine::MlsDevice;
use chat_node_api::build_router;
use chat_node_api::state::{AppState, FixedClock as NodeFixedClock};
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_node_services::canonical::hex_encode;
use chat_node_services::group::{GroupCandidate, GroupCreation, GroupMemberSpec, KIND_APPLICATION};
use chat_node_services::registration::{
    DeviceCredential, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest, RegistrationCapability,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use chat_protocol::{ConversationId, DeviceId, ErrorCode, GroupId, OperationId};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store};
use chat_sync::clock::FixedClock;
use ed25519_dalek::{Signature, Signer, SigningKey};

const NOW_MS: i64 = 1_700_000_000_000;
const NOW_S: u64 = 1_700_000_000;
const WORKER: &str = "outbox-worker";
const OTHER_WORKER: &str = "other-worker";
const BODY: &str = "outbox 批执行消息 v1";
const ACCOUNT: &str = "acct-outbox";
const NODE_ID: &str = "node-outbox-test";

fn conversation() -> String {
    ConversationId::from_bytes([0x11; 32]).encode()
}

fn operation(seed: u8) -> String {
    OperationId::from_bytes([seed; 32]).encode()
}

// ---------------------------------------------------------------- 真实节点

struct TestNode {
    address: String,
    node_public_key: [u8; 32],
    supervisor: SigningKey,
}

fn start_node() -> TestNode {
    let service = NodeService::open_in_memory(NodeServiceConfig {
        node_id: NODE_ID.into(),
        signing_seed: [7_u8; 32],
        policy: NodeAuthPolicy::default(),
    })
    .expect("node service opens");
    let node_public_key = service.node_public_key();
    let clock = Arc::new(NodeFixedClock::new(NOW_S));
    let state = AppState::new(service, clock);
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
            endpoint_id: "endpoint-outbox".into(),
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

// ---------------------------------------------------------------- 临时库与真实引擎

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
            "chat-core-outbox-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn open(&self) -> Store {
        open_store(
            StoreOptions::new(self.0.join("chat.db"), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes([0x5a; 32]),
        )
        .expect("open formal store")
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct MlsCrypto {
    device: MlsDevice,
    encryptions: Arc<AtomicUsize>,
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
        self.encryptions.fetch_add(1, Ordering::SeqCst);
        let ciphertext = self
            .device
            .encrypt(body)
            .map_err(|_| chat_sync::SyncError::new(ErrorCode::StorageFailure, "mls encrypt"))?;
        let mls_epoch = self
            .device
            .epoch()
            .map_err(|_| chat_sync::SyncError::new(ErrorCode::StorageFailure, "mls epoch"))?;
        Ok(EncryptedPayload {
            mls_epoch,
            ciphertext,
        })
    }
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

// ---------------------------------------------------------------- 夹具

#[derive(Clone, Copy, Debug)]
enum NodeTarget {
    Real,
    Unreachable,
}

struct Harness {
    orchestrator: SendOrchestrator<NodeClientSubmitter, MlsCrypto, FixedClock>,
    metrics: SubmitterMetrics,
    encryptions: Arc<AtomicUsize>,
    device_id: String,
    group_id: String,
    now_ms: i64,
    // 最后声明：先关存储句柄再删目录（Windows 不能删被打开的数据库）。
    node: TestNode,
    _dir: FixtureDir,
}

impl Harness {
    fn start(target: NodeTarget) -> Self {
        let dir = FixtureDir::new();
        let node = start_node();
        let device_id = DeviceId::from_bytes([21_u8; 32]).encode();
        let key = SigningKey::from_bytes(&[21_u8; 32]);
        let setup = node.client(&device_id, &key);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("setup runtime");
        runtime.block_on(async {
            setup
                .register_device(&node.registration_request(&device_id, &key, "cap-outbox"))
                .await
                .expect("device registers");
            let group_id = GroupId::from_bytes([31_u8; 32]).encode();
            let creation = GroupCreation {
                group_id,
                owner_account_id: ACCOUNT.into(),
                initial_control_head: hex_encode(&[0x11_u8; 32]),
                policy_version: 1,
                mls_epoch: 0,
                members: vec![GroupMemberSpec {
                    device_id: device_id.clone(),
                    role: "owner".into(),
                }],
            };
            setup.create_group(&creation).await.expect("group created");
        });
        let group_id = GroupId::from_bytes([31_u8; 32]).encode();
        let submitter_client = match target {
            NodeTarget::Real => node.client(&device_id, &key),
            NodeTarget::Unreachable => {
                let endpoint = NodeEndpoint::new("http://127.0.0.1:1", NODE_ID)
                    .expect("endpoint")
                    .with_node_public_key(node.node_public_key);
                NodeClient::new(endpoint, device_id.clone(), key.clone()).expect("dead client")
            }
        };
        let submitter =
            NodeClientSubmitter::new(submitter_client, group_id.clone()).expect("submitter");
        let metrics = submitter.metrics();
        let (alice, _peer) = mls_pair();
        let encryptions = Arc::new(AtomicUsize::new(0));
        let engine = MlsCrypto {
            device: alice,
            encryptions: encryptions.clone(),
        };
        let store = dir.open();
        let orchestrator =
            SendOrchestrator::new(store, submitter, engine, FixedClock::new(NOW_MS), WORKER)
                .expect("orchestrator");
        Self {
            orchestrator,
            metrics,
            encryptions,
            device_id,
            group_id,
            now_ms: NOW_MS,
            node,
            _dir: dir,
        }
    }

    fn begin(&mut self, seed: u8) -> String {
        self.orchestrator
            .begin_send(&conversation(), &operation(seed), BODY)
            .expect("begin send")
    }

    fn run(&mut self) -> chat_core::outbox_worker::OutboxBatchResult {
        let stop = AtomicBool::new(false);
        run_outbox_once_at(&mut self.orchestrator, &stop, self.now_ms, OUTBOX_BATCH_LIMIT)
            .expect("outbox round")
    }

    fn advance(&mut self, delta_ms: i64) {
        self.now_ms = self.now_ms.saturating_add(delta_ms);
        self.orchestrator.clock_mut().set(self.now_ms);
    }

    fn submit_calls(&self) -> u64 {
        self.metrics.submit_calls()
    }

    fn query_calls(&self) -> u64 {
        self.metrics.query_calls()
    }

    fn encryptions(&self) -> usize {
        self.encryptions.load(Ordering::SeqCst)
    }

    fn draft_state(&self, draft_id: &str) -> Option<String> {
        scalar(
            self.orchestrator.store(),
            "SELECT state FROM drafts WHERE draft_id = ?1",
            draft_id,
        )
    }

    fn attempt_count(&self) -> i64 {
        count(
            self.orchestrator.store(),
            "SELECT COUNT(*) FROM message_attempts",
        )
    }

    /// 某草稿的耐久 SubmissionId（prepare 之后才有）。
    fn submission_id_for(&self, draft_id: &str) -> String {
        scalar(
            self.orchestrator.store(),
            "SELECT submission_id FROM message_attempts WHERE draft_id = ?1",
            draft_id,
        )
        .expect("prepared attempt exists")
    }

    /// 先用另一份字节占用这个 SubmissionId：该草稿的真实提交会被节点判为冲突（Rejected）。
    fn consume_submission_id_at_node(&self, submission_id: &str) {
        let key = SigningKey::from_bytes(&[21_u8; 32]);
        let client = self.node.client(&self.device_id, &key);
        let candidate = GroupCandidate {
            protocol_major: 1,
            protocol_minor: 0,
            submission_id: submission_id.to_owned(),
            group_id: self.group_id.clone(),
            author_device_id: self.device_id.clone(),
            kind: KIND_APPLICATION.to_owned(),
            expected_control_head: hex_encode(&[0x11_u8; 32]),
            policy_version: 1,
            mls_epoch: 0,
            encrypted_payload: hex_encode(b"probe payload occupying the id"),
            control_metadata: None,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("probe runtime");
        runtime.block_on(async {
            client
                .append_entry(&candidate)
                .await
                .expect("probe entry is accepted by the real node");
        });
    }

    fn job_states(&self) -> Vec<(String, String)> {
        let store = self.orchestrator.store();
        let connection = store.connection();
        let mut statement = connection
            .prepare("SELECT dedup_key, state FROM outbox_jobs ORDER BY job_id ASC")
            .expect("prepare outbox query");
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query outbox");
        rows.map(|row| row.expect("outbox row")).collect()
    }
}

fn scalar(store: &Store, sql: &str, key: &str) -> Option<String> {
    let connection = store.connection();
    let mut statement = connection.prepare(sql).expect("prepare scalar");
    let mut rows = statement.query([key]).expect("query scalar");
    match rows.next().expect("next row") {
        Some(row) => Some(row.get(0).expect("scalar value")),
        None => None,
    }
}

fn count(store: &Store, sql: &str) -> i64 {
    let connection = store.connection();
    let mut statement = connection.prepare(sql).expect("prepare count");
    let mut rows = statement.query([]).expect("query count");
    match rows.next().expect("next row") {
        Some(row) => row.get(0).expect("count value"),
        None => 0,
    }
}

// ---------------------------------------------------------------- 用例

#[test]
fn three_drafts_are_all_accepted_in_one_batch() {
    let mut harness = Harness::start(NodeTarget::Real);
    let drafts: Vec<String> = (1..=3).map(|seed| harness.begin(seed)).collect();
    assert_eq!(harness.job_states().len(), 3, "三个 prepare 任务");

    let result = harness.run();
    assert_eq!(result.processed, 3, "一轮处理三条（prepare）: {result:?}");
    assert_eq!(result.accepted, 3);
    assert_eq!(result.unknown, 0);
    assert_eq!(result.rejected, 0);
    assert_eq!(result.deferred, 0);
    assert_eq!(result.stopped_reason, None);
    assert_eq!(harness.encryptions(), 3, "每条草稿恰好加密一次");
    assert_eq!(harness.submit_calls(), 0, "prepare 阶段不提交");
    for draft in &drafts {
        assert_eq!(harness.draft_state(draft).as_deref(), Some("queued"));
    }
    assert_eq!(harness.attempt_count(), 3, "三条耐久尝试");

    // 第二轮把三条 send 任务提交到真节点。
    let result = harness.run();
    assert_eq!(result.processed, 3, "二轮处理三条（send）: {result:?}");
    assert_eq!(result.accepted, 3);
    assert_eq!(harness.submit_calls(), 3, "每条尝试恰好提交一次");
    assert_eq!(harness.query_calls(), 0, "没有对账（全部一次成功）");
    for draft in &drafts {
        assert_eq!(harness.draft_state(draft).as_deref(), Some("accepted"));
    }
    assert!(harness
        .job_states()
        .iter()
        .all(|(_, state)| state == "complete"),
        "全部任务完成: {:?}",
        harness.job_states()
    );
}

#[test]
fn rejected_submission_does_not_block_the_others() {
    let mut harness = Harness::start(NodeTarget::Real);
    let first = harness.begin(1);
    let second = harness.begin(2);
    // 第一轮：全部 prepare 成功。
    let prepared = harness.run();
    assert_eq!(prepared.accepted, 2);
    // 让第二条草稿的提交必然被节点拒绝：先用相异字节占用它的 SubmissionId
    // （节点返回 SUBMISSION_ID_CONFLICT → 三态映射为 Rejected）。
    let poisoned = harness.submission_id_for(&second);
    harness.consume_submission_id_at_node(&poisoned);
    assert_ne!(harness.submission_id_for(&first), poisoned);
    let result = harness.run();
    assert_eq!(result.processed, 2, "两条 send 都尝试过");
    assert_eq!(result.accepted, 1, "一条成功");
    assert_eq!(result.rejected, 1, "一条被节点拒绝");
    assert_eq!(result.unknown, 0);
    assert_eq!(result.deferred, 0);
    let states = harness.job_states();
    assert!(states.iter().any(|(_, state)| state == "failed"), "拒绝的任务置 failed: {states:?}");
    assert!(states.iter().filter(|(_, state)| state == "complete").count() >= 3);
}

#[test]
fn unreachable_node_marks_every_send_unknown_and_never_resends() {
    let mut harness = Harness::start(NodeTarget::Unreachable);
    harness.begin(1);
    harness.begin(2);
    // prepare 阶段不需要节点：两条都成功。
    let prepared = harness.run();
    assert_eq!(prepared.accepted, 2);
    // send 阶段节点不可达 → 全部 unknown，且零重发（提交计数只增加一次/条）。
    let result = harness.run();
    assert_eq!(result.processed, 2);
    assert_eq!(result.unknown, 2, "不可达 = 结果未知: {result:?}");
    assert_eq!(result.accepted, 0);
    assert_eq!(harness.submit_calls(), 2, "每条尝试只提交一次");
    // 重复执行：unknown 任务在对账窗口内不可领取 → 零新提交。
    let before = harness.submit_calls();
    let repeat = harness.run();
    assert_eq!(repeat.processed, 0, "对账窗口内不再领取: {repeat:?}");
    assert_eq!(repeat.unknown, 0);
    assert_eq!(harness.submit_calls(), before, "零重发");
}

#[test]
fn repeating_a_finished_batch_submits_nothing() {
    let mut harness = Harness::start(NodeTarget::Real);
    harness.begin(1);
    harness.begin(2);
    harness.run();
    harness.run();
    let submits = harness.submit_calls();
    let encryptions = harness.encryptions();
    assert_eq!(submits, 2);
    // 再跑两轮：没有可领取任务 → 不提交、不加密。
    for _ in 0..2 {
        let result = harness.run();
        assert_eq!(result.processed, 0, "没有任务: {result:?}");
        assert_eq!(result.stopped_reason, None);
    }
    assert_eq!(harness.submit_calls(), submits, "零重复提交");
    assert_eq!(harness.encryptions(), encryptions, "零重复加密");
}

#[test]
fn task_held_by_another_worker_is_deferred() {
    let mut harness = Harness::start(NodeTarget::Real);
    harness.begin(1);
    // 用另一个 worker 领走这条 prepare 任务（有效租约）。
    let leased = harness
        .orchestrator
        .store_mut()
        .claim_outbox_batch(harness.now_ms, 1, OTHER_WORKER)
        .expect("other worker claims");
    assert_eq!(leased.len(), 1);
    let result = harness.run();
    assert_eq!(result.processed, 0, "任务被别人持有: {result:?}");
    assert_eq!(result.deferred, 1, "明确计入 deferred");
    // 租约到期后本 worker 自然接管（既有重领机制）。
    harness.advance(chat_storage::formal::OUTBOX_LEASE_MS + 1);
    let result = harness.run();
    assert_eq!(result.processed, 1, "到期后接管: {result:?}");
    assert_eq!(result.accepted, 1);
}

#[test]
fn stop_request_stops_at_a_task_boundary() {
    let mut harness = Harness::start(NodeTarget::Real);
    harness.begin(1);
    harness.begin(2);
    let stop = AtomicBool::new(true);
    let result = run_outbox_once(&mut harness.orchestrator, &stop).expect("round with stop");
    assert_eq!(result.processed, 0, "一开始就停止: {result:?}");
    assert_eq!(result.stopped_reason, Some(OutboxStopReason::StopRequested));
    assert_eq!(harness.encryptions(), 0, "停止后没有任何加密");
}

