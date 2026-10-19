//! 附件记录正式持久化（X 包）集成测试：幂等保存、白名单状态机、密封与篡改、
//! 有界输入、记录上限与删除。

use chat_storage::formal::{
    ATTACHMENT_STATES, AttachmentRecordInput, KeyProvenance, MAX_ATTACHMENTS,
    MAX_ATTACHMENT_CHUNK_SIZE, MAX_ATTACHMENT_CHUNKS, MAX_ATTACHMENT_LIST,
    MAX_ATTACHMENT_PLAIN_BYTES, ProtectedStorageKey, Store, StoreOptions, open_store,
};
use rusqlite::Connection;
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
            "chat-att-records-{tag}-{}",
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

    fn raw(&self) -> Connection {
        Connection::open(&self.path).expect("open raw sqlite connection")
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

fn input(attachment_id: &str, state: &str) -> AttachmentRecordInput {
    AttachmentRecordInput {
        attachment_id: attachment_id.to_owned(),
        state: state.to_owned(),
        plain_size: 4096,
        chunk_size: 262_144,
        chunk_count: 1,
        content_digest: "ab".repeat(32),
        ciphertext_digest: "cd".repeat(32),
        display_name: "会议纪要.pdf".to_owned(),
        created_at_ms: 1_700_000_000_000,
    }
}

/// 在 Store 仍持有独占锁时只能用它的连接读数（不允许再开第二个连接）。
fn rows(store: &Store, table: &str) -> i64 {
    store
        .connection()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
        .unwrap()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

#[test]
fn attachment_record_roundtrip_is_idempotent_and_ordered() {
    let fixture = Fixture::new("roundtrip");
    let mut store = fixture.open();
    assert_eq!(
        store.schema_version(),
        13,
        "0005 attachments, 0006 device credentials, 0007 secrets, 0008 progress, 0009 refs, 0010 plugins, 0011 credential directory, 0012 attachment gc, 0013 device registry"
    );

    let saved = store.save_attachment(input("att-1234", "staging")).expect("save");
    assert!(!saved.already_existed);
    assert_eq!(saved.attachment_id, "att-1234");
    assert_eq!(saved.content_digest, "ab".repeat(32));
    assert_eq!(saved.display_name, "会议纪要.pdf");
    assert!(saved.updated_at_ms >= saved.created_at_ms);

    // 同内容重复保存：幂等，不覆盖状态、不新增行。
    let replay = store.save_attachment(input("att-1234", "imported")).expect("replay");
    assert!(replay.already_existed);
    assert_eq!(replay.state, "staging", "save never advances the state machine");
    assert_eq!(replay.created_at_ms, saved.created_at_ms);

    // 同 ID 换内容：显式冲突，不静默覆盖。
    let mut changed = input("att-1234", "staging");
    changed.content_digest = "ef".repeat(32);
    assert_eq!(
        store.save_attachment(changed).unwrap_err().code(),
        "OPERATION_ID_CONFLICT"
    );

    let loaded = store.load_attachment("att-1234").expect("load").expect("present");
    assert_eq!(loaded.content_digest, saved.content_digest);
    assert!(loaded.already_existed);
    assert!(store.load_attachment("att-missing").expect("load missing").is_none());

    // 另一条记录按创建时间倒序排列。
    let mut second = input("att-5678", "imported");
    second.created_at_ms = 1_700_000_001_000;
    store.save_attachment(second).expect("save second");
    let listed = store.list_attachments(10).expect("list");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].attachment_id, "att-5678");
    assert_eq!(listed[1].attachment_id, "att-1234");

    // 列表上限有界。
    assert_eq!(store.list_attachments(0).unwrap_err().code(), "INVALID_INPUT");
    assert_eq!(
        store
            .list_attachments(MAX_ATTACHMENT_LIST + 1)
            .unwrap_err()
            .code(),
        "INVALID_INPUT"
    );
    assert_eq!(rows(&store, "attachments"), 2);
}

