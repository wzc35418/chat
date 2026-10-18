//! 正式存储的真实进程终止（abort）崩溃断点测试。
//!
//! 子进程在 T1/T2 的断点上调用 std::process::abort()，父进程重新打开同一个文件库并
//! 断言：要么旧完整状态，要么新完整状态，绝不出现半条消息；随后恢复流程只核对既有
//! 字节、不重新加密。

use chat_storage::formal::{
    CryptoRecordChange, KeyProvenance, MessageContent, MessageOrigin, PreparedWrite,
    ProtectedStorageKey, StoreOptions, ValidatedSendRequest, WriteScope, commit_transaction,
    open_store, persist_prepared_attempt, persist_send_intent, recover_incomplete_writes,
};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const CONVERSATION: &str = "conv-crash";

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
    key_bytes: [u8; 32],
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("chat-crash-{}", uuid::Uuid::new_v4()));
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

    fn key_hex(&self) -> String {
        let mut hex = String::with_capacity(64);
        for byte in self.key_bytes {
            hex.push_str(&format!("{byte:02x}"));
        }
        hex
    }

    fn open(&self) -> chat_storage::formal::Store {
        open_store(
            StoreOptions::new(&self.path, KeyProvenance::Raw32),
            self.key(),
        )
        .expect("open formal store")
    }

    fn seed(&self, store: &chat_storage::formal::Store) {
        store
            .connection()
            .execute(
                "INSERT INTO accounts (account_id, public_key, protected_profile) VALUES ('account-1', X'00', X'00')",
                [],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO device_credentials (device_id, account_id, credential, endpoint_id, status, version)
                 VALUES ('device-1', 'account-1', X'00', 'endpoint-1', 'active', 0)",
                [],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO conversations (conversation_id, kind, coordinator_id, protected_metadata, state)
                 VALUES (?1, 'direct', 'node-1', X'00', 'active')",
                [CONVERSATION],
            )
            .unwrap();
    }

    fn commit_draft(&self, operation: &str, text: &str) -> String {
        let mut store = self.open();
        self.seed(&store);
        let mut transaction = store
            .begin_transaction(WriteScope::intent(CONVERSATION))
            .unwrap();
        let record = persist_send_intent(
            &mut transaction,
            ValidatedSendRequest {
                conversation_id: CONVERSATION.to_owned(),
                client_operation_id: operation.to_owned(),
                content: MessageContent::text(text).unwrap(),
                reply_to: None,
                origin: MessageOrigin::UserInput,
                created_at: 1_700_000_000_000,
            },
        )
        .unwrap();
        commit_transaction(transaction).unwrap();
        record.draft_id
    }

    fn crash(&self, arguments: &[&str]) {
        let output = Command::new(env!("CARGO_BIN_EXE_storage-crash-worker"))
            .arg(arguments[0])
            .arg(&self.path)
            .arg(self.key_hex())
            .args(&arguments[1..])
            .output()
            .expect("run crash worker");
        assert!(
            !output.status.success(),
            "crash worker must not exit successfully"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("STORAGE_CRASH_CHECKPOINT="),
            "worker did not reach a checkpoint: {stderr}"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn intent_abort_before_commit_leaves_no_partial_draft() {
    for checkpoint in ["draft", "prepare-job"] {
        let fixture = Fixture::new();
        {
            let store = fixture.open();
            fixture.seed(&store);
        }
        fixture.crash(&[
            "intent",
            checkpoint,
            CONVERSATION,
            &format!("op-crash-{checkpoint}"),
            "崩溃断点正文",
        ]);

        let mut store = fixture.open();
        let drafts: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM drafts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(drafts, 0, "no half draft after abort at {checkpoint}");
        assert_eq!(store.outbox_jobs().unwrap().len(), 0);
        assert_eq!(store.recovery_pending().markers, 0);
        assert_eq!(store.waiting_drafts().unwrap().len(), 0);
        let report = recover_incomplete_writes(&mut store).unwrap();
        assert_eq!(report.markers_processed, 0);
        assert!(!report.state_corrupted);
    }
}

#[test]
fn attempt_abort_before_commit_leaves_no_partial_message() {
    for checkpoint in ["attempt", "mls", "outbox"] {
        let fixture = Fixture::new();
        let draft_id = fixture.commit_draft(&format!("op-attempt-{checkpoint}"), "崩溃断点尝试");
        let ciphertext_hex = "c0ffee1234";
        let secret = format!("crash-secret-{checkpoint}");
        fixture.crash(&[
            "attempt",
            checkpoint,
            CONVERSATION,
            &draft_id,
            &format!("sub-crash-{checkpoint}"),
            "7",
            ciphertext_hex,
            &secret,
        ]);

        let mut store = fixture.open();
        // 事务整体回滚：没有 attempt、没有发送任务，草稿与准备任务仍完整。
        let attempts: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM message_attempts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(attempts, 0, "no half attempt after abort at {checkpoint}");
        let jobs = store.outbox_jobs().unwrap();
        assert_eq!(jobs.len(), 1, "only the prepare job may survive");
        assert_eq!(jobs[0].kind, "prepare");
        assert_eq!(jobs[0].state, "pending");
        // 只有 T1 的草稿恢复标记，T2 的标记随回滚一起消失。
        assert_eq!(store.recovery_pending().markers, 1);
        let report = recover_incomplete_writes(&mut store).unwrap();
        assert_eq!(report.markers_processed, 1);
        assert_eq!(report.markers_cleared, 1);
        assert_eq!(report.send_jobs_requeued, 0);
        assert!(!report.state_corrupted);

        // 恢复后可以正常准备尝试，且再次恢复不会重造任何东西。
        let draft = store.read_draft(&draft_id).unwrap().unwrap();
        let mut prepared = PreparedWrite {
            draft_id: draft_id.clone(),
            conversation_id: CONVERSATION.to_owned(),
            submission_id: format!("sub-after-{checkpoint}"),
            mls_epoch: 1,
            immutable_ciphertext: b"recovered-ciphertext".to_vec(),
            request_hash: draft.request_hash,
            crypto_changes: vec![CryptoRecordChange::new(
                b"mls/group-state".to_vec(),
                0,
                b"recovered-secret".to_vec(),
            )],
        };
        prepared.crypto_changes[0].record_version = 0;
        let mut transaction = store
            .begin_transaction(WriteScope::attempt(CONVERSATION))
            .unwrap();
        persist_prepared_attempt(&mut transaction, prepared).unwrap();
        commit_transaction(transaction).unwrap();
        let attempts: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM message_attempts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(attempts, 1);
        // 草稿标记已在上一次恢复中清除，此时只剩本次尝试的标记。
        let report = recover_incomplete_writes(&mut store).unwrap();
        assert_eq!(report.markers_cleared, 1);
        assert_eq!(report.send_jobs_requeued, 0);
        let secret = store
            .read_mls_record(CONVERSATION, b"mls/group-state")
            .unwrap()
            .unwrap();
        assert_eq!(secret.as_slice(), b"recovered-secret");
    }
}
