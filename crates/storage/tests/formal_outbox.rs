//! STORAGE-014 / STORAGE-015 outbox 租约与结果标记集成测试。
//!
//! 覆盖：到期才领取、租约过期重领、并发/顺序领取的唯一性（CAS 版本）、依赖与取消跳过、
//! 批量条数与载荷字节上界、success/failed/unknown 三态、`unknown` 不推进为完成、
//! 重复调用幂等、冲突/过期租约与畸形输入显式拒绝。

use chat_storage::formal::{
    JobOutcome, JobResult, KeyProvenance, LeasedJob, MAX_CLAIM_BATCH, MAX_CLAIM_BATCH_BYTES,
    MessageContent, MessageOrigin, OUTBOX_LEASE_MS, OUTBOX_RETRY_BASE_MS,
    OUTBOX_UNKNOWN_RECONCILE_MS, PreparedWrite, ProtectedStorageKey, StorageError, Store,
    StoreOptions, ValidatedSendRequest, WriteScope, commit_transaction, open_store,
    persist_prepared_attempt, persist_send_intent,
};
use rusqlite::params;
use std::fs;
use std::path::PathBuf;

const CONVERSATION: &str = "conv-1";
const NOW: i64 = 1_700_000_000_000;

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
    key_bytes: [u8; 32],
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("chat-formal-outbox-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).expect("create fixture directory");
        let path = dir.join("chat.db");
        let mut key_bytes = [0u8; 32];
        key_bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        key_bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Self {
            dir,
            path,
            key_bytes,
        }
    }

    fn key(&self) -> ProtectedStorageKey {
        ProtectedStorageKey::from_bytes(self.key_bytes)
    }

    fn open(&self) -> Store {
        open_store(
            StoreOptions::new(&self.path, KeyProvenance::Raw32),
            self.key(),
        )
        .expect("open formal store")
    }

    fn seed(&self, store: &Store) {
        let connection = store.connection();
        connection
            .execute(
                "INSERT INTO accounts (account_id, public_key, protected_profile) VALUES ('account-1', X'00', X'00')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO device_credentials (device_id, account_id, credential, endpoint_id, status, version)
                 VALUES ('device-1', 'account-1', X'00', 'endpoint-1', 'active', 0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO conversations (conversation_id, kind, coordinator_id, protected_metadata, state)
                 VALUES (?1, 'direct', 'node-1', X'00', 'active')",
                [CONVERSATION],
            )
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn error_code<T>(result: Result<T, StorageError>) -> &'static str {
    match result {
        Ok(_) => panic!("expected an explicit failure"),
        Err(error) => error.code(),
    }
}

fn create_draft(store: &mut Store, operation: &str, text: &str, created_at: i64) -> String {
    let request = ValidatedSendRequest {
        conversation_id: CONVERSATION.to_owned(),
        client_operation_id: operation.to_owned(),
        content: MessageContent::text(text).unwrap(),
        reply_to: None,
        origin: MessageOrigin::UserInput,
        created_at,
    };
    let mut transaction = store
        .begin_transaction(WriteScope::intent(CONVERSATION))
        .unwrap();
    let draft_id = persist_send_intent(&mut transaction, request).unwrap().draft_id;
    commit_transaction(transaction).unwrap();
    draft_id
}

/// 建草稿 + 正式尝试（T2）：准备任务完成，产生 send 任务。
fn prepare_send(store: &mut Store, operation: &str, submission: &str) -> String {
    let draft_id = create_draft(store, operation, "正文", NOW);
    let draft = store.read_draft(&draft_id).unwrap().unwrap();
    let prepared = PreparedWrite {
        draft_id: draft_id.clone(),
        conversation_id: CONVERSATION.to_owned(),
        submission_id: submission.to_owned(),
        mls_epoch: 5,
        immutable_ciphertext: vec![9u8; 64],
        request_hash: draft.request_hash,
        crypto_changes: Vec::new(),
    };
    let mut transaction = store
        .begin_transaction(WriteScope::attempt(CONVERSATION))
        .unwrap();
    persist_prepared_attempt(&mut transaction, prepared).unwrap();
    commit_transaction(transaction).unwrap();
    draft_id
}

fn insert_job(
    store: &Store,
    job_id: &str,
    dedup_key: &str,
    kind: &str,
    payload_ref: &str,
    depends_on: Option<&str>,
    state: &str,
    retry_at: i64,
    cancel_requested: bool,
) {
    store
        .connection()
        .execute(
            "INSERT INTO outbox_jobs (job_id, dedup_key, kind, payload_ref, conversation_id, depends_on,
                                      state, retry_count, retry_at, lease_owner, lease_until,
                                      lease_version, cancel_requested)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, NULL, NULL, 0, ?9)",
            params![
                job_id,
                dedup_key,
                kind,
                payload_ref,
                CONVERSATION,
                depends_on,
                state,
                retry_at,
                i64::from(cancel_requested)
            ],
        )
        .unwrap();
}