#[test]
fn attachment_state_machine_is_whitelisted() {
    let fixture = Fixture::new("states");
    let mut store = fixture.open();
    store.save_attachment(input("att-state", "staging")).expect("save");

    let imported = store
        .mark_attachment_state("att-state", "imported")
        .expect("staging -> imported");
    assert_eq!(imported.state, "imported");
    // 同状态重复标记幂等。
    let again = store
        .mark_attachment_state("att-state", "imported")
        .expect("idempotent mark");
    assert_eq!(again.state, "imported");
    assert_eq!(again.updated_at_ms, imported.updated_at_ms);

    // 白名单内的迁移可用。
    store
        .mark_attachment_state("att-state", "uploading")
        .expect("imported -> uploading");
    store
        .mark_attachment_state("att-state", "available")
        .expect("uploading -> available");
    store
        .mark_attachment_state("att-state", "downloading")
        .expect("available -> downloading");
    store
        .mark_attachment_state("att-state", "cancelled")
        .expect("downloading -> cancelled");

    // 终态不可再迁移；未知状态与未知记录显式拒绝。
    assert_eq!(
        store
            .mark_attachment_state("att-state", "imported")
            .unwrap_err()
            .code(),
        "INVALID_STATE_TRANSITION"
    );
    assert_eq!(
        store
            .mark_attachment_state("att-state", "bogus")
            .unwrap_err()
            .code(),
        "INVALID_INPUT"
    );
    assert_eq!(
        store
            .mark_attachment_state("att-missing", "imported")
            .unwrap_err()
            .code(),
        "ATTACHMENT_NOT_FOUND"
    );
    // 状态白名单常量覆盖 schema 允许的全部状态。
    assert_eq!(ATTACHMENT_STATES.len(), 7);
    assert!(ATTACHMENT_STATES.contains(&"cancelled"));
}

#[test]
fn stored_attachment_metadata_is_sealed_and_tamper_evident() {
    let fixture = Fixture::new("sealed");
    let marker_name = format!("绝密附件-{}.pdf", uuid::Uuid::new_v4());
    let marker_digest = format!("{:064x}", 0xdead_beefu64);
    {
        let mut store = fixture.open();
        let mut record = input("att-sealed", "imported");
        record.display_name = marker_name.clone();
        record.content_digest = marker_digest.clone();
        store.save_attachment(record).expect("save");
        assert_eq!(store.load_attachment("att-sealed").unwrap().unwrap().display_name, marker_name);
    }

    // 磁盘上不出现明文文件名/摘要（它们只在密封列里）。
    for (suffix, bytes) in fixture.store_files() {
        assert!(
            !contains(&bytes, marker_name.as_bytes()),
            "display name leaked into {suffix} file"
        );
        assert!(
            !contains(&bytes, marker_digest.as_bytes()),
            "content digest leaked into {suffix} file"
        );
    }

    // 错误主密钥：打开即拒绝，不覆盖数据。
    assert_eq!(
        match open_store(fixture.options(), fixture.other_key()) {
            Ok(_) => panic!("wrong key must be rejected"),
            Err(error) => error.code(),
        },
        "STATE_CORRUPTED"
    );

    // 篡改密封列：读取显式 STATE_CORRUPTED。
    {
        let connection = fixture.raw();
        connection
            .execute(
                "UPDATE attachments SET protected_descriptor = X'00ff00ff' WHERE attachment_id = 'att-sealed'",
                [],
            )
            .unwrap();
    }
    let store = fixture.open();
    assert_eq!(
        store.load_attachment("att-sealed").unwrap_err().code(),
        "STATE_CORRUPTED"
    );
    assert_eq!(store.list_attachments(10).unwrap_err().code(), "STATE_CORRUPTED");
    drop(store);

    // 把密封列挪到另一行（AAD 绑定 attachment_id）：读取同样失败。
    let fixture = Fixture::new("moved");
    {
        let mut store = fixture.open();
        store.save_attachment(input("att-source", "imported")).expect("save");
        let sealed: Vec<u8> = store
            .connection()
            .query_row(
                "SELECT protected_descriptor FROM attachments WHERE attachment_id = 'att-source'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO attachments
                     (attachment_id, protected_descriptor, plain_size, chunk_count, state, chunk_size, created_at, updated_at)
                 VALUES ('att-moved', ?1, 4096, 1, 'imported', 262144, 1, 1)",
                [sealed],
            )
            .unwrap();
    }
    let store = fixture.open();
    assert_eq!(
        store.load_attachment("att-moved").unwrap_err().code(),
        "STATE_CORRUPTED"
    );
    // 原行仍然可读（没有把数据写坏）。
    assert!(store.load_attachment("att-source").unwrap().is_some());
}

