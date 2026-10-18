//! 正式加密存储（P0-04 基础）集成测试。
//!
//! 覆盖 STORAGE-001 打开/迁移/锁定、003 事务、006 意图幂等、007 尝试原子性、
//! 009/010 提交/回滚语义、011 恢复，以及“磁盘字节中不出现明文正文或主密钥”“错误密钥
//! 必须拒绝”“篡改密文必须拒绝”的实证要求。

use chat_storage::formal::{
    AttemptRecord, CryptoRecordChange, DraftRecord, KeyProvenance, MessageContent, MessageOrigin,
    PreparedWrite, ProtectedStorageKey, StorageError, Store, StoreOptions, StoreStatus,
    ValidatedSendRequest, WriteScope, commit_transaction, open_store, persist_prepared_attempt,
    persist_send_intent, recover_incomplete_writes, rollback_transaction,
};
use rusqlite::Connection;
use std::fs;
use std::path::PathBuf;

const CONVERSATION: &str = "conv-1";
const CONVERSATION_TWO: &str = "conv-2";

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
    key_bytes: [u8; 32],
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("chat-formal-{}", uuid::Uuid::new_v4()));
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

    fn other_key(&self) -> ProtectedStorageKey {
        let mut other = self.key_bytes;
        other[0] ^= 0xff;
        ProtectedStorageKey::from_bytes(other)
    }

    fn options(&self) -> StoreOptions {
        StoreOptions::new(&self.path, KeyProvenance::Raw32)
    }

    fn open(&self) -> Store {
        open_store(self.options(), self.key()).expect("open formal store")
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
        for conversation in [CONVERSATION, CONVERSATION_TWO] {
            connection
                .execute(
                    "INSERT INTO conversations (conversation_id, kind, coordinator_id, protected_metadata, state)
                     VALUES (?1, 'direct', 'node-1', X'00', 'active')",
                    [conversation],
                )
                .unwrap();
        }
    }

    fn raw(&self) -> Connection {
        Connection::open(&self.path).expect("open raw sqlite connection")
    }

    /// 只在该库没有打开的 Store 时使用（独占锁会阻止第二个连接）。
    fn scalar(&self, sql: &str) -> i64 {
        let connection = self.raw();
        connection.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    fn store_files(&self) -> Vec<(String, Vec<u8>)> {
        let mut files = Vec::new();
        for suffix in ["", "-wal", "-shm"] {
            let path = PathBuf::from(format!("{}{suffix}", self.path.display()));
            if let Ok(bytes) = fs::read(path) {
                files.push((suffix.to_owned(), bytes));
            }
        }
        files
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn rows(store: &Store, table: &str) -> i64 {
    store
        .connection()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
        .unwrap()
}

fn request(conversation: &str, operation: &str, text: &str) -> ValidatedSendRequest {
    ValidatedSendRequest {
        conversation_id: conversation.to_owned(),
        client_operation_id: operation.to_owned(),
        content: MessageContent::text(text).expect("valid text"),
        reply_to: None,
        origin: MessageOrigin::UserInput,
        created_at: 1_700_000_000_000,
    }
}

fn persisted_draft(fixture: &Fixture, text: &str) -> String {
    let mut store = fixture.open();
    fixture.seed(&store);
    let draft_id;
    {
        let mut transaction = store
            .begin_transaction(WriteScope::intent(CONVERSATION))
            .unwrap();
        draft_id = persist_send_intent(&mut transaction, request(CONVERSATION, "op-1", text))
            .unwrap()
            .draft_id;
        commit_transaction(transaction).unwrap();
    }
    draft_id
}

fn prepared_write(submission: &str, ciphertext: &[u8], secret: &[u8]) -> PreparedWrite {
    PreparedWrite {
        draft_id: "unset".to_owned(),
        conversation_id: CONVERSATION.to_owned(),
        submission_id: submission.to_owned(),
        mls_epoch: 7,
        immutable_ciphertext: ciphertext.to_vec(),
        request_hash: [0u8; 32],
        crypto_changes: vec![CryptoRecordChange::new(
            b"mls/group-state".to_vec(),
            0,
            secret.to_vec(),
        )],
    }
}

fn error_code<T>(result: Result<T, StorageError>) -> &'static str {
    match result {
        Ok(_) => panic!("expected an explicit failure"),
        Err(error) => error.code(),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

#[test]
fn open_store_configures_durable_single_writer_store() {
    let fixture = Fixture::new();
    let store = fixture.open();

    assert_eq!(store.schema_version(), 6);
    assert_eq!(store.store_version().unwrap(), 0);
    let journal: String = store
        .connection()
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal.to_ascii_lowercase(), "wal");
    let synchronous: i64 = store
        .connection()
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .unwrap();
    assert_eq!(synchronous, 2, "synchronous=FULL must be enforced");
    let foreign_keys: i64 = store
        .connection()
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    assert_eq!(foreign_keys, 1);
    assert_eq!(store.status(), StoreStatus::Ready);
    assert_eq!(store.recovery_pending().markers, 0);
    drop(store);

    // 重复启动必须幂等：不重复执行迁移，也不新增密钥校验记录。
    let store = fixture.open();
    assert_eq!(store.schema_version(), 6);
    assert_eq!(rows(&store, "schema_migrations"), 6);
    assert_eq!(rows(&store, "store_key_check"), 1);
}

#[test]
fn second_open_is_rejected_while_the_first_holds_the_writer_lock() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let error = match open_store(fixture.options(), fixture.key()) {
        Ok(_) => panic!("second writer must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "STORAGE_LOCKED");
    drop(store);
    let store = fixture.open();
    assert_eq!(store.status(), StoreStatus::Ready);
}

#[test]
fn wrong_key_or_wrong_provenance_is_rejected_without_overwriting_data() {
    let fixture = Fixture::new();
    let draft_id = persisted_draft(&fixture, "错误密钥不得覆盖数据");
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM drafts"), 1);

    let error = match open_store(fixture.options(), fixture.other_key()) {
        Ok(_) => panic!("wrong key must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "STATE_CORRUPTED");

    let provenance = KeyProvenance::Argon2id {
        m_cost_kib: 8 * 1024,
        t_cost: 1,
        parallelism: 1,
        salt: vec![9u8; 16],
    };
    let error = match open_store(StoreOptions::new(&fixture.path, provenance), fixture.key()) {
        Ok(_) => panic!("wrong key provenance must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "STATE_CORRUPTED");

    // 正确密钥仍能读到原数据，且没有被“重建空库”。
    let store = fixture.open();
    let draft = store.read_draft(&draft_id).unwrap().unwrap();
    assert_eq!(
        draft.content,
        MessageContent::text("错误密钥不得覆盖数据").unwrap()
    );
    assert_eq!(rows(&store, "drafts"), 1);
}

#[test]
fn send_intent_is_idempotent_and_conflicting_requests_fail() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    fixture.seed(&store);

    let mut transaction = store
        .begin_transaction(WriteScope::intent(CONVERSATION))
        .unwrap();
    let first = persist_send_intent(&mut transaction, request(CONVERSATION, "op-1", "你好 意图"))
        .unwrap();
    assert!(!first.already_existed);
    let mut retry_request = request(CONVERSATION, "op-1", "你好 意图");
    retry_request.created_at += 100;
    let second = persist_send_intent(&mut transaction, retry_request).unwrap();
    assert!(second.already_existed);
    assert_eq!(second.draft_id, first.draft_id);
    assert_eq!(second.request_hash, first.request_hash);
    let token = commit_transaction(transaction).unwrap();
    assert_eq!(token.committed_version, 1);
    assert!(token.changed_rows >= 2);

    // 连点发送只产生一条草稿、一个准备任务、一个恢复标记。
    assert_eq!(rows(&store, "drafts"), 1);
    assert_eq!(rows(&store, "outbox_jobs"), 1);
    assert_eq!(rows(&store, "store_recovery_markers"), 1);
    assert_eq!(store.waiting_drafts().unwrap(), vec![first.draft_id.clone()]);

    // 同 ID 换正文必须失败，且失败事务不能提交半成品。
    let mut transaction = store
        .begin_transaction(WriteScope::intent(CONVERSATION))
        .unwrap();
    let error = persist_send_intent(&mut transaction, request(CONVERSATION, "op-1", "换正文"))
        .unwrap_err();
    assert_eq!(error.code(), "OPERATION_ID_CONFLICT");
    assert!(transaction.is_poisoned());
    assert_eq!(
        error_code(commit_transaction(transaction)),
        "STORAGE_FAILURE",
        "poisoned transaction must refuse to commit"
    );
    assert_eq!(rows(&store, "drafts"), 1);

    // 同 operation ID 出现在另一个会话也必须失败（唯一索引 + 摘要校验）。
    let mut transaction = store
        .begin_transaction(WriteScope::intent(CONVERSATION_TWO))
        .unwrap();
    let error = persist_send_intent(
        &mut transaction,
        request(CONVERSATION_TWO, "op-1", "你好 意图"),
    )
    .unwrap_err();
    assert_eq!(error.code(), "OPERATION_ID_CONFLICT");
    rollback_transaction(transaction).unwrap();

    // 有界拒绝：空正文、超长正文、非法 ID、负时间戳、错误作用域、缺失会话。
    let mut transaction = store
        .begin_transaction(WriteScope::intent(CONVERSATION))
        .unwrap();
    let mut blank = request(CONVERSATION, "op-empty", "占位");
    blank.content = MessageContent::Text("   ".to_owned());
    assert_eq!(
        persist_send_intent(&mut transaction, blank).unwrap_err().code(),
        "INVALID_INPUT"
    );
    rollback_transaction(transaction).unwrap();

    let mut transaction = store
        .begin_transaction(WriteScope::intent(CONVERSATION))
        .unwrap();
    let mut oversized = request(CONVERSATION, "op-big", "占位");
    oversized.content =
        MessageContent::Text("中".repeat(chat_storage::formal::MAX_TEXT_BYTES));
    assert_eq!(
        persist_send_intent(&mut transaction, oversized)
            .unwrap_err()
            .code(),
        "INVALID_INPUT"
    );
    rollback_transaction(transaction).unwrap();

    let mut transaction = store
        .begin_transaction(WriteScope::intent(CONVERSATION))
        .unwrap();
    let mut invalid = request(CONVERSATION, "op-ok", "body");
    invalid.client_operation_id = "bad id".to_owned();
    assert_eq!(
        persist_send_intent(&mut transaction, invalid)
            .unwrap_err()
            .code(),
        "INVALID_INPUT"
    );
    rollback_transaction(transaction).unwrap();

    let mut transaction = store
        .begin_transaction(WriteScope::intent(CONVERSATION))
        .unwrap();
    let mut negative = request(CONVERSATION, "op-neg", "body");
    negative.created_at = -1;
    assert_eq!(
        persist_send_intent(&mut transaction, negative)
            .unwrap_err()
            .code(),
        "INVALID_INPUT"
    );
    rollback_transaction(transaction).unwrap();

    let mut transaction = store
        .begin_transaction(WriteScope::attempt(CONVERSATION))
        .unwrap();
    assert_eq!(
        persist_send_intent(&mut transaction, request(CONVERSATION, "op-wrong-scope", "body"))
            .unwrap_err()
            .code(),
        "INVALID_INPUT"
    );
    rollback_transaction(transaction).unwrap();

    let mut transaction = store
        .begin_transaction(WriteScope::intent(CONVERSATION))
        .unwrap();
    assert_eq!(
        persist_send_intent(
            &mut transaction,
            request("conv-missing", "op-missing", "body")
        )
        .unwrap_err()
        .code(),
        "INVALID_INPUT"
    );
    rollback_transaction(transaction).unwrap();
}

#[test]
fn transaction_drop_rolls_back_and_version_is_checked() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    fixture.seed(&store);

    {
        let mut transaction = store
            .begin_transaction(WriteScope::intent(CONVERSATION))
            .unwrap();
        persist_send_intent(&mut transaction, request(CONVERSATION, "op-drop", "未提交")).unwrap();
        // 不 commit：Drop 必须等价于 rollback。
    }
    assert_eq!(rows(&store, "drafts"), 0);
    assert_eq!(rows(&store, "outbox_jobs"), 0);
    assert_eq!(rows(&store, "store_recovery_markers"), 0);
    assert_eq!(store.store_version().unwrap(), 0);

    let mut transaction = store
        .begin_transaction(WriteScope::intent(CONVERSATION))
        .unwrap();
    persist_send_intent(&mut transaction, request(CONVERSATION, "op-drop", "未提交")).unwrap();
    rollback_transaction(transaction).unwrap();
    assert_eq!(rows(&store, "drafts"), 0);

    // 期望版本不匹配时事务开启即失败。
    let error = match store.begin_transaction(
        WriteScope::intent(CONVERSATION).with_expected_version(5),
    ) {
        Ok(_) => panic!("stale expected version must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "VERSION_CONFLICT");
    assert_eq!(store.store_version().unwrap(), 0);

    let mut transaction = store
        .begin_transaction(WriteScope::intent(CONVERSATION).with_expected_version(0))
        .unwrap();
    persist_send_intent(&mut transaction, request(CONVERSATION, "op-drop", "未提交")).unwrap();
    let token = commit_transaction(transaction).unwrap();
    assert_eq!(token.committed_version, 1);
    assert_eq!(store.store_version().unwrap(), 1);
}

#[test]
fn fail_closed_store_rejects_every_operation() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    store.mark_faulted("rollback could not be confirmed");
    assert_eq!(store.status(), StoreStatus::Faulted);
    assert_eq!(
        error_code(store.begin_transaction(WriteScope::intent(CONVERSATION))),
        "STORAGE_FAILURE"
    );
    assert_eq!(
        error_code(store.read_draft("draft-missing")),
        "STORAGE_FAILURE"
    );
    assert_eq!(error_code(store.store_version()), "STORAGE_FAILURE");
    assert_eq!(error_code(store.outbox_jobs()), "STORAGE_FAILURE");
}

#[test]
fn prepared_attempt_is_atomic_idempotent_and_version_checked() {
    let fixture = Fixture::new();
    let draft_id = persisted_draft(&fixture, "准备尝试");
    let mut store = fixture.open();
    let draft = store.read_draft(&draft_id).unwrap().unwrap();

    let ciphertext = b"mls-ciphertext-bytes".to_vec();
    let mut prepared = prepared_write("sub-1", &ciphertext, b"mls-secret-value");
    prepared.draft_id = draft_id.clone();
    prepared.request_hash = draft.request_hash;

    let attempt: AttemptRecord;
    {
        let mut transaction = store
            .begin_transaction(WriteScope::attempt(CONVERSATION))
            .unwrap();
        attempt = persist_prepared_attempt(&mut transaction, prepared.clone()).unwrap();
        assert!(!attempt.already_persisted);
        assert_eq!(attempt.candidate_hash.len(), 32);
        // 同事务必须能读到刚写入（未提交）的 MLS 秘密。
        let value = transaction
            .read_mls_record(CONVERSATION, b"mls/group-state")
            .unwrap()
            .expect("sealed MLS record must be readable inside the same transaction");
        assert_eq!(value.as_slice(), b"mls-secret-value");
        // 幂等重放返回同一记录，不产生第二个任务。
        let replay = persist_prepared_attempt(&mut transaction, prepared.clone()).unwrap();
        assert!(replay.already_persisted);
        assert_eq!(replay.candidate_hash, attempt.candidate_hash);
        commit_transaction(transaction).unwrap();
    }

    assert_eq!(rows(&store, "message_attempts"), 1);
    assert_eq!(rows(&store, "outbox_jobs"), 2);
    let jobs = store.outbox_jobs().unwrap();
    assert!(
        jobs.iter()
            .any(|job| job.kind == "prepare" && job.state == "complete")
    );
    assert!(
        jobs.iter()
            .any(|job| job.kind == "send" && job.state == "pending")
    );

    let stored = store.read_attempt("sub-1").unwrap().unwrap();
    assert_eq!(stored.immutable_ciphertext, ciphertext);
    assert_eq!(stored.candidate_hash, attempt.candidate_hash);
    let sealed_secret = store
        .read_mls_record(CONVERSATION, b"mls/group-state")
        .unwrap()
        .unwrap();
    assert_eq!(sealed_secret.as_slice(), b"mls-secret-value");

    // 同 SubmissionId 换密文必须失败。
    let mut changed = prepared.clone();
    changed.immutable_ciphertext = b"different-ciphertext".to_vec();
    let mut transaction = store
        .begin_transaction(WriteScope::attempt(CONVERSATION))
        .unwrap();
    assert_eq!(
        persist_prepared_attempt(&mut transaction, changed)
            .unwrap_err()
            .code(),
        "SUBMISSION_ID_CONFLICT"
    );
    assert!(transaction.is_poisoned());
    rollback_transaction(transaction).unwrap();

    // 同一草稿的第二个活跃尝试必须失败（attempt 不可原地换密文）。
    let mut other = prepared.clone();
    other.submission_id = "sub-2".to_owned();
    let mut transaction = store
        .begin_transaction(WriteScope::attempt(CONVERSATION))
        .unwrap();
    assert_eq!(
        persist_prepared_attempt(&mut transaction, other)
            .unwrap_err()
            .code(),
        "SUBMISSION_ID_CONFLICT"
    );
    rollback_transaction(transaction).unwrap();

    // 第二个草稿用于验证 MLS 记录版本必须递增。
    let mut transaction = store
        .begin_transaction(WriteScope::intent(CONVERSATION))
        .unwrap();
    let second_draft =
        persist_send_intent(&mut transaction, request(CONVERSATION, "op-2", "第二个草稿")).unwrap();
    commit_transaction(transaction).unwrap();
    let mut stale = prepared_write("sub-3", b"opaque-third", b"stale-version");
    stale.draft_id = second_draft.draft_id.clone();
    stale.request_hash = second_draft.request_hash;
    stale.crypto_changes = vec![CryptoRecordChange::new(
        b"mls/group-state".to_vec(),
        0,
        b"stale-version".to_vec(),
    )];
    let mut transaction = store
        .begin_transaction(WriteScope::attempt(CONVERSATION))
        .unwrap();
    assert_eq!(
        persist_prepared_attempt(&mut transaction, stale)
            .unwrap_err()
            .code(),
        "VERSION_CONFLICT"
    );
    rollback_transaction(transaction).unwrap();

    // 草稿摘要不一致必须拒绝，且整体回滚。
    let mut mismatched = prepared.clone();
    mismatched.submission_id = "sub-4".to_owned();
    mismatched.request_hash = [1u8; 32];
    let mut transaction = store
        .begin_transaction(WriteScope::attempt(CONVERSATION))
        .unwrap();
    assert_eq!(
        persist_prepared_attempt(&mut transaction, mismatched)
            .unwrap_err()
            .code(),
        "INVALID_INPUT"
    );
    rollback_transaction(transaction).unwrap();
    assert_eq!(rows(&store, "message_attempts"), 1);
}

#[test]
fn plaintext_body_and_master_key_never_appear_in_store_files() {
    let fixture = Fixture::new();
    let body = format!("绝密正文-{}", uuid::Uuid::new_v4());
    let secret = format!("MLS-SECRET-{}", uuid::Uuid::new_v4());
    let draft_id = persisted_draft(&fixture, &body);
    {
        let mut store = fixture.open();
        let draft = store.read_draft(&draft_id).unwrap().unwrap();
        let mut prepared = prepared_write("sub-plain", b"opaque-mls-ciphertext", secret.as_bytes());
        prepared.draft_id = draft_id.clone();
        prepared.request_hash = draft.request_hash;
        let mut transaction = store
            .begin_transaction(WriteScope::attempt(CONVERSATION))
            .unwrap();
        persist_prepared_attempt(&mut transaction, prepared).unwrap();
        commit_transaction(transaction).unwrap();
    }
    let files = fixture.store_files();
    assert!(!files.is_empty(), "at least the database file must exist");
    for (suffix, bytes) in &files {
        assert!(
            !contains(bytes, body.as_bytes()),
            "plaintext body leaked into {suffix} file"
        );
        assert!(
            !contains(bytes, secret.as_bytes()),
            "plaintext MLS secret leaked into {suffix} file"
        );
        assert!(
            !contains(bytes, &fixture.key_bytes),
            "master key bytes leaked into {suffix} file"
        );
    }
    // 重新打开后仍能正确解密（证明写入的是密文而不是丢弃正文）。
    let store = fixture.open();
    let draft = store.read_draft(&draft_id).unwrap().unwrap();
    assert_eq!(draft.content, MessageContent::text(body).unwrap());
}

#[test]
fn tampered_or_relocated_ciphertext_is_rejected() {
    let fixture = Fixture::new();
    let draft_id = persisted_draft(&fixture, "篡改检测正文");
    {
        let mut store = fixture.open();
        let draft = store.read_draft(&draft_id).unwrap().unwrap();
        let mut prepared = prepared_write("sub-tamper", b"opaque-ciphertext", b"mls-secret");
        prepared.draft_id = draft_id.clone();
        prepared.request_hash = draft.request_hash;
        let mut transaction = store
            .begin_transaction(WriteScope::attempt(CONVERSATION))
            .unwrap();
        persist_prepared_attempt(&mut transaction, prepared).unwrap();
        commit_transaction(transaction).unwrap();
    }

    // 直接改一个字节：AEAD 认证必须失败。
    {
        let connection = fixture.raw();
        let mut sealed: Vec<u8> = connection
            .query_row(
                "SELECT protected_content FROM drafts WHERE draft_id = ?1",
                [&draft_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(sealed.len() > 20, "sealed value must carry nonce and tag");
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;
        connection
            .execute(
                "UPDATE drafts SET protected_content = ?1 WHERE draft_id = ?2",
                rusqlite::params![sealed, draft_id],
            )
            .unwrap();
    }
    let store = fixture.open();
    assert_eq!(
        error_code(store.read_draft(&draft_id)),
        "STATE_CORRUPTED",
        "tampered draft content must be rejected"
    );
    drop(store);

    // 把同一段密文挪到另一行：AAD 绑定草稿身份，必须失败。
    {
        let connection = fixture.raw();
        connection
            .execute(
                "INSERT INTO drafts (draft_id, conversation_id, operation_id, request_hash, protected_content, state, created_at)
                 SELECT 'draft-moved', conversation_id, 'op-moved', request_hash, protected_content, state, created_at
                 FROM drafts WHERE draft_id = ?1",
                [&draft_id],
            )
            .unwrap();
    }
    let store = fixture.open();
    assert_eq!(
        error_code(store.read_draft("draft-moved")),
        "STATE_CORRUPTED",
        "relocated ciphertext must not authenticate under another row identity"
    );
    assert!(store.read_mls_record(CONVERSATION, b"no-such-record").unwrap().is_none());
    drop(store);

    // MLS 记录值篡改同样必须拒绝。
    {
        let connection = fixture.raw();
        let mut sealed: Vec<u8> = connection
            .query_row(
                "SELECT protected_value FROM mls_state_records WHERE record_key = X'6D6C732F67726F75702D7374617465'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        sealed[0] ^= 0x01;
        connection
            .execute(
                "UPDATE mls_state_records SET protected_value = ?1 WHERE record_key = X'6D6C732F67726F75702D7374617465'",
                rusqlite::params![sealed],
            )
            .unwrap();
    }
    let store = fixture.open();
    assert_eq!(
        error_code(store.read_mls_record(CONVERSATION, b"mls/group-state")),
        "STATE_CORRUPTED"
    );
}

#[test]
fn recover_incomplete_writes_clears_markers_and_resets_leases() {
    let fixture = Fixture::new();
    let draft_id = persisted_draft(&fixture, "恢复标记");
    {
        let mut store = fixture.open();
        let draft = store.read_draft(&draft_id).unwrap().unwrap();
        let mut prepared = prepared_write("sub-recover", b"opaque", b"mls-secret");
        prepared.draft_id = draft_id.clone();
        prepared.request_hash = draft.request_hash;
        let mut transaction = store
            .begin_transaction(WriteScope::attempt(CONVERSATION))
            .unwrap();
        persist_prepared_attempt(&mut transaction, prepared).unwrap();
        commit_transaction(transaction).unwrap();
    }

    let mut store = fixture.open();
    // 两条恢复标记（草稿 + 尝试）。
    assert_eq!(store.recovery_pending().markers, 2);
    assert_eq!(store.recovery_markers().unwrap().len(), 2);

    // 制造外部损坏：删除已提交的发送任务并放一个过期租约。
    store
        .connection()
        .execute("DELETE FROM outbox_jobs WHERE kind = 'send'", [])
        .unwrap();
    store
        .connection()
        .execute(
            "UPDATE outbox_jobs SET state = 'leased', lease_owner = 'worker-old', lease_until = 1",
            [],
        )
        .unwrap();

    let report = recover_incomplete_writes(&mut store).unwrap();
    assert_eq!(report.markers_processed, 2);
    assert_eq!(report.markers_cleared, 2);
    assert_eq!(report.send_jobs_requeued, 1);
    assert_eq!(report.leases_reset, 1);
    assert!(!report.recovery_required);
    assert!(!report.state_corrupted);
    assert_eq!(store.recovery_pending().markers, 0);

    let jobs = store.outbox_jobs().unwrap();
    let send = jobs
        .iter()
        .find(|job| job.kind == "send")
        .expect("send job must be rebuilt from the committed attempt");
    assert_eq!(send.state, "pending");
    assert_eq!(send.lease_until, None);
    // 不产生半消息：attempt 仍只有一条且内容不变。
    assert_eq!(rows(&store, "message_attempts"), 1);
    let attempt = store.read_attempt("sub-recover").unwrap().unwrap();
    assert_eq!(attempt.immutable_ciphertext, b"opaque");

    // 幂等：第二次恢复没有标记可处理。
    let second = recover_incomplete_writes(&mut store).unwrap();
    assert_eq!(second.markers_processed, 0);
    assert_eq!(second.send_jobs_requeued, 0);
}

#[test]
fn recovery_repairs_a_missing_prepare_job_without_creating_messages() {
    let fixture = Fixture::new();
    let draft_id = persisted_draft(&fixture, "缺任务恢复");
    {
        let connection = fixture.raw();
        connection
            .execute("DELETE FROM outbox_jobs WHERE kind = 'prepare'", [])
            .unwrap();
    }
    let mut store = fixture.open();
    assert_eq!(store.recovery_pending().markers, 1);
    let report = recover_incomplete_writes(&mut store).unwrap();
    assert_eq!(report.prepare_jobs_requeued, 1);
    assert_eq!(report.markers_cleared, 1);
    assert_eq!(store.waiting_drafts().unwrap(), vec![draft_id]);
    assert_eq!(rows(&store, "message_attempts"), 0);
}

#[test]
fn recovery_quarantines_a_tampered_committed_attempt() {
    let fixture = Fixture::new();
    let draft_id = persisted_draft(&fixture, "篡改尝试隔离");
    {
        let mut store = fixture.open();
        let draft = store.read_draft(&draft_id).unwrap().unwrap();
        let mut prepared = prepared_write("sub-quarantine", b"opaque-ciphertext", b"mls-secret");
        prepared.draft_id = draft_id.clone();
        prepared.request_hash = draft.request_hash;
        let mut transaction = store
            .begin_transaction(WriteScope::attempt(CONVERSATION))
            .unwrap();
        persist_prepared_attempt(&mut transaction, prepared).unwrap();
        commit_transaction(transaction).unwrap();
    }
    {
        let connection = fixture.raw();
        let mut ciphertext: Vec<u8> = connection
            .query_row(
                "SELECT immutable_ciphertext FROM message_attempts WHERE submission_id = 'sub-quarantine'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        ciphertext[0] ^= 0x01;
        connection
            .execute(
                "UPDATE message_attempts SET immutable_ciphertext = ?1 WHERE submission_id = 'sub-quarantine'",
                rusqlite::params![ciphertext],
            )
            .unwrap();
    }

    let mut store = fixture.open();
    let error = recover_incomplete_writes(&mut store).unwrap_err();
    assert_eq!(error.code(), "STATE_CORRUPTED");
    let quarantined = store.quarantine_records().unwrap();
    assert_eq!(quarantined.len(), 1);
    assert_eq!(quarantined[0].kind, "attempt_digest_mismatch");
    assert_eq!(quarantined[0].ref_id, "sub-quarantine");
    assert!(quarantined[0].detail.contains("digest"));
    let attempt = store.read_attempt("sub-quarantine").unwrap().unwrap();
    assert_eq!(
        attempt.state, "unknown",
        "quarantined attempt must be isolated"
    );
    assert_eq!(store.recovery_pending().markers, 0);
    // 同一隔离记录不会被重复写入。
    let _ = recover_incomplete_writes(&mut store);
    assert_eq!(store.quarantine_records().unwrap().len(), 1);
}

#[test]
fn migration_checksum_mismatch_fails_closed() {
    let fixture = Fixture::new();
    {
        let store = fixture.open();
        assert_eq!(store.schema_version(), 6);
    }
    {
        let connection = fixture.raw();
        connection
            .execute(
                "UPDATE schema_migrations SET checksum = 'tampered' WHERE version = 1",
                [],
            )
            .unwrap();
    }
    let error = match open_store(fixture.options(), fixture.key()) {
        Ok(_) => panic!("checksum mismatch must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "MIGRATION_FAILED");
}

#[test]
fn too_new_schema_version_is_rejected() {
    let fixture = Fixture::new();
    {
        let store = fixture.open();
        assert_eq!(store.schema_version(), 6);
    }
    {
        let connection = fixture.raw();
        connection
            .execute(
                "INSERT INTO schema_migrations (version, checksum, applied_at) VALUES (99, 'future', 0)",
                [],
            )
            .unwrap();
    }
    let error = match open_store(fixture.options(), fixture.key()) {
        Ok(_) => panic!("newer schema must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "SCHEMA_TOO_NEW");
}

/// 已写入受保护数据却没有密钥校验记录时必须拒绝（不能用新密钥静默接管）。
#[test]
fn store_without_key_check_record_is_rejected_when_data_exists() {
    let fixture = Fixture::new();
    let _draft_id = persisted_draft(&fixture, "已有数据的库");
    {
        let connection = fixture.raw();
        connection
            .execute("DELETE FROM store_key_check", [])
            .unwrap();
    }
    let error = match open_store(fixture.options(), fixture.key()) {
        Ok(_) => panic!("store without key verification must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "STATE_CORRUPTED");
}

/// 首次打开中途崩溃（只有 schema、没有业务行）允许补写密钥校验记录，不会把库卡死。
#[test]
fn empty_store_without_key_check_record_can_be_initialised_again() {
    let fixture = Fixture::new();
    {
        let store = fixture.open();
        assert_eq!(store.schema_version(), 6);
    }
    {
        let connection = fixture.raw();
        connection
            .execute("DELETE FROM store_key_check", [])
            .unwrap();
    }
    // 换一把密钥重新初始化：此时库里没有任何受保护数据，允许补写。
    let mut store = open_store(fixture.options(), fixture.other_key()).unwrap();
    fixture.seed(&store);
    let mut transaction = store
        .begin_transaction(WriteScope::intent(CONVERSATION))
        .unwrap();
    persist_send_intent(&mut transaction, request(CONVERSATION, "op-reinit", "新密钥")).unwrap();
    commit_transaction(transaction).unwrap();
    assert_eq!(rows(&store, "drafts"), 1);
    drop(store);
    // 原密钥不再能打开该库（校验记录已按新密钥写入）。
    let error = match open_store(fixture.options(), fixture.key()) {
        Ok(_) => panic!("old key must be rejected after re-initialisation"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "STATE_CORRUPTED");
}

#[test]
fn draft_records_expose_the_documented_projection() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    fixture.seed(&store);
    let mut transaction = store
        .begin_transaction(WriteScope::intent(CONVERSATION))
        .unwrap();
    let record: DraftRecord =
        persist_send_intent(&mut transaction, request(CONVERSATION, "op-projection", "投影"))
            .unwrap();
    commit_transaction(transaction).unwrap();
    assert_eq!(record.state, "queued");
    assert_eq!(record.conversation_id, CONVERSATION);
    assert_eq!(record.operation_id, "op-projection");
    assert_eq!(record.created_at, 1_700_000_000_000);
    let read = store.read_draft(&record.draft_id).unwrap().unwrap();
    assert_eq!(read.request_hash, record.request_hash);
    assert!(read.draft_id.starts_with("draft-"));
    // 不存在的记录返回 None，而不是伪造空记录。
    assert!(store.read_draft("draft-missing").unwrap().is_none());
    assert!(store.read_attempt("sub-missing").unwrap().is_none());
}