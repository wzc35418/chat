//! O 包端到端：真实节点（axum + node-services）+ 真实 OpenMLS 设备 + 真实加密库 +
//! `SendOrchestrator` + `NodeClientSubmitter`。
//!
//! 节点跑在自己的线程/运行时上，提交端口用独立线程把同步调用桥到异步客户端，因此测试
//! 线程可以在同步 `dispatch` 里阻塞，而服务端仍能被调度（不会互相等死）。
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::node_submitter::{NodeClientSubmitter, SubmitterMetrics};
use chat_core::send_orchestrator::{
    CryptoEngine, DispatchOutcome, EncryptedPayload, PrepareOutcome, SendOrchestrator, SendResult,
};
use chat_crypto::engine::MlsDevice;
use chat_node_api::build_router;
use chat_node_api::state::{AppState, FixedClock as NodeFixedClock};
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_node_services::canonical::{decode_hex_bytes, hex_encode};
use chat_node_services::group::{GroupCandidate, GroupCreation, GroupMemberSpec, KIND_APPLICATION};
use chat_node_services::registration::{
    DeviceCredential, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest, RegistrationCapability,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use chat_protocol::{ConversationId, DeviceId, ErrorCode, GroupId, OperationId};
use chat_storage::formal::{
    KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store,
};
use chat_sync::clock::FixedClock;
use ed25519_dalek::{Signature, Signer, SigningKey};

const NOW_MS: i64 = 1_700_000_000_000;
const NOW_S: u64 = 1_700_000_000;
const WORKER: &str = "worker-o";
const BODY: &str = "真实节点闭环消息 v1";
const ACCOUNT: &str = "acct-core";
const NODE_ID: &str = "node-core-test";
const OWNER_ROLE: &str = "owner";

fn conversation() -> String {
    ConversationId::from_bytes([0x11; 32]).encode()
}

fn operation(seed: u8) -> String {
    OperationId::from_bytes([seed; 32]).encode()
}

// ---------------------------------------------------------------- 真实节点夹具

struct TestNode {
    address: String,
    node_public_key: [u8; 32],
    supervisor: SigningKey,
}

/// 起一个真实 node-api 服务。服务端占用独立线程 + 独立运行时：测试线程随后会在同步
/// `dispatch` 里阻塞，不能与它共用同一个运行时。
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

    fn credential(&self, device_id: &str, key: &SigningKey) -> DeviceCredential {
        let mut credential = DeviceCredential {
            account_id: ACCOUNT.into(),
            device_id: device_id.to_owned(),
            device_public_key: hex_encode(&key.verifying_key().to_bytes()),
            endpoint_id: "endpoint-core".into(),
            credential_version: 1,
            not_before: NOW_S - 10,
            expires_at: NOW_S + 3_600,
            supervisor_public_key: hex_encode(&self.supervisor.verifying_key().to_bytes()),
            signature: String::new(),
        };
        let signature: Signature = self
            .supervisor
            .sign(&credential.signing_bytes().expect("credential bytes"));
        credential.signature = hex_encode(&signature.to_bytes());
        credential
    }

    fn capability(&self, capability_id: &str, device_id: &str) -> RegistrationCapability {
        let mut capability = RegistrationCapability {
            capability_id: capability_id.to_owned(),
            account_id: ACCOUNT.into(),
            device_id: device_id.to_owned(),
            purpose: REGISTRATION_CAPABILITY_PURPOSE.into(),
            issued_at: NOW_S - 10,
            expires_at: NOW_S + 600,
            revocation_version: 1,
            supervisor_public_key: hex_encode(&self.supervisor.verifying_key().to_bytes()),
            signature: String::new(),
        };
        let signature: Signature = self
            .supervisor
            .sign(&capability.signing_bytes().expect("capability bytes"));
        capability.signature = hex_encode(&signature.to_bytes());
        capability
    }

    fn registration_request(
        &self,
        device_id: &str,
        key: &SigningKey,
        capability_id: &str,
    ) -> RegisterDeviceRequest {
        RegisterDeviceRequest {
            account_id: ACCOUNT.into(),
            credential: self.credential(device_id, key),
            capability: self.capability(capability_id, device_id),
        }
    }
}

// ---------------------------------------------------------------- 临时加密库夹具

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
            "chat-core-node-submit-{}-{nanos}-{counter}",
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

// ---------------------------------------------------------------- 真实 OpenMLS 引擎

struct MlsCrypto {
    device: MlsDevice,
    encryptions: Arc<AtomicUsize>,
}