#[test]
fn attachment_input_bounds_are_enforced() {
    let fixture = Fixture::new("bounds");
    let mut store = fixture.open();

    // 明文大小 / 块大小 / 块数。
    let mut oversized = input("att-big", "imported");
    oversized.plain_size = MAX_ATTACHMENT_PLAIN_BYTES + 1;
    assert_eq!(store.save_attachment(oversized).unwrap_err().code(), "INVALID_INPUT");
    let mut zero_chunk = input("att-zero-chunk", "imported");
    zero_chunk.chunk_size = 0;
    assert_eq!(store.save_attachment(zero_chunk).unwrap_err().code(), "INVALID_INPUT");
    let mut big_chunk = input("att-big-chunk", "imported");
    big_chunk.chunk_size = MAX_ATTACHMENT_CHUNK_SIZE + 1;
    assert_eq!(store.save_attachment(big_chunk).unwrap_err().code(), "INVALID_INPUT");
    let mut zero_count = input("att-zero-count", "imported");
    zero_count.chunk_count = 0;
    assert_eq!(store.save_attachment(zero_count).unwrap_err().code(), "INVALID_INPUT");
    let mut many_chunks = input("att-many-chunks", "imported");
    many_chunks.chunk_count = MAX_ATTACHMENT_CHUNKS + 1;
    assert_eq!(store.save_attachment(many_chunks).unwrap_err().code(), "INVALID_INPUT");

    // 摘要必须是 64 位小写 hex。
    let mut short_digest = input("att-short", "imported");
    short_digest.content_digest = "ab".repeat(31);
    assert_eq!(store.save_attachment(short_digest).unwrap_err().code(), "INVALID_INPUT");
    let mut upper_digest = input("att-upper", "imported");
    upper_digest.ciphertext_digest = "AB".repeat(32);
    assert_eq!(store.save_attachment(upper_digest).unwrap_err().code(), "INVALID_INPUT");

    // 文件名/ID/状态/时间戳。
    let mut empty_name = input("att-empty-name", "imported");
    empty_name.display_name = String::new();
    assert_eq!(store.save_attachment(empty_name).unwrap_err().code(), "INVALID_INPUT");
    let mut control_name = input("att-control-name", "imported");
    control_name.display_name = "bad\nname".to_owned();
    assert_eq!(store.save_attachment(control_name).unwrap_err().code(), "INVALID_INPUT");
    let mut long_name = input("att-long-name", "imported");
    long_name.display_name = "x".repeat(201);
    assert_eq!(store.save_attachment(long_name).unwrap_err().code(), "INVALID_INPUT");
    let mut bad_state = input("att-bad-state", "collecting");
    bad_state.state = "collecting".to_owned();
    assert_eq!(store.save_attachment(bad_state).unwrap_err().code(), "INVALID_INPUT");
    let mut bad_id = input("", "imported");
    bad_id.attachment_id = String::new();
    assert_eq!(store.save_attachment(bad_id).unwrap_err().code(), "INVALID_INPUT");
    let mut negative_time = input("att-negative", "imported");
    negative_time.created_at_ms = -1;
    assert_eq!(store.save_attachment(negative_time).unwrap_err().code(), "INVALID_INPUT");

    assert_eq!(rows(&store, "attachments"), 0);
    assert_eq!(store.list_attachments(10).unwrap().len(), 0);
}

