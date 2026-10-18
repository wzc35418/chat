//! CORE-007 + SYNC-003/004 发送编排的端到端测试。
//!
//! 全部使用真实依赖：真实加密库（临时目录 + 主密钥）、真实 OpenMLS 设备加密
//! （对端能解密出原文）、真实 T1/T2 与 outbox 租约/结果落库；只有节点提交是可注入
//! 的脚本端口，且每步都断言"调用次数"证明没有重发/重新加密。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::send_orchestrator::{
    CryptoEngine, DispatchOutcome, EncryptedPayload, NodeSubmitter, PrepareOutcome, SendOrchestrator,
    SendResult, SubmitAttempt, SubmitOutcome,
};
use chat_crypto::engine::MlsDevice;
use chat_protocol::{ConversationId, ErrorCode, OperationId, SubmissionId};
use chat_storage::formal::{
    KeyProvenance, OUTBOX_LEASE_MS, ProtectedStorageKey, Store, StoreOptions, open_store,
};
use chat_sync::clock::{Clock, FixedClock};
use chat_sync::error::SyncError;

const NOW: i64 = 1_700_000_000_000;
const WORKER: &str = "worker-a";
const BODY: &str = "端到端真实密文消息 v1";
const ATTEMPT_CAP: usize = 64;

fn conversation() -> String {
    ConversationId::from_bytes([0x11; 32]).encode()
}

fn operation(seed: u8) -> String {
    OperationId::from_bytes([seed; 32]).encode()
}