fn job_state(store: &Store, job_id: &str) -> (String, Option<String>, Option<i64>, u64, i64) {
    store
        .connection()
        .query_row(
            "SELECT state, lease_owner, lease_until, lease_version, retry_count
               FROM outbox_jobs WHERE job_id = ?1",
            [job_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap()
}

fn attempt_state(store: &Store, submission_id: &str) -> String {
    store
        .connection()
        .query_row(
            "SELECT state FROM message_attempts WHERE submission_id = ?1",
            [submission_id],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn claim_leases_due_jobs_once_and_reclaims_after_expiry() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    fixture.seed(&store);
    let draft_id = create_draft(&mut store, "op-1", "待发", NOW);

    assert!(
        store.claim_outbox_batch(NOW - 1, 4, "worker-a").unwrap().is_empty(),
        "jobs before retry_at must not be claimed"
    );
    let jobs = store.claim_outbox_batch(NOW, 4, "worker-a").unwrap();
    assert_eq!(jobs.len(), 1);
    let job = &jobs[0];
    assert_eq!(job.kind, "prepare");
    assert_eq!(job.payload_ref, draft_id);
    assert_eq!(job.worker, "worker-a");
    assert_eq!(job.lease_version, 1);
    assert_eq!(job.lease_until, NOW + OUTBOX_LEASE_MS);

    // 租约有效期内，第二个 worker 拿不到同一条任务。
    assert!(
        store
            .claim_outbox_batch(NOW + OUTBOX_LEASE_MS - 1, 4, "worker-b")
            .unwrap()
            .is_empty()
    );
    // 崩溃/超时：到期后可以被重新领取，并且版本递增。
    let reclaimed = store
        .claim_outbox_batch(NOW + OUTBOX_LEASE_MS, 4, "worker-b")
        .unwrap();
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].job_id, job.job_id);
    assert_eq!(reclaimed[0].lease_version, 2);
    assert_eq!(reclaimed[0].worker, "worker-b");

    // 迟到旧 worker 不能覆盖新结果。
    let error = store
        .mark_outbox_result_at(
            job,
            JobResult::MailboxAck {
                detail: "late".to_owned(),
            },
            NOW + OUTBOX_LEASE_MS + 1,
        )
        .unwrap_err();
    assert_eq!(error.code(), "JOB_LEASE_CONFLICT");
    let (state, owner, _, version, _) = job_state(&store, &job.job_id);
    assert_eq!(state, "leased");
    assert_eq!(owner.as_deref(), Some("worker-b"));
    assert_eq!(version, 2);
}

#[test]
fn claim_skips_dependencies_and_cancelled_jobs() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    fixture.seed(&store);
    let draft_id = create_draft(&mut store, "op-1", "待发", NOW);
    // 直接读取 T1 写下的准备任务 ID。
    let prepare_job: String = store
        .connection()
        .query_row(
            "SELECT job_id FROM outbox_jobs WHERE dedup_key = ?1",
            [format!("prepare:{draft_id}")],
            |row| row.get(0),
        )
        .unwrap();
    insert_job(
        &store,
        "job-dependent",
        "manual:dependent",
        "prepare",
        &draft_id,
        Some(&prepare_job),
        "pending",
        NOW,
        false,
    );
    insert_job(
        &store,
        "job-cancelled",
        "manual:cancelled",
        "prepare",
        &draft_id,
        None,
        "pending",
        NOW,
        true,
    );

    let jobs = store.claim_outbox_batch(NOW, 8, "worker-a").unwrap();
    assert_eq!(jobs.len(), 1, "only the prepare job is due and unblocked");
    assert_eq!(jobs[0].job_id, prepare_job);

    let state = store
        .mark_outbox_result_at(
            &jobs[0],
            JobResult::FormallyAccepted {
                detail: "prepare done".to_owned(),
            },
            NOW,
        )
        .unwrap();
    assert_eq!(state.state, "complete");

    let unblocked = store.claim_outbox_batch(NOW, 8, "worker-a").unwrap();
    assert_eq!(unblocked.len(), 1);
    assert_eq!(unblocked[0].job_id, "job-dependent");
    // 取消请求的任务永远不领取。
    assert!(
        store
            .claim_outbox_batch(NOW + OUTBOX_LEASE_MS * 100, 8, "worker-b")
            .unwrap()
            .iter()
            .all(|job| job.job_id != "job-cancelled")
    );
}