#[test]
fn attachment_limit_and_delete() {
    let fixture = Fixture::new("limit-delete");
    {
        let mut store = fixture.open();
        store.save_attachment(input("att-a", "imported")).expect("save a");
        store.save_attachment(input("att-b", "imported")).expect("save b");
        assert!(store.delete_attachment("att-a").expect("delete a"));
        assert!(!store.delete_attachment("att-a").expect("delete again"));
        assert!(store.load_attachment("att-a").expect("load").is_none());
        assert!(store.load_attachment("att-b").expect("load").is_some());
        assert_eq!(rows(&store, "attachments"), 1);
    }

    // 记录上限：直接批量写入到上限，再保存必须显式拒绝（不静默丢弃）。
    {
        let connection = fixture.raw();
        let transaction = connection.unchecked_transaction().expect("begin");
        for index in 0..MAX_ATTACHMENTS {
            transaction
                .execute(
                    "INSERT INTO attachments
                         (attachment_id, protected_descriptor, plain_size, chunk_count, state, chunk_size, created_at, updated_at)
                     VALUES (?1, X'00', 1, 1, 'imported', 1, ?2, ?2)",
                    rusqlite::params![format!("att-bulk-{index:05}"), i64::from(index)],
                )
                .expect("bulk insert");
        }
        transaction.commit().expect("commit bulk");
    }
    let mut store = fixture.open();
    // 批量行是“未密封的占位行”（只为占满条数），读取必须显式 STATE_CORRUPTED，
    // 不允许静默跳过坏记录。
    assert_eq!(
        store.list_attachments(10).unwrap_err().code(),
        "STATE_CORRUPTED"
    );
    // 到达上限后新增记录显式拒绝，不静默丢弃。
    assert_eq!(
        store
            .save_attachment(input("att-over-limit", "imported"))
            .unwrap_err()
            .code(),
        "INVALID_INPUT"
    );
}

#[test]
fn attachment_migration_is_applied_once_and_keeps_previous_tables() {
    let fixture = Fixture::new("migration");
    {
        let store = fixture.open();
        assert_eq!(store.schema_version(), 13);
    }
    let store = fixture.open();
    assert_eq!(store.schema_version(), 13);
    assert_eq!(rows(&store, "schema_migrations"), 13);
    // 0001~0004 的既有表仍可用（这里抽查 0004 的 MLS 状态表与 0002 的密钥校验表）。
    assert_eq!(rows(&store, "mls_group_state"), 0);
    assert_eq!(rows(&store, "store_key_check"), 1);
}

/// 回收计划的 outbox 引用护栏：`outbox_jobs.payload_ref` 指向的附件必须保留，
/// 否则进行中的上传/投递任务会丢掉自己的附件记录。
#[test]
fn attachment_gc_plan_keeps_attachments_referenced_by_outbox_jobs() {
    let fixture = Fixture::new("gc-outbox");
    let attachment_id = "att-00112233445566778899aabbccddeeff";
    {
        let mut store = fixture.open();
        store
            .save_attachment(input(attachment_id, "imported"))
            .expect("save attachment");
    }
    {
        // 直接写一条 outbox 任务指向该附件（真实写路径需要完整发送请求，这里只验护栏 SQL）。
        let connection = fixture.raw();
        connection
            .execute(
                "INSERT INTO outbox_jobs (job_id, dedup_key, kind, payload_ref, conversation_id, depends_on, state, retry_count, retry_at, lease_owner, lease_until, lease_version, cancel_requested) VALUES ('job-1', 'dedup-1', 'attachment_upload', ?1, NULL, NULL, 'pending', 0, 0, NULL, 0, 0, 0)",
                [attachment_id],
            )
            .expect("insert outbox job");
    }
    let store = fixture.open();
    let plan = store
        .plan_attachment_gc(1_800_000_000_000, 0, 16)
        .expect("plan");
    assert!(
        plan.candidates.is_empty(),
        "outbox-referenced attachments must be kept"
    );
    assert_eq!(plan.skipped_outbox, 1, "the outbox reference is the reason it is kept");
    assert_eq!(plan.skipped_within_grace, 0, "the attachment is past the grace window");
    assert!(store.pending_attachment_gc(16).expect("pending").is_empty());
}
