//! STORAGE-008 已验证条目写入的集成测试。
//!
//! 覆盖：写入后分页查询可读、正文/标题不落盘明文、按 message_id 幂等、
//! 同 ID 不同内容显式冲突、会话行自动创建与摘要更新、失败整体回滚、
//! 畸形输入有界拒绝、外来 protected_metadata 不被覆盖（显式 STATE_CORRUPTED）。

use chat_storage::formal::{
    AppliedEntry, KeyProvenance, MAX_SESSION_PREVIEW_CHARS, MAX_SESSION_TITLE_BYTES,
    MessageContent, ProtectedStorageKey, SessionSummary, StorageError, Store, StoreOptions,
    VerifiedEntryInput, WriteScope, commit_transaction, materialize_session, open_store,
    verified_entry_submission_id,
};
use rusqlite::params;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

const CONVERSATION: &str = "conv-entry-1";
const OTHER_CONVERSATION: &str = "conv-entry-2";
const MESSAGE: &str = "msg-entry-1";
const ACCOUNT: &str = "account-entry";
const DEVICE: &str = "device-entry";
const NOW: i64 = 1_700_000_000_000;

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
    key_bytes: [u8; 32],
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("chat-formal-entry-{}", uuid::Uuid::new_v4()));
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

fn entry(conversation: &str, message: &str, body: &str, created_at: i64) -> VerifiedEntryInput {
    VerifiedEntryInput {
        conversation_id: conversation.to_owned(),
        title: "项目讨论".to_owned(),
        message_id: message.to_owned(),
        author_account: ACCOUNT.to_owned(),
        author_device: DEVICE.to_owned(),
        body: body.to_owned(),
        mine: true,
        created_at_ms: created_at,
    }
}

fn count(store: &Store, sql: &str) -> i64 {
    store.connection().query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn entry_write_is_visible_to_pages_and_session_summary() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let applied = store
        .apply_verified_entry(entry(CONVERSATION, MESSAGE, "第一条正式消息", NOW))
        .expect("apply verified entry");
    assert!(!applied.already_existed);
    assert_eq!(applied.message_id, MESSAGE);
    assert_eq!(applied.group_seq, 1);
    assert_eq!(applied.submission_id, verified_entry_submission_id(CONVERSATION, MESSAGE));

    // 1. 分页查询能读到正式消息（待发段为空，正式段一条）。
    let page = store
        .query_message_page(CONVERSATION, None, 10)
        .expect("message page");
    assert_eq!(page.items.len(), 1);
    let view = &page.items[0];
    assert_eq!(view.message_id, MESSAGE);
    assert_eq!(view.content, MessageContent::text("第一条正式消息").unwrap());
    assert_eq!(view.author_account_id.as_deref(), Some(ACCOUNT));
    assert_eq!(view.author_device_id.as_deref(), Some(DEVICE));
    assert!(!view.local_pending);
    assert!(page.next_cursor.is_none());

    // 2. 会话列表出现该会话，摘要有标题/预览/活动时间/本机身份。
    let conversations = store
        .query_conversation_page(None, 10)
        .expect("conversation page");
    assert_eq!(conversations.items.len(), 1);
    assert_eq!(conversations.items[0].conversation_id, CONVERSATION);
    assert_eq!(conversations.items[0].last_activity_at, NOW);
    let summary = store
        .session_summary(CONVERSATION)
        .expect("read summary")
        .expect("summary exists");
    assert_eq!(summary.title, "项目讨论");
    assert_eq!(summary.preview, "第一条正式消息");
    assert_eq!(summary.updated_at_ms, NOW);
    assert_eq!(summary.local_account.as_deref(), Some(ACCOUNT));
    assert_eq!(summary.local_device.as_deref(), Some(DEVICE));
}

#[test]
fn entry_write_is_idempotent_and_conflicts_are_explicit() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let input = entry(CONVERSATION, MESSAGE, "幂等正文", NOW);
    let first = store
        .apply_verified_entry(input.clone())
        .expect("first apply");
    let version_after_first = store.store_version().expect("version");
    let second = store
        .apply_verified_entry(input)
        .expect("idempotent replay");
    assert!(second.already_existed, "replay must report already_existed");
    assert_eq!(first.message_id, second.message_id);
    assert_eq!(first.group_seq, second.group_seq);
    assert_eq!(first.submission_id, second.submission_id);
    assert_eq!(
        store.store_version().expect("version"),
        version_after_first,
        "an idempotent replay must not advance the store version"
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM messages"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM message_attempts"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM conversations"), 1);

    // 同 message_id 不同正文 → 显式冲突，且不覆盖原行。
    let mut conflicting = entry(CONVERSATION, MESSAGE, "另一段正文", NOW);
    conflicting.title = "另一个标题".to_owned();
    assert_eq!(
        error_code(store.apply_verified_entry(conflicting)),
        "MESSAGE_ID_CONFLICT"
    );
    // 同正文不同展示时间/作者同样是不同条目。
    let mut retimed = entry(CONVERSATION, MESSAGE, "幂等正文", NOW + 1);
    retimed.mine = true;
    assert_eq!(
        error_code(store.apply_verified_entry(retimed)),
        "MESSAGE_ID_CONFLICT"
    );
    let mut reauthored = entry(CONVERSATION, MESSAGE, "幂等正文", NOW);
    reauthored.author_device = "device-other".to_owned();
    assert_eq!(
        error_code(store.apply_verified_entry(reauthored)),
        "MESSAGE_ID_CONFLICT"
    );
    let page = store
        .query_message_page(CONVERSATION, None, 10)
        .expect("message page");
    assert_eq!(page.items.len(), 1, "conflicts must not add rows");
    assert_eq!(page.items[0].content, MessageContent::text("幂等正文").unwrap());
}