#[test]
fn claim_bounds_the_batch_by_payload_bytes() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    fixture.seed(&store);
    let payload = vec![0u8; 3 * 1024 * 1024];
    for index in 1..=2 {
        let draft_id = format!("draft-big-{index}");
        store
            .connection()
            .execute(
                "INSERT INTO drafts (draft_id, conversation_id, operation_id, request_hash,
                                     protected_content, state, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6)",
                params![
                    draft_id,
                    CONVERSATION,
                    format!("op-big-{index}"),
                    vec![0u8; 32],
                    payload.clone(),
                    NOW
                ],
            )
            .unwrap();
        insert_job(
            &store,
            &format!("job-big-{index}"),
            &format!("prepare:{draft_id}"),
            "prepare",
            &draft_id,
            None,
            "pending",
            NOW,
            false,
        );
    }
    assert!(payload.len() * 2 > MAX_CLAIM_BATCH_BYTES);
    let first = store.claim_outbox_batch(NOW, 8, "worker-a").unwrap();
    assert_eq!(first.len(), 1, "byte budget must stop the batch");
    assert_eq!(first[0].job_id, "job-big-1");
    let second = store.claim_outbox_batch(NOW, 8, "worker-a").unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].job_id, "job-big-2");
}

#[test]
fn unknown_result_stays_reconcilable_and_never_completes() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    fixture.seed(&store);
    prepare_send(&mut store, "op-1", "submission-1");
    let jobs = store.claim_outbox_batch(NOW, 8, "worker-a").unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].kind, "send");
    let lease = &jobs[0];

    let state = store
        .mark_outbox_result_at(
            lease,
            JobResult::Unknown {
                detail: "connection reset".to_owned(),
            },
            NOW + 100,
        )
        .unwrap();
    assert_eq!(state.outcome, JobOutcome::Unknown);
    assert_eq!(state.state, "leased", "unknown must not become complete");
    assert_eq!(
        state.lease_until,
        Some(NOW + 100 + OUTBOX_UNKNOWN_RECONCILE_MS)
    );
    assert_eq!(state.attempt_state.as_deref(), Some("unknown"));
    let (raw_state, owner, lease_until, version, _) = job_state(&store, &lease.job_id);
    assert_eq!(raw_state, "leased");
    assert_eq!(owner.as_deref(), Some("worker-a"));
    assert_eq!(lease_until, Some(NOW + 100 + OUTBOX_UNKNOWN_RECONCILE_MS));
    assert_eq!(version, lease.lease_version);
    assert_eq!(attempt_state(&store, "submission-1"), "unknown");
    assert_eq!(
        store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM outbox_jobs WHERE kind = 'send' AND state = 'complete'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0,
        "no delivery job may be treated as success"
    );

    // 对账窗口内不能被其它 worker 重领。
    assert!(
        store
            .claim_outbox_batch(NOW + 100 + OUTBOX_UNKNOWN_RECONCILE_MS - 1, 8, "worker-b")
            .unwrap()
            .is_empty()
    );
    // 窗口结束后仍可被重新领取（任务没有被删除）。
    let reopened = store
        .claim_outbox_batch(NOW + 100 + OUTBOX_UNKNOWN_RECONCILE_MS, 8, "worker-b")
        .unwrap();
    assert_eq!(reopened.len(), 1);
    assert_eq!(reopened[0].lease_version, lease.lease_version + 1);

    // 对账得到正式送达后可以完成，attempt 升级为 accepted。
    let completed = store
        .mark_outbox_result_at(
            &reopened[0],
            JobResult::Delivered {
                detail: "delivered receipt".to_owned(),
            },
            NOW + 200 + OUTBOX_UNKNOWN_RECONCILE_MS,
        )
        .unwrap();
    assert_eq!(completed.outcome, JobOutcome::Completed);
    assert_eq!(completed.state, "complete");
    assert_eq!(completed.attempt_state.as_deref(), Some("accepted"));
    assert_eq!(attempt_state(&store, "submission-1"), "accepted");
}