// ---------------------------------------------------------------- 存储夹具

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "chat-core-send-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self { dir }
    }

    fn open(&self) -> Store {
        open_store(
            StoreOptions::new(self.dir.join("chat.db"), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes([0x5a; 32]),
        )
        .expect("open formal store")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

// ---------------------------------------------------------------- 脚本化节点端口

#[derive(Clone, Debug)]
enum Scripted {
    Outcome(SubmitOutcome),
    Failure(ErrorCode, &'static str),
}

#[derive(Default)]
struct SubmitterState {
    submits: Vec<SubmitAttempt>,
    queries: Vec<String>,
    script: Vec<Scripted>,
}

#[derive(Clone, Default)]
struct SubmitterProbe {
    state: Arc<Mutex<SubmitterState>>,
}

impl SubmitterProbe {
    fn scripted(script: Vec<Scripted>) -> Self {
        let probe = Self::default();
        probe.state.lock().expect("lock probe").script = script;
        probe
    }

    fn submit_count(&self) -> usize {
        self.state.lock().expect("lock probe").submits.len()
    }

    fn query_count(&self) -> usize {
        self.state.lock().expect("lock probe").queries.len()
    }

    fn submitted(&self) -> Vec<SubmitAttempt> {
        self.state.lock().expect("lock probe").submits.clone()
    }
}

struct ScriptedSubmitter {
    probe: SubmitterProbe,
}

impl NodeSubmitter for ScriptedSubmitter {
    fn submit(&self, attempt: &SubmitAttempt) -> SendResult<SubmitOutcome> {
        let mut state = self.probe.state.lock().expect("lock probe");
        state.submits.push(attempt.clone());
        match state.script.first().cloned() {
            Some(Scripted::Outcome(outcome)) => {
                state.script.remove(0);
                Ok(outcome)
            }
            Some(Scripted::Failure(code, detail)) => {
                state.script.remove(0);
                Err(SyncError::new(code, detail))
            }
            None => Err(SyncError::new(
                ErrorCode::InternalFailure,
                "script has no submit outcome",
            )),
        }
    }

    fn query(&self, submission_id: &str) -> SendResult<SubmitOutcome> {
        let mut state = self.probe.state.lock().expect("lock probe");
        state.queries.push(submission_id.to_owned());
        match state.script.first().cloned() {
            Some(Scripted::Outcome(outcome)) => {
                state.script.remove(0);
                Ok(outcome)
            }
            Some(Scripted::Failure(code, detail)) => {
                state.script.remove(0);
                Err(SyncError::new(code, detail))
            }
            None => Err(SyncError::new(
                ErrorCode::InternalFailure,
                "script has no query outcome",
            )),
        }
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
            .map_err(|_| SyncError::new(ErrorCode::StorageFailure, "mls epoch unavailable"))
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
            .map_err(|_| SyncError::new(ErrorCode::StorageFailure, "mls encryption failed"))?;
        let mls_epoch = self
            .device
            .epoch()
            .map_err(|_| SyncError::new(ErrorCode::StorageFailure, "mls epoch unavailable"))?;
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

// ---------------------------------------------------------------- 编排器夹具

struct Harness {
    _fixture: Fixture,
    orchestrator: SendOrchestrator<ScriptedSubmitter, MlsCrypto, FixedClock>,
    probe: SubmitterProbe,
    encryptions: Arc<AtomicUsize>,
    peer: MlsDevice,
    now: i64,
}

impl Harness {
    fn new(script: Vec<Scripted>) -> Self {
        let fixture = Fixture::new();
        let store = fixture.open();
        let probe = SubmitterProbe::scripted(script);
        let encryptions = Arc::new(AtomicUsize::new(0));
        let (alice, peer) = mls_pair();
        let engine = MlsCrypto {
            device: alice,
            encryptions: encryptions.clone(),
        };
        let orchestrator =
            SendOrchestrator::new(store, ScriptedSubmitter { probe: probe.clone() }, engine, FixedClock::new(NOW), WORKER)
                .expect("orchestrator");
        Self {
            _fixture: fixture,
            orchestrator,
            probe,
            encryptions,
            peer,
            now: NOW,
        }
    }

    fn begin(&mut self, operation_seed: u8) -> String {
        self.orchestrator
            .begin_send(&conversation(), &operation(operation_seed), BODY)
            .expect("begin send")
    }

    fn advance(&mut self, delta_ms: i64) {
        self.now = self.now.saturating_add(delta_ms);
        self.orchestrator.clock_mut().set(self.now);
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

    fn send_job_state(&self, submission_id: &str) -> Option<String> {
        scalar(
            self.orchestrator.store(),
            "SELECT state FROM outbox_jobs WHERE dedup_key = 'send:' || ?1",
            submission_id,
        )
    }

    fn attempt_count(&self) -> i64 {
        count(self.orchestrator.store(), "SELECT COUNT(*) FROM message_attempts")
    }

    fn stored_ciphertext(&self, submission_id: &str) -> Vec<u8> {
        let attempt = self
            .orchestrator
            .store()
            .read_attempt(submission_id)
            .expect("read attempt")
            .expect("attempt exists");
        attempt.immutable_ciphertext
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

fn accepted() -> SubmitOutcome {
    SubmitOutcome::Accepted {
        detail: "accepted".to_owned(),
    }
}

fn unknown() -> SubmitOutcome {
    SubmitOutcome::Unknown {
        detail: "timeout".to_owned(),
    }
}

fn rejected() -> SubmitOutcome {
    SubmitOutcome::Rejected {
        detail: "stale epoch".to_owned(),
    }
}

// ---------------------------------------------------------------- 用例

#[test]
fn begin_send_is_idempotent_and_prepare_encrypts_exactly_once() {
    let mut harness = Harness::new(Vec::new());
    let first = harness.begin(1);
    let second = harness.begin(1);
    assert_eq!(first, second, "same operation ID must reuse the same draft");
    assert_eq!(
        harness.draft_state(&first).as_deref(),
        Some("queued"),
        "a send intent is queued until accepted"
    );

    let prepared = harness.orchestrator.prepare(&first).expect("prepare");
    let PrepareOutcome::Prepared {
        submission_id,
        mls_epoch,
        attempt_index,
        already_prepared,
    } = prepared
    else {
        panic!("expected a prepared attempt, got {prepared:?}");
    };
    assert!(!already_prepared);
    assert_eq!(attempt_index, 0);
    assert_eq!(mls_epoch, 1, "fresh MLS group epoch");
    assert_eq!(harness.encryptions(), 1, "exactly one encryption");

    // 幂等：第二次 prepare 复用同一 SubmissionId，且绝不再次加密。
    let again = harness.orchestrator.prepare(&first).expect("prepare twice");
    assert_eq!(
        again,
        PrepareOutcome::Prepared {
            submission_id: submission_id.clone(),
            mls_epoch,
            attempt_index: 0,
            already_prepared: true,
        }
    );
    assert_eq!(harness.encryptions(), 1, "replays must not re-encrypt");
    assert_eq!(harness.attempt_count(), 1);

    // 密文是真实 MLS 输出：对端设备能解出原文。
    let ciphertext = harness.stored_ciphertext(&submission_id);
    assert!(!ciphertext.is_empty());
    let plaintext = harness
        .peer
        .decrypt(&ciphertext, "Alice")
        .expect("peer decrypts the stored ciphertext");
    assert_eq!(plaintext, BODY);
    // 提交前没有任何提交发生。
    assert_eq!(harness.probe.submit_count(), 0);
    // 准备任务完成、发送任务待发。
    assert_eq!(harness.send_job_state(&submission_id).as_deref(), Some("pending"));
    let prepare_job = scalar(
        harness.orchestrator.store(),
        "SELECT state FROM outbox_jobs WHERE dedup_key = 'prepare:' || ?1",
        &first,
    );
    assert_eq!(prepare_job.as_deref(), Some("complete"));
}

#[test]
fn unknown_result_is_never_resent_and_reconciles_by_submission_id() {
    let mut harness = Harness::new(vec![
        Scripted::Outcome(unknown()),
        Scripted::Outcome(unknown()),
        Scripted::Outcome(accepted()),
    ]);
    let draft = harness.begin(2);
    let PrepareOutcome::Prepared { submission_id, .. } =
        harness.orchestrator.prepare(&draft).expect("prepare")
    else {
        panic!("expected prepare");
    };

    let outcome = harness.orchestrator.dispatch(&draft).expect("dispatch");
    assert_eq!(
        outcome,
        DispatchOutcome::Unknown {
            submission_id: submission_id.clone()
        }
    );
    assert_eq!(harness.probe.submit_count(), 1);
    assert_eq!(harness.attempt_state(&submission_id).as_deref(), Some("unknown"));
    assert_eq!(harness.draft_state(&draft).as_deref(), Some("queued"));

    // 未知结果在租约/对账窗口内再次调度：只查询，绝不重发、绝不重新加密。
    let again = harness.orchestrator.dispatch(&draft).expect("reconcile");
    assert_eq!(
        again,
        DispatchOutcome::Unknown {
            submission_id: submission_id.clone()
        }
    );
    assert_eq!(harness.probe.submit_count(), 1, "unknown must never be resent");
    assert_eq!(harness.probe.query_count(), 1, "unknown is reconciled by ID");
    assert_eq!(harness.encryptions(), 1, "unknown must never be re-encrypted");

    // 对账得到正式接受：任务完成 + 草稿 accepted。
    let accepted = harness.orchestrator.dispatch(&draft).expect("reconcile");
    assert_eq!(
        accepted,
        DispatchOutcome::Accepted {
            submission_id: submission_id.clone(),
            idempotent: false,
        }
    );
    assert_eq!(harness.probe.query_count(), 2);
    assert_eq!(harness.probe.submit_count(), 1);
    assert_eq!(harness.attempt_state(&submission_id).as_deref(), Some("accepted"));
    assert_eq!(harness.send_job_state(&submission_id).as_deref(), Some("complete"));
    assert_eq!(harness.draft_state(&draft).as_deref(), Some("accepted"));
}

#[test]
fn rejected_attempt_rebuilds_with_a_new_submission_id() {
    let mut harness = Harness::new(vec![
        Scripted::Outcome(rejected()),
        Scripted::Outcome(accepted()),
    ]);
    let draft = harness.begin(3);
    let PrepareOutcome::Prepared {
        submission_id: first_submission,
        ..
    } = harness.orchestrator.prepare(&draft).expect("prepare")
    else {
        panic!("expected prepare");
    };
    let outcome = harness.orchestrator.dispatch(&draft).expect("dispatch");
    assert_eq!(
        outcome,
        DispatchOutcome::Rejected {
            submission_id: first_submission.clone()
        }
    );
    assert_eq!(
        harness.attempt_state(&first_submission).as_deref(),
        Some("rejected")
    );
    assert_eq!(
        harness.draft_state(&draft).as_deref(),
        Some("queued"),
        "a rejection must keep the draft for a rebuild"
    );
    assert_eq!(
        harness.send_job_state(&first_submission).as_deref(),
        Some("failed")
    );

    // 拒绝后必须先 prepare 重建，不能直接再次提交。
    let direct = harness.orchestrator.dispatch(&draft).expect_err("rebuild first");
    assert_eq!(direct.code(), ErrorCode::InvalidStateTransition);

    let rebuilt = harness.orchestrator.prepare(&draft).expect("rebuild");
    let PrepareOutcome::Prepared {
        submission_id: second_submission,
        attempt_index,
        already_prepared,
        ..
    } = rebuilt
    else {
        panic!("expected rebuilt attempt, got {rebuilt:?}");
    };
    assert!(!already_prepared, "a rebuild must encrypt again");
    assert_eq!(attempt_index, 1);
    assert_ne!(second_submission, first_submission);
    assert_eq!(harness.encryptions(), 2, "rebuild encrypts exactly once more");
    assert_eq!(harness.attempt_count(), 2);

    // 新密文同样是真实 MLS 输出。
    let ciphertext = harness.stored_ciphertext(&second_submission);
    assert_eq!(
        harness
            .peer
            .decrypt(&ciphertext, "Alice")
            .expect("peer decrypts rebuilt ciphertext"),
        BODY
    );

    let accepted = harness.orchestrator.dispatch(&draft).expect("dispatch rebuilt");
    assert_eq!(
        accepted,
        DispatchOutcome::Accepted {
            submission_id: second_submission.clone(),
            idempotent: false,
        }
    );
    assert_eq!(harness.draft_state(&draft).as_deref(), Some("accepted"));
}

#[test]
fn accepted_result_persists_draft_and_outbox_then_replays_idempotently() {
    let mut harness = Harness::new(vec![Scripted::Outcome(accepted())]);
    let draft = harness.begin(4);
    let PrepareOutcome::Prepared { submission_id, .. } =
        harness.orchestrator.prepare(&draft).expect("prepare")
    else {
        panic!("expected prepare");
    };
    let accepted = harness.orchestrator.dispatch(&draft).expect("dispatch");
    assert_eq!(
        accepted,
        DispatchOutcome::Accepted {
            submission_id: submission_id.clone(),
            idempotent: false,
        }
    );
    assert_eq!(harness.draft_state(&draft).as_deref(), Some("accepted"));
    assert_eq!(harness.attempt_state(&submission_id).as_deref(), Some("accepted"));
    assert_eq!(harness.send_job_state(&submission_id).as_deref(), Some("complete"));
    assert_eq!(harness.probe.submit_count(), 1);

    // 重复 dispatch：幂等返回，不再次提交、不再次加密。
    let replay = harness.orchestrator.dispatch(&draft).expect("replay dispatch");
    assert_eq!(
        replay,
        DispatchOutcome::Accepted {
            submission_id: submission_id.clone(),
            idempotent: true,
        }
    );
    assert_eq!(harness.probe.submit_count(), 1, "accepted must not be resubmitted");
    assert_eq!(harness.encryptions(), 1);

    // 接受之后 prepare 直接报告已接受，不再加密。
    let prepared = harness.orchestrator.prepare(&draft).expect("prepare after accept");
    assert_eq!(
        prepared,
        PrepareOutcome::AlreadyAccepted {
            submission_id: submission_id.clone()
        }
    );
    assert_eq!(harness.encryptions(), 1);
}

#[test]
fn live_lease_from_another_worker_blocks_encryption_and_submission() {
    let mut harness = Harness::new(vec![Scripted::Outcome(accepted())]);
    let draft = harness.begin(5);
    let PrepareOutcome::Prepared { submission_id, .. } =
        harness.orchestrator.prepare(&draft).expect("prepare")
    else {
        panic!("expected prepare");
    };

    // 另一个 worker 抢到发送任务租约（真实 STORAGE-014 领取路径）。
    let leased = harness
        .orchestrator
        .store_mut()
        .claim_outbox_batch(harness.now, 1, "worker-b")
        .expect("worker-b claims the send job");
    assert_eq!(leased.len(), 1);
    assert_eq!(leased[0].payload_ref, submission_id);

    // 有效租约期间：dispatch 与 prepare 都必须等待，不得提交、不得重新加密。
    let dispatch = harness.orchestrator.dispatch(&draft).expect("dispatch waits");
    let DispatchOutcome::WaitingForLease { until_ms, .. } = dispatch else {
        panic!("expected waiting, got {dispatch:?}");
    };
    assert_eq!(until_ms, harness.now + OUTBOX_LEASE_MS);
    assert_eq!(harness.probe.submit_count(), 0);
    let prepare = harness.orchestrator.prepare(&draft).expect("prepare waits");
    assert!(matches!(prepare, PrepareOutcome::WaitingForLease { .. }));

    // 租约到期后可以接管（同 SubmissionId、同字节，不重新加密）。
    harness.advance(OUTBOX_LEASE_MS + 1);
    let accepted = harness.orchestrator.dispatch(&draft).expect("dispatch after expiry");
    assert_eq!(
        accepted,
        DispatchOutcome::Accepted {
            submission_id: submission_id.clone(),
            idempotent: false,
        }
    );
    assert_eq!(harness.probe.submit_count(), 1);
    assert_eq!(harness.encryptions(), 1);
    assert_eq!(
        harness.probe.submitted()[0].ciphertext,
        harness.stored_ciphertext(&submission_id),
        "a resubmit must reuse the durable bytes"
    );
}

#[test]
fn transport_error_keeps_the_draft_and_retries_after_lease_expiry() {
    let mut harness = Harness::new(vec![
        Scripted::Failure(ErrorCode::StaleEpoch, "stale epoch"),
        Scripted::Outcome(accepted()),
    ]);
    let draft = harness.begin(6);
    let PrepareOutcome::Prepared { submission_id, .. } =
        harness.orchestrator.prepare(&draft).expect("prepare")
    else {
        panic!("expected prepare");
    };

    let error = harness.orchestrator.dispatch(&draft).expect_err("stale epoch");
    assert_eq!(error.code(), ErrorCode::StaleEpoch, "port errors keep their code");
    assert_eq!(harness.probe.submit_count(), 1);
    assert_eq!(
        harness.draft_state(&draft).as_deref(),
        Some("queued"),
        "a transport failure must keep the draft"
    );
    assert_eq!(
        harness.attempt_state(&submission_id).as_deref(),
        Some("prepared"),
        "the durable attempt is unchanged"
    );
    assert_eq!(harness.encryptions(), 1, "failures must not re-encrypt");

    // 租约到期后同一 SubmissionId 同字节重试。
    harness.advance(OUTBOX_LEASE_MS + 1);
    let accepted = harness.orchestrator.dispatch(&draft).expect("retry");
    assert_eq!(
        accepted,
        DispatchOutcome::Accepted {
            submission_id: submission_id.clone(),
            idempotent: false,
        }
    );
    assert_eq!(harness.probe.submitted().len(), 2);
    assert_eq!(
        harness.probe.submitted()[0].ciphertext,
        harness.probe.submitted()[1].ciphertext,
        "a retry reuses the same ciphertext"
    );
    assert_eq!(harness.encryptions(), 1);
}

#[test]
fn protocol_errors_and_state_preconditions_are_classified() {
    let mut harness = Harness::new(vec![Scripted::Failure(
        ErrorCode::InvalidSignature,
        "verification failed",
    )]);
    // 非规范 ID / 空正文在写库前就被拒绝。
    let bad_id = harness
        .orchestrator
        .begin_send("conv-1", &operation(7), BODY)
        .expect_err("non-canonical conversation ID");
    assert_eq!(bad_id.code(), ErrorCode::InvalidId);
    let bad_body = harness
        .orchestrator
        .begin_send(&conversation(), &operation(7), "")
        .expect_err("empty body");
    assert_eq!(bad_body.code(), ErrorCode::InvalidTarget);
    // 未准备的草稿不能直接提交。
    let draft = harness.begin(7);
    let unprepared = harness.orchestrator.dispatch(&draft).expect_err("prepare first");
    assert_eq!(unprepared.code(), ErrorCode::InvalidStateTransition);
    // 未知草稿显式失败。
    let missing = harness
        .orchestrator
        .prepare("draft-00000000000000000000000000000000")
        .expect_err("unknown draft");
    assert_eq!(missing.code(), ErrorCode::InvalidTarget);
    // 节点协议错误原样上抛，状态不变。
    let PrepareOutcome::Prepared { submission_id, .. } =
        harness.orchestrator.prepare(&draft).expect("prepare")
    else {
        panic!("expected prepare");
    };
    let error = harness.orchestrator.dispatch(&draft).expect_err("signature failure");
    assert_eq!(error.code(), ErrorCode::InvalidSignature);
    assert_eq!(harness.attempt_state(&submission_id).as_deref(), Some("prepared"));
    assert_eq!(harness.draft_state(&draft).as_deref(), Some("queued"));
}

#[test]
fn rebuild_budget_is_bounded() {
    let mut harness = Harness::new(Vec::new());
    let draft = harness.begin(8);
    // 直接铺满重建预算（等价于连续 64 次拒绝后的状态）。
    let connection = harness.orchestrator.store().connection();
    for index in 0..=ATTEMPT_CAP {
        let mut seed = [0u8; 32];
        seed[0] = u8::try_from(index % 251).unwrap_or(0);
        seed[1] = u8::try_from(index / 251).unwrap_or(0);
        seed[31] = 0x5a;
        let submission = SubmissionId::from_bytes(seed).encode();
        connection
            .execute(
                "INSERT INTO message_attempts
                   (submission_id, origin, draft_id, operation_id, conversation_id, mls_epoch,
                    immutable_ciphertext, candidate_hash, state)
                 VALUES (?1, 'local_application', ?2, NULL, ?3, 0, X'01', ?4, 'rejected')",
                (
                    submission.as_str(),
                    draft.as_str(),
                    conversation().as_str(),
                    [0u8; 32].as_slice(),
                ),
            )
            .expect("insert rejected attempt");
    }
    let error = harness.orchestrator.prepare(&draft).expect_err("rebuild budget");
    assert_eq!(error.code(), ErrorCode::InvalidTarget);
    assert_eq!(harness.encryptions(), 0, "no encryption past the budget");
}

#[test]
fn orchestrator_rejects_malformed_worker_id() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let (alice, _peer) = mls_pair();
    let engine = MlsCrypto {
        device: alice,
        encryptions: Arc::new(AtomicUsize::new(0)),
    };
    let error = SendOrchestrator::new(
        store,
        ScriptedSubmitter {
            probe: SubmitterProbe::default(),
        },
        engine,
        FixedClock::new(NOW),
        "bad worker!",
    )
    .expect_err("worker id must be bounded");
    assert_eq!(error.code(), ErrorCode::InvalidId);
    // 固定时钟的负值也被拒绝（时钟故障不静默继续）。
    let fixture = Fixture::new();
    let store = fixture.open();
    let (alice, _peer) = mls_pair();
    let engine = MlsCrypto {
        device: alice,
        encryptions: Arc::new(AtomicUsize::new(0)),
    };
    let mut orchestrator = SendOrchestrator::new(
        store,
        ScriptedSubmitter {
            probe: SubmitterProbe::default(),
        },
        engine,
        FixedClock::new(0),
        WORKER,
    )
    .expect("orchestrator with a broken clock");
    assert_eq!(orchestrator.clock_mut().now_unix_ms(), 0);
    let error = orchestrator
        .begin_send(&conversation(), &operation(9), BODY)
        .expect_err("broken clock");
    assert_eq!(error.code(), ErrorCode::InvalidTarget);
}