impl CryptoEngine for MlsCrypto {
    fn epoch(&self, _conversation_id: &str) -> SendResult<u64> {
        self.device
            .epoch()
            .map_err(|_| chat_sync::SyncError::new(ErrorCode::StorageFailure, "mls epoch unavailable"))
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
            .map_err(|_| chat_sync::SyncError::new(ErrorCode::StorageFailure, "mls encryption failed"))?;
        let mls_epoch = self
            .device
            .epoch()
            .map_err(|_| chat_sync::SyncError::new(ErrorCode::StorageFailure, "mls epoch unavailable"))?;
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
// ---------------------------------------------------------------- 编排夹具

/// 提交端点选择：真节点，或一个必然连不上的端点（“断链 → Unknown”用例）。
#[derive(Clone, Copy, Debug)]
enum SubmitterTarget {
    Real,
    Unreachable,
}

struct Harness {
    orchestrator: SendOrchestrator<NodeClientSubmitter, MlsCrypto, FixedClock>,
    probe: NodeClient,
    node: TestNode,
    device_id: String,
    group_id: String,
    peer: MlsDevice,
    encryptions: Arc<AtomicUsize>,
    metrics: SubmitterMetrics,
    // 最后声明：先关存储句柄再删目录（Windows 上不能删被打开的数据库）。
    _dir: FixtureDir,
}

impl Harness {
    /// 起节点 → 注册设备 → 建群 → 装编排器（提交端口指向 target）。
    async fn start(target: SubmitterTarget) -> Self {
        let dir = FixtureDir::new();
        let node = start_node();
        let device_id = DeviceId::from_bytes([21_u8; 32]).encode();
        let key = SigningKey::from_bytes(&[21_u8; 32]);
        let setup = node.client(&device_id, &key);
        setup
            .register_device(&node.registration_request(&device_id, &key, "cap-o"))
            .await
            .expect("device registers");
        let group_id = GroupId::from_bytes([31_u8; 32]).encode();
        let creation = GroupCreation {
            group_id: group_id.clone(),
            owner_account_id: ACCOUNT.into(),
            initial_control_head: hex_encode(&[0x11_u8; 32]),
            policy_version: 1,
            mls_epoch: 0,
            members: vec![GroupMemberSpec {
                device_id: device_id.clone(),
                role: OWNER_ROLE.into(),
            }],
        };
        setup.create_group(&creation).await.expect("group created");
        let submitter_client = match target {
            SubmitterTarget::Real => node.client(&device_id, &key),
            SubmitterTarget::Unreachable => {
                let endpoint = NodeEndpoint::new("http://127.0.0.1:1", NODE_ID)
                    .expect("endpoint")
                    .with_node_public_key(node.node_public_key);
                NodeClient::new(endpoint, device_id.clone(), key.clone()).expect("dead client")
            }
        };
        let submitter = NodeClientSubmitter::new(submitter_client, group_id.clone())
            .expect("submitter");
        let metrics = submitter.metrics();
        let (alice, peer) = mls_pair();
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
            probe: node.client(&device_id, &key),
            node,
            device_id,
            group_id,
            peer,
            encryptions,
            metrics,
            _dir: dir,
        }
    }

    fn begin(&mut self, seed: u8) -> String {
        self.orchestrator
            .begin_send(&conversation(), &operation(seed), BODY)
            .expect("begin send")
    }

    fn prepare(&mut self, draft_id: &str) -> (String, u64) {
        let prepared = self.orchestrator.prepare(draft_id).expect("prepare");
        let PrepareOutcome::Prepared {
            submission_id,
            mls_epoch,
            ..
        } = prepared
        else {
            panic!("expected a prepared attempt, got {prepared:?}");
        };
        (submission_id, mls_epoch)
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

    fn attempt_state(&self, submission_id: &str) -> Option<String> {
        scalar(
            self.orchestrator.store(),
            "SELECT state FROM message_attempts WHERE submission_id = ?1",
            submission_id,
        )
    }

    fn job_state(&self, submission_id: &str) -> Option<String> {
        scalar(
            self.orchestrator.store(),
            "SELECT state FROM outbox_jobs WHERE dedup_key = 'send:' || ?1",
            submission_id,
        )
    }

    fn attempt_count(&self) -> i64 {
        count(
            self.orchestrator.store(),
            "SELECT COUNT(*) FROM message_attempts",
        )
    }

    fn stored_ciphertext(&self, submission_id: &str) -> Vec<u8> {
        self.orchestrator
            .store()
            .read_attempt(submission_id)
            .expect("read attempt")
            .expect("attempt exists")
            .immutable_ciphertext
    }

    /// 模拟“提交后客户端崩溃、结果没落库”：本地投影变成 unknown，发送任务重新可领取。
    fn mark_result_unknown(&self, submission_id: &str) {
        let connection = self.orchestrator.store().connection();
        connection
            .execute(
                "UPDATE message_attempts SET state = 'unknown' WHERE submission_id = ?1",
                [submission_id],
            )
            .expect("mark attempt unknown");
        connection
            .execute(
                "UPDATE outbox_jobs SET state = 'pending', lease_owner = NULL, lease_until = NULL,
                    retry_at = 0
                  WHERE dedup_key = 'send:' || ?1",
                [submission_id],
            )
            .expect("reset send job");
    }

    async fn node_head(&self) -> chat_node_services::group::SignedLogHead {
        self.probe
            .get_head(&self.group_id)
            .await
            .expect("node head")
    }

    async fn node_log(&self) -> Vec<(String, String)> {
        let page = self
            .probe
            .fetch_log(&self.group_id, 0, 50)
            .await
            .expect("node log");
        page.entries
            .into_iter()
            .map(|entry| {
                (
                    entry.receipt.submission_id,
                    entry.candidate.candidate.encrypted_payload,
                )
            })
            .collect()
    }

}

fn scalar(store: &Store, sql: &str, param: &str) -> Option<String> {
    let connection = store.connection();
    let mut statement = connection.prepare(sql).expect("prepare scalar");
    let rows = statement
        .query_map([param], |row| {
            let value: String = row.get(0)?;
            Ok(value)
        })
        .expect("run scalar");
    let mut found = None;
    for row in rows {
        found = Some(row.expect("scalar row"));
    }
    found
}

fn count(store: &Store, sql: &str) -> i64 {
    let connection = store.connection();
    let mut statement = connection.prepare(sql).expect("prepare count");
    let rows = statement
        .query_map([], |row| {
            let value: i64 = row.get(0)?;
            Ok(value)
        })
        .expect("run count");
    let mut total = 0;
    for row in rows {
        total = row.expect("count row");
    }
    total
}

fn divergent_candidate(
    group_id: &str,
    device_id: &str,
    submission_id: &str,
    head: &chat_node_services::group::SignedLogHead,
    payload: &[u8],
) -> GroupCandidate {
    GroupCandidate {
        protocol_major: 1,
        protocol_minor: 0,
        submission_id: submission_id.to_owned(),
        group_id: group_id.to_owned(),
        author_device_id: device_id.to_owned(),
        kind: KIND_APPLICATION.to_owned(),
        expected_control_head: head.control_head.clone(),
        policy_version: head.policy_version,
        mls_epoch: head.mls_epoch,
        encrypted_payload: hex_encode(payload),
        control_metadata: None,
    }
}
// ---------------------------------------------------------------- 用例

#[tokio::test]
async fn real_node_accepts_the_orchestrated_send_and_persists_every_layer() {
    let mut harness = Harness::start(SubmitterTarget::Real).await;
    let draft = harness.begin(1);
    let (submission_id, epoch) = harness.prepare(&draft);
    assert_eq!(epoch, 1, "real MLS group epoch");

    let outcome = harness.orchestrator.dispatch(&draft).expect("dispatch");
    assert_eq!(
        outcome,
        DispatchOutcome::Accepted {
            submission_id: submission_id.clone(),
            idempotent: false,
        }
    );

    // 本地三层一致：草稿 accepted / attempt accepted / 发送任务 complete。
    assert_eq!(harness.draft_state(&draft).as_deref(), Some("accepted"));
    assert_eq!(
        harness.attempt_state(&submission_id).as_deref(),
        Some("accepted")
    );
    assert_eq!(
        harness.job_state(&submission_id).as_deref(),
        Some("complete")
    );
    assert_eq!(harness.metrics.submit_calls(), 1);
    assert_eq!(harness.metrics.query_calls(), 0);
    assert_eq!(harness.encryptions(), 1, "exactly one encryption");
    assert_eq!(harness.attempt_count(), 1);

    // 节点回执：seq 从 1 开始、绑定本 SubmissionId、由节点签名。
    let receipt = harness.metrics.last_receipt().expect("node receipt");
    assert_eq!(receipt.seq, 1);
    assert_eq!(receipt.submission_id, submission_id);
    assert_eq!(receipt.author_device_id, harness.device_id);
    receipt
        .verify(&harness.node.node_public_key)
        .expect("node-signed append receipt");

    // 节点日志里存的正是本地耐久密文，且对端真实 OpenMLS 设备能解出原文。
    let log = harness.node_log().await;
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].0, submission_id);
    let node_ciphertext = decode_hex_bytes("payload", &log[0].1, 64 * 1024).expect("ciphertext hex");
    assert_eq!(
        node_ciphertext,
        harness.stored_ciphertext(&submission_id),
        "the node stores exactly the durable local ciphertext"
    );
    assert_eq!(
        harness
            .peer
            .decrypt(&node_ciphertext, "Alice")
            .expect("peer decrypts the node-stored ciphertext"),
        BODY
    );

