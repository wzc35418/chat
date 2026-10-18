//! CORE-010 草稿状态 + CORE-003/004 最小事件流的端到端测试。
//!
//! 真实依赖：真实临时目录加密库、真实 OpenMLS 设备（密文来自真实加密）、真实 T1/T2 与
//! outbox 落库；只有节点提交是脚本端口。事件/状态都不含正文，这里用可搜索的正文标记
//! 反证“事件里没有正文”。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::core_events::{
    CoreEvent, MAX_EVENT_BATCH, MAX_EVENT_CAPACITY, SendService,
};
use chat_core::send_orchestrator::{
    CryptoEngine, DispatchOutcome, EncryptedPayload, NodeSubmitter, PrepareOutcome, SendResult,
    SubmitAttempt, SubmitOutcome,
};
use chat_core::service::{DraftStatusView, error_code};
use chat_crypto::engine::MlsDevice;
use chat_protocol::{ConversationId, ErrorCode, OperationId};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store};
use chat_sync::clock::FixedClock;
use chat_sync::error::SyncError;

const NOW: i64 = 1_700_000_000_000;
const WORKER: &str = "worker-events";
const BODY: &str = "事件正文不该出现-SECRET-BODY-v1";

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
            "chat-core-events-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self { dir }
    }

    fn open(&self) -> Store {
        open_store(
            StoreOptions::new(self.dir.join("chat.db"), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes([0x6b; 32]),
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
}

#[derive(Default)]
struct SubmitterState {
    submits: usize,
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

    fn submits(&self) -> usize {
        self.state.lock().expect("lock probe").submits
    }
}

struct ScriptedSubmitter {
    probe: SubmitterProbe,
}

impl NodeSubmitter for ScriptedSubmitter {
    fn submit(&self, _attempt: &SubmitAttempt) -> SendResult<SubmitOutcome> {
        let mut state = self.probe.state.lock().expect("lock probe");
        state.submits += 1;
        match state.script.first().cloned() {
            Some(Scripted::Outcome(outcome)) => {
                state.script.remove(0);
                Ok(outcome)
            }
            None => Err(SyncError::new(
                ErrorCode::InternalFailure,
                "script has no submit outcome",
            )),
        }
    }

    fn query(&self, _submission_id: &str) -> SendResult<SubmitOutcome> {
        let mut state = self.probe.state.lock().expect("lock probe");
        match state.script.first().cloned() {
            Some(Scripted::Outcome(outcome)) => {
                state.script.remove(0);
                Ok(outcome)
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
            ciphertext,
            mls_epoch,
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

// ---------------------------------------------------------------- 服务夹具

struct Harness {
    _fixture: Fixture,
    service: SendService<ScriptedSubmitter, MlsCrypto, FixedClock>,
    probe: SubmitterProbe,
}

impl Harness {
    fn new(script: Vec<Scripted>, capacity: Option<usize>) -> Self {
        let fixture = Fixture::new();
        let store = fixture.open();
        let probe = SubmitterProbe::scripted(script);
        let (alice, _peer) = mls_pair();
        let engine = MlsCrypto {
            device: alice,
            encryptions: Arc::new(AtomicUsize::new(0)),
        };
        let submitter = ScriptedSubmitter {
            probe: probe.clone(),
        };
        let service = match capacity {
            Some(capacity) => SendService::with_event_capacity(
                store,
                submitter,
                engine,
                FixedClock::new(NOW),
                WORKER,
                capacity,
            ),
            None => SendService::new(store, submitter, engine, FixedClock::new(NOW), WORKER),
        }
        .expect("send service");
        Self {
            _fixture: fixture,
            service,
            probe,
        }
    }

    fn begin(&mut self, seed: u8) -> String {
        self.service
            .begin_send(&conversation(), &operation(seed), BODY)
            .expect("begin send")
    }

    fn status(&self, draft_id: &str) -> DraftStatusView {
        self.service
            .get_draft_status(draft_id)
            .expect("draft status")
            .expect("draft exists")
    }

    /// 取出截至当前的**全部**事件（按序号升序）。
    fn all_events(&self) -> Vec<CoreEvent> {
        let batch = self.service.poll_events(0, MAX_EVENT_BATCH as u32).expect("poll");
        assert!(!batch.resync_required, "default capacity must not overflow");
        batch.events
    }
}

fn conversation() -> String {
    ConversationId::from_bytes([0x21; 32]).encode()
}

fn operation(seed: u8) -> String {
    OperationId::from_bytes([seed; 32]).encode()
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

// ---------------------------------------------------------------- CORE-010 状态

#[test]
fn draft_status_tracks_every_lifecycle_step() {
    // 1) queued → prepared → accepted
    let mut harness = Harness::new(vec![Scripted::Outcome(accepted())], None);
    let draft = harness.begin(1);
    let queued = harness.status(&draft);
    assert_eq!(queued.status, "queued");
    assert_eq!(queued.submission_id, None);
    assert_eq!(queued.attempt_index, None);
    assert_eq!(queued.attempts, 0);
    assert!(queued.updated_at_ms > 0);
    assert!(queued.can_retry, "a queued draft can be prepared");
    assert!(queued.can_cancel, "a queued draft can be cancelled");
    assert_eq!(queued.conversation_id, conversation());
    assert_eq!(queued.operation_id, operation(1));

    harness.service.prepare(&draft).expect("prepare");
    let prepared = harness.status(&draft);
    assert_eq!(prepared.status, "prepared");
    assert!(prepared.submission_id.is_some());
    assert_eq!(prepared.attempt_index, Some(0));
    assert_eq!(prepared.attempts, 1);
    assert!(prepared.can_retry);
    assert!(prepared.can_cancel);

    let outcome = harness.service.dispatch(&draft).expect("dispatch");
    assert!(matches!(outcome, DispatchOutcome::Accepted { .. }));
    let accepted_status = harness.status(&draft);
    assert_eq!(accepted_status.status, "accepted");
    assert!(!accepted_status.can_retry, "accepted drafts cannot be retried");
    assert!(!accepted_status.can_cancel, "an accepted message cannot be cancelled");
    assert_eq!(harness.probe.submits(), 1);

    // 2) unknown：只能对账，可发起取消（CancelUnknown）
    let mut harness = Harness::new(vec![Scripted::Outcome(unknown())], None);
    let draft = harness.begin(2);
    harness.service.prepare(&draft).expect("prepare");
    harness.service.dispatch(&draft).expect("dispatch unknown");
    let status = harness.status(&draft);
    assert_eq!(status.status, "unknown");
    assert!(!status.can_retry, "unknown results must be reconciled, not retried");
    assert!(status.can_cancel);
    assert_eq!(status.attempts, 1);

    // 3) rejected：可重建（新 SubmissionId），重建后状态回到 prepared
    let mut harness = Harness::new(vec![Scripted::Outcome(rejected())], None);
    let draft = harness.begin(3);
    harness.service.prepare(&draft).expect("prepare");
    harness.service.dispatch(&draft).expect("dispatch rejected");
    let status = harness.status(&draft);
    assert_eq!(status.status, "rejected");
    assert!(status.can_retry, "a rejected attempt can be rebuilt");
    assert!(status.can_cancel);
    let original_submission = status.submission_id.clone();
    let PrepareOutcome::Prepared {
        submission_id: rebuilt_submission,
        attempt_index: rebuilt_index,
        already_prepared,
        ..
    } = harness.service.retry_draft(&draft).expect("retry")
    else {
        panic!("expected rebuild");
    };
    assert!(!already_prepared, "a rebuild must encrypt again");
    assert_eq!(rebuilt_index, 1);
    assert_ne!(
        Some(rebuilt_submission.as_str()),
        original_submission.as_deref(),
        "a rebuild must use a new SubmissionId"
    );
    let rebuilt = harness.status(&draft);
    assert_eq!(rebuilt.status, "prepared");
    assert_eq!(rebuilt.attempt_index, Some(1));
    assert_eq!(rebuilt.attempts, 2);
    assert_eq!(rebuilt.submission_id.as_deref(), Some(rebuilt_submission.as_str()));

    // 4) cancelled：不可重试、不可再取消
    let mut harness = Harness::new(Vec::new(), None);
    let draft = harness.begin(4);
    harness.service.prepare(&draft).expect("prepare");
    harness.service.cancel_draft(&draft).expect("cancel");
    let status = harness.status(&draft);
    assert_eq!(status.status, "cancelled");
    assert!(!status.can_retry);
    assert!(!status.can_cancel);

    // 5) 未知草稿与畸形 ID
    assert!(
        harness
            .service
            .get_draft_status("draft-00000000000000000000000000000000")
            .expect("unknown draft")
            .is_none()
    );
    let error = harness
        .service
        .get_draft_status("not a draft id")
        .expect_err("malformed draft id");
    assert_eq!(error_code(&error), "INVALID_INPUT");

    // 6) ConversationCore 共用同一投影实现
    let fixture = Fixture::new();
    let store = fixture.open();
    let core = chat_core::service::ConversationCore::open(
        &fixture.dir.join("chat.db").to_string_lossy(),
        "6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b",
    );
    match core {
        Ok(core) => {
            assert!(core.get_draft_status("draft-00000000000000000000000000000000").is_ok());
            core.close();
        }
        Err(error) => {
            // 同一库已被 harness 独占：必须显式 STORAGE_LOCKED，不能静默成功。
            assert_eq!(error_code(&error), "STORAGE_LOCKED");
        }
    }
    let _ = store;
}

// ---------------------------------------------------------------- CORE-003/004 事件流

#[test]
fn events_are_monotonic_and_repeat_polling_is_idempotent() {
    let mut harness = Harness::new(vec![Scripted::Outcome(accepted())], None);
    let empty = harness.service.poll_events(0, 32).expect("empty poll");
    assert!(empty.events.is_empty());
    assert_eq!(empty.next_seq, 0);
    assert!(!empty.resync_required);

    let draft = harness.begin(10);
    let first = harness.service.poll_events(0, 32).expect("first poll");
    assert_eq!(first.next_seq, 2);
    assert_eq!(first.events.len(), 2);
    assert!(matches!(
        &first.events[0],
        CoreEvent::DraftChanged { draft_id, status, .. }
            if draft_id == &draft && status == "queued"
    ));
    assert!(matches!(
        &first.events[1],
        CoreEvent::ConversationChanged { conversation_id }
            if conversation_id == &conversation()
    ));
    // 重复 poll 幂等：同参数同结果，且不改变游标。
    let repeated = harness.service.poll_events(0, 32).expect("repeat poll");
    assert_eq!(repeated, first);
    let idle = harness.service.poll_events(first.next_seq, 32).expect("idle poll");
    assert!(idle.events.is_empty());
    assert_eq!(idle.next_seq, first.next_seq);

    harness.service.prepare(&draft).expect("prepare");
    let second = harness
        .service
        .poll_events(first.next_seq, 32)
        .expect("second poll");
    assert_eq!(second.events.len(), 2, "status really changed → two events");
    assert!(matches!(
        &second.events[0],
        CoreEvent::DraftChanged { status, .. } if status == "prepared"
    ));
    assert_eq!(second.next_seq, 4);

    harness.service.dispatch(&draft).expect("dispatch");
    let third = harness
        .service
        .poll_events(second.next_seq, 32)
        .expect("third poll");
    assert_eq!(third.events.len(), 2);
    assert!(matches!(
        &third.events[0],
        CoreEvent::DraftChanged { status, .. } if status == "accepted"
    ));
    assert_eq!(third.next_seq, 6);

    // 幂等重放：状态没变 → 不刷事件。
    harness.service.dispatch(&draft).expect("replay dispatch");
    let idle = harness.service.poll_events(third.next_seq, 32).expect("idle");
    assert!(idle.events.is_empty(), "an unchanged status must not emit events");
    assert_eq!(idle.next_seq, third.next_seq);

    // 序号严格单调：把三批拼起来检查 1..=6。
    let mut seqs: Vec<u64> = Vec::new();
    let mut cursor = 0u64;
    loop {
        let batch = harness.service.poll_events(cursor, 1).expect("page");
        if batch.events.is_empty() {
            break;
        }
        seqs.push(batch.next_seq);
        cursor = batch.next_seq;
        assert!(seqs.len() < 32, "pagination must terminate");
    }
    assert_eq!(seqs, vec![1, 2, 3, 4, 5, 6]);
}

#[test]
fn overflow_returns_resync_required_instead_of_dropping_silently() {
    let mut harness = Harness::new(vec![Scripted::Outcome(accepted())], Some(4));
    let draft = harness.begin(11); // seq 1,2
    harness.service.prepare(&draft).expect("prepare"); // seq 3,4（缓冲恰好满）

    let full = harness.service.poll_events(0, 32).expect("full poll");
    assert_eq!(full.events.len(), 4);
    assert!(!full.resync_required);

    harness.service.dispatch(&draft).expect("dispatch accepted"); // seq 5,6 → 1,2 被丢弃
    let resync = harness.service.poll_events(0, 32).expect("overflow poll");
    assert!(resync.resync_required);
    assert_eq!(resync.next_seq, 6);
    assert_eq!(resync.events.len(), 1);
    assert!(matches!(
        &resync.events[0],
        CoreEvent::ResyncRequired { .. }
    ));

    // 仍在窗口内的游标可以继续增量读取（不强制 resync）。
    let incremental = harness.service.poll_events(4, 32).expect("incremental poll");
    assert!(!incremental.resync_required);
    assert_eq!(incremental.events.len(), 2);
    assert!(matches!(
        &incremental.events[0],
        CoreEvent::DraftChanged { status, .. } if status == "accepted"
    ));

    // 超前游标（来自别的实例/未来）同样要求 resync，而不是静默返回空。
    let ahead = harness.service.poll_events(99, 32).expect("ahead poll");
    assert!(ahead.resync_required);

    // 有界拒绝：limit 越界、容量越界。
    let error = harness.service.poll_events(0, 0).expect_err("limit 0");
    assert_eq!(error_code(&error), "INVALID_EVENT_LIMIT");
    let error = harness
        .service
        .poll_events(0, MAX_EVENT_BATCH + 1)
        .expect_err("limit too large");
    assert_eq!(error_code(&error), "INVALID_EVENT_LIMIT");
    let fixture = Fixture::new();
    let store = fixture.open();
    let (alice, _peer) = mls_pair();
    let error = SendService::with_event_capacity(
        store,
        ScriptedSubmitter {
            probe: SubmitterProbe::default(),
        },
        MlsCrypto {
            device: alice,
            encryptions: Arc::new(AtomicUsize::new(0)),
        },
        FixedClock::new(NOW),
        WORKER,
        MAX_EVENT_CAPACITY + 1,
    )
    .expect_err("capacity too large");
    assert_eq!(error_code(&error), "INVALID_EVENT_CAPACITY");
}

#[test]
fn events_never_carry_message_content_or_keys() {
    let mut harness = Harness::new(vec![Scripted::Outcome(accepted())], None);
    let draft = harness.begin(12);
    harness.service.prepare(&draft).expect("prepare");
    harness.service.dispatch(&draft).expect("dispatch");

    let events = harness.all_events();
    assert!(!events.is_empty());
    let rendered = format!("{events:?}");
    assert!(
        !rendered.contains("SECRET-BODY"),
        "events must never contain the plaintext body: {rendered}"
    );
    assert!(
        !rendered.contains(BODY),
        "events must never contain the plaintext body"
    );
    // 结构断言：DraftChanged 只有 ID/状态字段，没有正文或密钥字段。
    for event in &events {
        match event {
            CoreEvent::DraftChanged {
                draft_id,
                conversation_id,
                status,
                submission_id,
            } => {
                assert!(!draft_id.is_empty());
                assert!(!conversation_id.is_empty());
                assert!(!status.is_empty());
                let _ = submission_id.as_ref();
            }
            CoreEvent::ConversationChanged { conversation_id } => {
                assert!(!conversation_id.is_empty());
            }
            CoreEvent::ResyncRequired { .. } | CoreEvent::CoreClosed => {}
        }
    }
}

#[test]
fn shutdown_publishes_core_closed_and_rejects_later_calls() {
    let mut harness = Harness::new(Vec::new(), None);
    let draft = harness.begin(13);
    let before = harness.service.poll_events(0, 32).expect("before shutdown");
    harness.service.shutdown();
    assert!(!harness.service.is_open());

    let after = harness
        .service
        .poll_events(before.next_seq, 32)
        .expect("after shutdown");
    assert_eq!(after.events.len(), 1);
    assert!(matches!(&after.events[0], CoreEvent::CoreClosed));

    let error = harness.service.get_draft_status(&draft).expect_err("closed");
    assert_eq!(error_code(&error), "CORE_CLOSED");
    let error = harness
        .service
        .begin_send(&conversation(), &operation(14), BODY)
        .expect_err("closed");
    assert_eq!(error_code(&error), "CORE_CLOSED");
    // 重复 shutdown 幂等：不再追加 CoreClosed。
    harness.service.shutdown();
    let repeated = harness
        .service
        .poll_events(after.next_seq, 32)
        .expect("repeat shutdown");
    assert!(repeated.events.is_empty());
}