#[test]
fn mailbox_ack_does_not_upgrade_delivery() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    fixture.seed(&store);
    prepare_send(&mut store, "op-1", "submission-1");
    let jobs = store.claim_outbox_batch(NOW, 8, "worker-a").unwrap();
    let lease = &jobs[0];
    let state = store
        .mark_outbox_result_at(
            lease,
            JobResult::MailboxAck {
                detail: "mailbox ack".to_owned(),
            },
            NOW,
        )
        .unwrap();
    assert_eq!(state.state, "complete");
    assert_eq!(
        state.attempt_state.as_deref(),
        Some("submitted"),
        "mailbox ACK must not be promoted to accepted/delivered"
    );
    assert_eq!(attempt_state(&store, "submission-1"), "submitted");
    // 送达投影（messages.send_state）不受本函数影响：这里没有消息行，也没有被伪造。
    let messages: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(messages, 0);
}

#[test]
fn mark_is_idempotent_and_rejects_conflicting_or_stale_results() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    fixture.seed(&store);
    // 两条独立的准备任务：一条用于幂等，一条用于租约冲突。
    create_draft(&mut store, "op-1", "一", NOW);
    let jobs = store.claim_outbox_batch(NOW, 8, "worker-a").unwrap();
    let lease = jobs[0].clone();

    let first = store
        .mark_outbox_result_at(
            &lease,
            JobResult::TransportAccepted {
                detail: "transport ok".to_owned(),
            },
            NOW + 1,
        )
        .unwrap();
    assert!(!first.idempotent);
    assert_eq!(first.state, "complete");
    let version_after_first = store.store_version().unwrap();
    let repeat = store
        .mark_outbox_result_at(
            &lease,
            JobResult::TransportAccepted {
                detail: "transport ok".to_owned(),
            },
            NOW + 2,
        )
        .unwrap();
    assert!(repeat.idempotent, "repeated identical result is idempotent");
    assert_eq!(repeat.state, "complete");
    assert_eq!(
        store.store_version().unwrap(),
        version_after_first,
        "idempotent replay must not write rows"
    );

    // 同一终态上的不同结果类别：显式拒绝，不覆盖既有结果。
    let conflict = store
        .mark_outbox_result_at(
            &lease,
            JobResult::Failed {
                retryable: false,
                detail: "late failure".to_owned(),
            },
            NOW + 3,
        )
        .unwrap_err();
    assert_eq!(conflict.code(), "INVALID_STATE_TRANSITION");

    // 旧版本租约：显式拒绝。
    let mut stale = lease.clone();
    stale.lease_version = stale.lease_version.saturating_sub(1);
    let stale_error = store
        .mark_outbox_result_at(
            &stale,
            JobResult::TransportAccepted {
                detail: "stale".to_owned(),
            },
            NOW + 4,
        )
        .unwrap_err();
    assert_eq!(stale_error.code(), "JOB_LEASE_CONFLICT");

    // 另一个 worker 伪造当前版本：显式拒绝。
    create_draft(&mut store, "op-2", "二", NOW);
    let claimed = store.claim_outbox_batch(NOW, 8, "worker-b").unwrap();
    assert_eq!(claimed.len(), 1);
    let mut forged = claimed[0].clone();
    forged.worker = "worker-evil".to_owned();
    let forged_error = store
        .mark_outbox_result_at(
            &forged,
            JobResult::MailboxAck {
                detail: "forged".to_owned(),
            },
            NOW + 5,
        )
        .unwrap_err();
    assert_eq!(forged_error.code(), "JOB_LEASE_CONFLICT");

    // 已取消的任务不能记录结果。
    insert_job(
        &store,
        "job-cancelled",
        "manual:cancelled",
        "prepare",
        "payload",
        None,
        "cancelled",
        NOW,
        false,
    );
    let fake_lease = LeasedJob {
        job_id: "job-cancelled".to_owned(),
        dedup_key: "manual:cancelled".to_owned(),
        kind: "prepare".to_owned(),
        payload_ref: "payload".to_owned(),
        conversation_id: Some(CONVERSATION.to_owned()),
        depends_on: None,
        retry_count: 0,
        retry_at: NOW,
        worker: "worker-a".to_owned(),
        lease_until: NOW + OUTBOX_LEASE_MS,
        lease_version: 0,
    };
    let cancelled = store
        .mark_outbox_result_at(
            &fake_lease,
            JobResult::MailboxAck {
                detail: "cancelled".to_owned(),
            },
            NOW + 6,
        )
        .unwrap_err();
    assert_eq!(cancelled.code(), "INVALID_STATE_TRANSITION");
}