    // 节点 head 前进到 seq 1，并与回执的 log_hash 一致。
    let head = harness.node_head().await;
    assert_eq!(head.seq, 1);
    assert_eq!(head.log_hash, receipt.log_hash);
    head.verify(&harness.node.node_public_key)
        .expect("signed head");
}

#[tokio::test]
async fn repeated_dispatch_after_acceptance_never_resubmits() {
    let mut harness = Harness::start(SubmitterTarget::Real).await;
    let draft = harness.begin(2);
    let (submission_id, _) = harness.prepare(&draft);
    harness.orchestrator.dispatch(&draft).expect("dispatch");
    assert_eq!(harness.metrics.submit_calls(), 1);

    // 重复 dispatch：幂等返回，不再次提交、不再次加密。
    let replay = harness.orchestrator.dispatch(&draft).expect("replay");
    assert_eq!(
        replay,
        DispatchOutcome::Accepted {
            submission_id: submission_id.clone(),
            idempotent: true,
        }
    );
    assert_eq!(harness.metrics.submit_calls(), 1, "accepted must not be resubmitted");
    assert_eq!(harness.metrics.query_calls(), 0);
    assert_eq!(harness.encryptions(), 1);

    // 再 prepare 直接报告已接受。
    assert_eq!(
        harness.orchestrator.prepare(&draft).expect("prepare"),
        PrepareOutcome::AlreadyAccepted {
            submission_id: submission_id.clone()
        }
    );
    assert_eq!(harness.encryptions(), 1);
    // 节点侧仍然只有一条日志。
    assert_eq!(harness.node_log().await.len(), 1);
}

