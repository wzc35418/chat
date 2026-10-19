//! 消息对附件的引用（迁移 0009）集成测试：幂等引用、级联删除、count/list、
//! 回收用的“被引用附件集合”与 staging 候选查询。

use chat_storage::formal::{
    AttachmentRecordInput, KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store,
};
use std::fs;
use std::path::PathBuf;

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
    key_bytes: [u8; 32],
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "chat-msg-att-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&dir).expect("create fixture directory");
        let mut key_bytes = [0u8; 32];
        key_bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        key_bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Self {
            path: dir.join("chat.db"),
            dir,
            key_bytes,
        }
    }

    fn open(&self) -> Store {
        open_store(
            StoreOptions::new(&self.path, KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(self.key_bytes),
        )
        .expect("open formal store")
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

fn attachment(attachment_id: &str, state: &str, created_at_ms: i64) -> AttachmentRecordInput {
    AttachmentRecordInput {
        attachment_id: attachment_id.to_owned(),
        state: state.to_owned(),
        plain_size: 1024,
        chunk_size: 262_144,
        chunk_count: 1,
        content_digest: "ab".repeat(32),
        ciphertext_digest: "cd".repeat(32),
        display_name: "引用测试.pdf".to_owned(),
        created_at_ms,
    }
}

/// 测试夹具：直接写入最小可用的会话/attempt/消息行（消息引用需要真实外键目标）。
fn seed_message(store: &Store, message_id: &str) {
    let connection = store.connection();
    connection
        .execute(
            "INSERT OR IGNORE INTO conversations
                 (conversation_id, kind, coordinator_id, protected_metadata, state)
             VALUES ('conv-ref-test', 'direct', '', X'00', 'active')",
            [],
        )
        .expect("seed conversation");
    connection
        .execute(
            "INSERT OR IGNORE INTO message_attempts
                 (submission_id, origin, conversation_id, mls_epoch, immutable_ciphertext,
                  candidate_hash, state)
             VALUES ('sub-ref-test', 'remote', 'conv-ref-test', 0, X'00', X'00', 'accepted')",
            [],
        )
        .expect("seed attempt");
    connection
        .execute(
            "INSERT OR IGNORE INTO messages
                 (message_id, conversation_id, submission_id, author_account_id,
                  author_device_id, group_seq, protected_content, display_created_at, send_state)
             VALUES (?1, 'conv-ref-test', 'sub-ref-test', 'acct-ref', 'dev-ref', 1, X'00', 1, 'accepted')",
            [message_id],
        )
        .expect("seed message");
}

#[test]
fn message_attachment_refs_are_idempotent_and_cascade() {
    let fixture = Fixture::new("refs");
    let mut store = fixture.open();
    assert_eq!(
        store.schema_version(),
        12,
        "0009 adds message attachments + 0010 plugins + 0011 credential directory"
    );
    store
        .save_attachment(attachment("att-ref", "imported", 1_000))
        .expect("save attachment");
    seed_message(&store, "msg-1");

    // 幂等：重复引用不新增行。
    store
        .attach_message_attachment("msg-1", "att-ref")
        .expect("attach");
    store
        .attach_message_attachment("msg-1", "att-ref")
        .expect("attach again");
    assert_eq!(rows(&store, "message_attachments"), 1);
    assert_eq!(store.message_attachment_count("att-ref").expect("count"), 1);
    assert_eq!(
        store.message_attachment_ids("att-ref").expect("list"),
        vec!["msg-1".to_owned()]
    );

    // 解除：幂等。
    assert!(store
        .detach_message_attachment("msg-1", "att-ref")
        .expect("detach"));
    assert!(!store
        .detach_message_attachment("msg-1", "att-ref")
        .expect("detach again"));
    assert_eq!(store.message_attachment_count("att-ref").expect("count"), 0);

    // 重新引用后删除附件记录：外键级联清掉引用。
    store
        .attach_message_attachment("msg-1", "att-ref")
        .expect("attach again");
    assert!(store.delete_attachment("att-ref").expect("delete attachment"));
    assert_eq!(rows(&store, "message_attachments"), 0, "refs cascade with the record");
    assert_eq!(store.message_attachment_count("att-ref").expect("count"), 0);
}

#[test]
fn message_attachment_refs_require_existing_rows() {
    let fixture = Fixture::new("missing");
    let mut store = fixture.open();
    store
        .save_attachment(attachment("att-ok", "imported", 1_000))
        .expect("save attachment");
    seed_message(&store, "msg-ok");

    // 附件记录不存在。
    assert_eq!(
        store
            .attach_message_attachment("msg-ok", "att-missing")
            .unwrap_err()
            .code(),
        "ATTACHMENT_NOT_FOUND"
    );
    // 消息不存在。
    assert_eq!(
        store
            .attach_message_attachment("msg-missing", "att-ok")
            .unwrap_err()
            .code(),
        "INVALID_INPUT"
    );
    // 畸形 ID。
    assert_eq!(
        store
            .attach_message_attachment("bad id", "att-ok")
            .unwrap_err()
            .code(),
        "INVALID_INPUT"
    );
    assert_eq!(rows(&store, "message_attachments"), 0);
}

#[test]
fn message_referenced_ids_are_distinct_and_detachable() {
    let fixture = Fixture::new("bulk");
    let mut store = fixture.open();
    for id in ["att-a", "att-b"] {
        store
            .save_attachment(attachment(id, "imported", 1_000))
            .expect("save attachment");
    }
    seed_message(&store, "msg-1");
    store
        .attach_message_attachment("msg-1", "att-a")
        .expect("attach a");
    store
        .attach_message_attachment("msg-1", "att-b")
        .expect("attach b");
    assert_eq!(rows(&store, "message_attachments"), 2);

    let referenced = store
        .message_referenced_attachment_ids()
        .expect("referenced ids");
    assert_eq!(referenced, vec!["att-a".to_owned(), "att-b".to_owned()]);

    assert_eq!(
        store
            .detach_message_attachments("msg-1")
            .expect("detach all"),
        2
    );
    assert_eq!(store.message_attachment_count("att-a").expect("count"), 0);
    assert_eq!(
        store
            .message_referenced_attachment_ids()
            .expect("referenced ids"),
        Vec::<String>::new()
    );
}

#[test]
fn attachment_ids_with_state_before_filters_state_and_age() {
    let fixture = Fixture::new("staging");
    let mut store = fixture.open();
    store
        .save_attachment(attachment("att-old-staging", "staging", 1_000))
        .expect("old staging");
    store
        .save_attachment(attachment("att-new-staging", "staging", 9_000_000_000_000))
        .expect("new staging");
    store
        .save_attachment(attachment("att-old-imported", "imported", 1_000))
        .expect("old imported");

    let stale = store
        .attachment_ids_with_state_before("staging", 5_000, 16)
        .expect("staging candidates");
    assert_eq!(stale, vec!["att-old-staging".to_owned()]);
    // 白名单/边界。
    assert_eq!(
        store
            .attachment_ids_with_state_before("bogus", 5_000, 16)
            .unwrap_err()
            .code(),
        "INVALID_INPUT"
    );
    assert_eq!(
        store
            .attachment_ids_with_state_before("staging", -1, 16)
            .unwrap_err()
            .code(),
        "INVALID_INPUT"
    );
    assert_eq!(
        store
            .attachment_ids_with_state_before("staging", 5_000, 0)
            .unwrap_err()
            .code(),
        "INVALID_INPUT"
    );
}