#[test]
fn entry_write_never_leaves_plaintext_in_store_files() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let marker = format!("BODY-{}", uuid::Uuid::new_v4());
    let title_marker = format!("TITLE-{}", uuid::Uuid::new_v4());
    let mut input = entry(CONVERSATION, MESSAGE, &format!("机密正文 {marker}"), NOW);
    input.title = title_marker.clone();
    store
        .apply_verified_entry(input)
        .expect("apply verified entry");
    let summary = store
        .session_summary(CONVERSATION)
        .expect("read summary")
        .expect("summary exists");
    assert!(summary.preview.contains(&marker), "preview is derived from the body");

    let mut scanned = 0usize;
    for name in ["chat.db", "chat.db-wal", "chat.db-shm"] {
        let path = fixture.dir.join(name);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => panic!("read {name}: {error}"),
        };
        scanned += bytes.len();
        assert!(
            !contains(&bytes, marker.as_bytes()),
            "{name} must not contain the plaintext body"
        );
        assert!(
            !contains(&bytes, title_marker.as_bytes()),
            "{name} must not contain the plaintext title"
        );
    }
    assert!(scanned > 0, "store files must be readable for the scan");
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

#[test]
fn entry_write_auto_creates_conversation_and_keeps_the_first_title() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    assert_eq!(count(&store, "SELECT COUNT(*) FROM conversations"), 0);
    let mut first = entry(CONVERSATION, "msg-1", "第一条", NOW);
    first.title = "最初标题".to_owned();
    store.apply_verified_entry(first).expect("apply first");
    let (kind, state): (String, String) = store
        .connection()
        .query_row(
            "SELECT kind, state FROM conversations WHERE conversation_id = ?1",
            [CONVERSATION],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("conversation row");
    assert_eq!(kind, "direct");
    assert_eq!(state, "active");

    // 第二条带来另一个标题：已建好的本地标题不被覆盖，但预览与时间前进。
    let mut second = entry(CONVERSATION, "msg-2", "第二条", NOW + 10);
    second.title = "后来标题".to_owned();
    store.apply_verified_entry(second).expect("apply second");
    let summary = store
        .session_summary(CONVERSATION)
        .expect("read summary")
        .expect("summary exists");
    assert_eq!(summary.title, "最初标题");
    assert_eq!(summary.preview, "第二条");
    assert_eq!(summary.updated_at_ms, NOW + 10);

    // 第三条（更早的展示时间）不把预览退回旧内容。
    let earlier = entry(CONVERSATION, "msg-3", "更早的补投", NOW + 5);
    store.apply_verified_entry(earlier).expect("apply third");
    let summary = store
        .session_summary(CONVERSATION)
        .expect("read summary")
        .expect("summary exists");
    assert_eq!(summary.preview, "第二条");
    assert_eq!(summary.updated_at_ms, NOW + 10);
    let page = store
        .query_message_page(CONVERSATION, None, 10)
        .expect("message page");
    assert_eq!(page.items.len(), 3);
    // 正式消息按分配到的 group_seq 降序（本轮为写入顺序），展示时间只是元数据。
    assert_eq!(page.items[0].message_id, "msg-3");
    assert_eq!(page.items[0].display_created_at, NOW + 5);
    assert_eq!(page.items[1].message_id, "msg-2");
}