#[tokio::test]
async fn node_conflict_on_divergent_bytes_maps_to_rejected() {
    let mut harness = Harness::start(SubmitterTarget::Real).await;
    let draft = harness.begin(3);
    let (submission_id, _) = harness.prepare(&draft);

    // 先把同一 SubmissionId 的**相异字节**提交给节点（模拟“同 ID 不同密文”的历史尝试）。
    let head = harness.node_head().await;
    let divergent = divergent_candidate(
        &harness.group_id,
        &harness.device_id,
        &submission_id,
        &head,
        b"divergent ciphertext",
    );
    let first = harness
        .probe
        .append_entry(&divergent)
        .await
        .expect("divergent bytes accepted first");
    assert_eq!(first.seq, 1);

    // 编排器用真实密文提交同 ID → 节点 409 → Rejected（不重试、不重新加密）。
    let outcome = harness.orchestrator.dispatch(&draft).expect("dispatch");
    assert_eq!(
        outcome,
        DispatchOutcome::Rejected {
            submission_id: submission_id.clone(),
        }
    );
    assert_eq!(
        harness.attempt_state(&submission_id).as_deref(),
        Some("rejected")
    );
    assert_eq!(
        harness.draft_state(&draft).as_deref(),
        Some("queued"),
        "a rejection keeps the draft for a rebuild"
    );
    assert_eq!(
        harness.job_state(&submission_id).as_deref(),
        Some("failed")
    );
    assert_eq!(harness.metrics.submit_calls(), 1, "a conflict is not retried");
    assert_eq!(harness.encryptions(), 1);

    // 节点日志仍然只有那条相异条目：我们的候选没有被写入。
    let log = harness.node_log().await;
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].1, hex_encode(b"divergent ciphertext"));

    // 拒绝后必须由 prepare 用新 SubmissionId 重建（真实再加密一次）。
    let rebuilt = harness.orchestrator.prepare(&draft).expect("rebuild");
    let PrepareOutcome::Prepared {
        submission_id: next_submission,
        attempt_index,
        already_prepared,
        ..
    } = rebuilt
    else {
        panic!("expected rebuilt attempt, got {rebuilt:?}");
    };
    assert!(!already_prepared);
    assert_eq!(attempt_index, 1);
    assert_ne!(next_submission, submission_id);
    assert_eq!(harness.encryptions(), 2);
}