#[test]
fn retryable_failure_reschedules_with_backoff_and_terminal_failure_stops() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    fixture.seed(&store);
    prepare_send(&mut store, "op-1", "submission-1");
    let jobs = store.claim_outbox_batch(NOW, 8, "worker-a").unwrap();
    let lease = &jobs[0];
    let retried = store
        .mark_outbox_result_at(
            lease,
            JobResult::Failed {
                retryable: true,
                detail: "timeout".to_owned(),
            },
            NOW + 10,
        )
        .unwrap();
    assert_eq!(retried.outcome, JobOutcome::RetryScheduled);
    assert_eq!(retried.state, "pending");
    assert_eq!(retried.retry_count, 1);
    assert_eq!(retried.retry_at, NOW + 10 + OUTBOX_RETRY_BASE_MS);
    assert_eq!(retried.attempt_state.as_deref(), Some("prepared"));
    let (raw_state, owner, lease_until, version, retry_count) = job_state(&store, &lease.job_id);
    assert_eq!(raw_state, "pending");
    assert_eq!(owner, None);
    assert_eq!(lease_until, None);
    assert_eq!(version, lease.lease_version);
    assert_eq!(retry_count, 1);

    // 退避期内不重领；到期后重领，版本递增。
    assert!(
        store
            .claim_outbox_batch(NOW + 10 + OUTBOX_RETRY_BASE_MS - 1, 8, "worker-b")
            .unwrap()
            .is_empty()
    );
    let reclaimed = store
        .claim_outbox_batch(NOW + 10 + OUTBOX_RETRY_BASE_MS, 8, "worker-b")
        .unwrap();
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].lease_version, version + 1);

    // 终态失败：任务 failed，尝试 rejected，且不再被领取。
    let failed = store
        .mark_outbox_result_at(
            &reclaimed[0],
            JobResult::Failed {
                retryable: false,
                detail: "rejected by node".to_owned(),
            },
            NOW + 20,
        )
        .unwrap();
    assert_eq!(failed.outcome, JobOutcome::Failed);
    assert_eq!(failed.state, "failed");
    assert_eq!(failed.retry_count, 2);
    assert_eq!(attempt_state(&store, "submission-1"), "rejected");
    assert!(
        store
            .claim_outbox_batch(NOW + 10_000_000, 8, "worker-c")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn claim_and_mark_reject_malformed_input() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    fixture.seed(&store);
    create_draft(&mut store, "op-1", "待发", NOW);
    assert_eq!(
        error_code(store.claim_outbox_batch(NOW, 0, "worker-a")),
        "INVALID_INPUT"
    );
    assert_eq!(
        error_code(store.claim_outbox_batch(NOW, MAX_CLAIM_BATCH + 1, "worker-a")),
        "INVALID_INPUT"
    );
    assert_eq!(
        error_code(store.claim_outbox_batch(NOW, 4, "bad worker")),
        "INVALID_INPUT"
    );
    assert_eq!(
        error_code(store.claim_outbox_batch(-1, 4, "worker-a")),
        "INVALID_INPUT"
    );

    let jobs = store.claim_outbox_batch(NOW, 4, "worker-a").unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(
        error_code(store.mark_outbox_result_at(
            &jobs[0],
            JobResult::MailboxAck {
                detail: "ok".to_owned()
            },
            -1
        )),
        "INVALID_INPUT"
    );
    let mut bad_id = jobs[0].clone();
    bad_id.job_id = "bad job".to_owned();
    assert_eq!(
        error_code(store.mark_outbox_result_at(
            &bad_id,
            JobResult::MailboxAck {
                detail: "ok".to_owned()
            },
            NOW
        )),
        "INVALID_INPUT"
    );
    let mut missing = jobs[0].clone();
    missing.job_id = "job-missing".to_owned();
    assert_eq!(
        error_code(store.mark_outbox_result_at(
            &missing,
            JobResult::MailboxAck {
                detail: "ok".to_owned()
            },
            NOW
        )),
        "JOB_LEASE_CONFLICT"
    );
}