#[test]
fn entry_write_rolls_back_everything_on_failure() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    store
        .connection()
        .execute(
            "INSERT INTO conversations (conversation_id, kind, coordinator_id, protected_metadata, state)
             VALUES (?1, 'direct', 'node-1', X'00', 'active')",
            [OTHER_CONVERSATION],
        )
        .unwrap();
    // 把“New conversation + msg-1”派生出的 SubmissionId 预先绑到另一个会话上，
    // 让真正的写入在会话行与摘要已写入之后才失败。
    let derived = verified_entry_submission_id(CONVERSATION, MESSAGE);
    store
        .connection()
        .execute(
            "INSERT INTO message_attempts (submission_id, origin, draft_id, operation_id, conversation_id,
                                           mls_epoch, immutable_ciphertext, candidate_hash, state)
             VALUES (?1, 'remote', NULL, NULL, ?2, 0, X'00', ?3, 'accepted')",
            params![derived, OTHER_CONVERSATION, vec![0u8; 32]],
        )
        .unwrap();
    let version_before = store.store_version().expect("version");
    assert_eq!(
        error_code(store.apply_verified_entry(entry(CONVERSATION, MESSAGE, "会回滚的正文", NOW))),
        "STATE_CORRUPTED"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM conversations WHERE conversation_id = 'conv-entry-1'"),
        0,
        "the auto-created conversation must be rolled back"
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM messages"), 0);
    assert!(
        store
            .session_summary(CONVERSATION)
            .expect("read summary")
            .is_none()
    );
    assert_eq!(
        store.store_version().expect("version"),
        version_before,
        "a rolled back write must not advance the store version"
    );
    // 回滚后连接仍可用：换一条消息即可正常写入。
    store
        .apply_verified_entry(entry(CONVERSATION, "msg-2", "重试成功", NOW))
        .expect("apply after rollback");
    assert_eq!(store.store_version().expect("version"), version_before + 1);
}

#[test]
fn entry_write_rejects_malformed_input_and_foreign_metadata() {
    let fixture = Fixture::new();
    let mut store = fixture.open();

    // 畸形 ID / 空正文 / 越界标题 / 负时间戳：全部有界拒绝，不写任何行。
    let mut bad = entry(CONVERSATION, MESSAGE, "正文", NOW);
    bad.message_id = String::new();
    assert_eq!(error_code(store.apply_verified_entry(bad)), "INVALID_INPUT");
    let mut bad = entry(CONVERSATION, MESSAGE, "正文", NOW);
    bad.message_id = "msg with space".to_owned();
    assert_eq!(error_code(store.apply_verified_entry(bad)), "INVALID_INPUT");
    let bad = entry(CONVERSATION, MESSAGE, "", NOW);
    assert_eq!(error_code(store.apply_verified_entry(bad)), "INVALID_INPUT");
    let bad = entry(CONVERSATION, MESSAGE, "正文", -1);
    assert_eq!(error_code(store.apply_verified_entry(bad)), "INVALID_INPUT");
    let mut bad = entry(CONVERSATION, MESSAGE, "正文", NOW);
    bad.title = "t".repeat(MAX_SESSION_TITLE_BYTES + 1);
    assert_eq!(error_code(store.apply_verified_entry(bad)), "INVALID_INPUT");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM messages"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM conversations"), 0);

    // 非本格式的 protected_metadata（由别处建立的会话）不被覆盖：显式 STATE_CORRUPTED。
    store
        .connection()
        .execute(
            "INSERT INTO conversations (conversation_id, kind, coordinator_id, protected_metadata, state)
             VALUES (?1, 'group', 'node-1', X'00', 'active')",
            [OTHER_CONVERSATION],
        )
        .unwrap();
    assert_eq!(
        error_code(store.apply_verified_entry(entry(OTHER_CONVERSATION, MESSAGE, "正文", NOW))),
        "STATE_CORRUPTED"
    );
    let metadata: Vec<u8> = store
        .connection()
        .query_row(
            "SELECT protected_metadata FROM conversations WHERE conversation_id = ?1",
            [OTHER_CONVERSATION],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(metadata, vec![0u8], "foreign metadata must not be overwritten");
}

#[test]
fn materialize_session_is_idempotent_and_bounds_the_preview() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let mut transaction = store
        .begin_transaction(WriteScope::intent(CONVERSATION))
        .expect("intent transaction");
    let summary: SessionSummary = materialize_session(&mut transaction, CONVERSATION, "草稿会话")
        .expect("materialize session");
    assert_eq!(summary.title, "草稿会话");
    assert!(summary.preview.is_empty());
    let again = materialize_session(&mut transaction, CONVERSATION, "另一个标题")
        .expect("materialize session is idempotent");
    assert_eq!(again.title, "草稿会话", "an existing title is not replaced");
    commit_transaction(transaction).expect("commit");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM conversations"), 1);

    // 预览按字符截断到上限，且不截断半个字符。
    let long_body: String = "消息预览".repeat(100);
    let applied: AppliedEntry = store
        .apply_verified_entry(entry(CONVERSATION, MESSAGE, &long_body, NOW))
        .expect("apply long body");
    assert_eq!(applied.body, long_body);
    let summary = store
        .session_summary(CONVERSATION)
        .expect("read summary")
        .expect("summary exists");
    assert_eq!(summary.preview.chars().count(), MAX_SESSION_PREVIEW_CHARS);
    assert!(long_body.starts_with(&summary.preview));
}