#[tokio::test]
async fn unreachable_node_maps_to_unknown_and_then_only_reconciles() {
    let mut harness = Harness::start(SubmitterTarget::Unreachable).await;
    let draft = harness.begin(4);
    let (submission_id, _) = harness.prepare(&draft);

    // 第一次提交：连不上节点 → Unknown（绝不当成成功，也不当成拒绝）。
    let outcome = harness.orchestrator.dispatch(&draft).expect("dispatch");
    assert_eq!(
        outcome,
        DispatchOutcome::Unknown {
            submission_id: submission_id.clone(),
        }
    );
    assert_eq!(
        harness.attempt_state(&submission_id).as_deref(),
        Some("unknown")
    );
    assert_eq!(harness.draft_state(&draft).as_deref(), Some("queued"));
    assert_eq!(harness.metrics.submit_calls(), 1);
    assert_eq!(harness.encryptions(), 1);

    // 第二次调度：plan 只允许对账；对账也连不上 → 显式错误上抛，仍不重发。
    let error = harness
        .orchestrator
        .dispatch(&draft)
        .expect_err("reconciliation without the node fails explicitly");
    assert_eq!(error.code(), ErrorCode::NetworkUnavailable);
    assert_eq!(harness.metrics.submit_calls(), 1, "unknown is never resent");
    assert_eq!(harness.metrics.query_calls(), 1);
    assert_eq!(harness.encryptions(), 1, "unknown is never re-encrypted");
    assert_eq!(
        harness.attempt_state(&submission_id).as_deref(),
        Some("unknown")
    );
    assert_eq!(harness.draft_state(&draft).as_deref(), Some("queued"));
}

#[tokio::test]
async fn query_reconciles_accepted_and_reports_unknown_for_missing_submission() {
    let mut harness = Harness::start(SubmitterTarget::Real).await;
    let draft = harness.begin(5);
    let (first, _) = harness.prepare(&draft);
    assert!(matches!(
        harness.orchestrator.dispatch(&draft).expect("dispatch"),
        DispatchOutcome::Accepted { idempotent: false, .. }
    ));

    // 模拟“提交成功但客户端在落库前崩溃”：本地投影 unknown，节点其实已接受。
    harness.mark_result_unknown(&first);
    let reconciled = harness.orchestrator.dispatch(&draft).expect("reconcile");
    assert_eq!(
        reconciled,
        DispatchOutcome::Accepted {
            submission_id: first.clone(),
            idempotent: false,
        }
    );
    assert_eq!(harness.metrics.submit_calls(), 1, "reconciliation must not resend");
    assert_eq!(harness.metrics.query_calls(), 1);
    assert_eq!(harness.draft_state(&draft).as_deref(), Some("accepted"));
    assert_eq!(harness.attempt_state(&first).as_deref(), Some("accepted"));
    assert_eq!(harness.job_state(&first).as_deref(), Some("complete"));

    // 第二个草稿：节点从未收到 → query 只能返回 Unknown（不是 Rejected）。
    let second_draft = harness.begin(6);
    let (second, _) = harness.prepare(&second_draft);
    harness.mark_result_unknown(&second);
    let unknown = harness.orchestrator.dispatch(&second_draft).expect("unknown");
    assert_eq!(
        unknown,
        DispatchOutcome::Unknown {
            submission_id: second.clone(),
        }
    );
    assert_eq!(harness.attempt_state(&second).as_deref(), Some("unknown"));
    assert_eq!(harness.draft_state(&second_draft).as_deref(), Some("queued"));
    assert_eq!(harness.metrics.submit_calls(), 1, "unknown only queries");
    assert_eq!(harness.metrics.query_calls(), 2);

    // 仍然未知：再调度一次也只查询，绝不重发、绝不重新加密。
    let again = harness.orchestrator.dispatch(&second_draft).expect("unknown again");
    assert_eq!(
        again,
        DispatchOutcome::Unknown {
            submission_id: second.clone(),
        }
    );
    assert_eq!(harness.metrics.submit_calls(), 1);
    assert_eq!(harness.metrics.query_calls(), 3);
    assert_eq!(harness.encryptions(), 2, "two drafts, one encryption each");

    // 节点日志只接受过第一条。
    let log = harness.node_log().await;
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].0, first);
}
