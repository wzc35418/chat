//! 附件传输进度（断点续传，迁移 0008）集成测试：方向隔离、幂等保存、位图校验、
//! 密封与篡改、位图数量与附件记录一致、外键级联清除。

use chat_storage::formal::{
    ATTACHMENT_DIRECTIONS, AttachmentProgressInput, AttachmentRecordInput, KeyProvenance,
    ProtectedStorageKey, Store, StoreOptions, open_store,
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
            "chat-att-progress-{tag}-{}",
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

    fn open(&self) -> Store {
        open_store(
            StoreOptions::new(&self.path, KeyProvenance::Raw32),
            self.key(),
        )
        .expect("open formal store")
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

fn rows(store: &Store, table: &str) -> i64 {
    store
        .connection()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
        .unwrap()
}

fn attachment(attachment_id: &str, chunk_count: u32) -> AttachmentRecordInput {
    AttachmentRecordInput {
        attachment_id: attachment_id.to_owned(),
        state: "imported".to_owned(),
        plain_size: u64::from(chunk_count) * 1024,
        chunk_size: 262_144,
        chunk_count,
        content_digest: "ab".repeat(32),
        ciphertext_digest: "cd".repeat(32),
        display_name: "进度测试.pdf".to_owned(),
        created_at_ms: 1_700_000_000_000,
    }
}

fn progress(attachment_id: &str, direction: &str, confirmed: &[bool]) -> AttachmentProgressInput {
    AttachmentProgressInput {
        attachment_id: attachment_id.to_owned(),
        direction: direction.to_owned(),
        chunk_count: confirmed.len() as u32,
        confirmed: confirmed.to_vec(),
        session_id: Some("sess-1".to_owned()),
        blob_id: Some("obj:1:demo".to_owned()),
        prefix_digest: None,
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

#[test]
fn attachment_progress_roundtrip_is_idempotent_and_directional() {
    let fixture = Fixture::new("roundtrip");
    let mut store = fixture.open();
    assert_eq!(
        store.schema_version(),
        14,
        "0008 progress + 0009 refs + 0010 plugins + 0011 credential directory + 0013 device registry"
    );
    store
        .save_attachment(attachment("att-progress", 3))
        .expect("save attachment");
    let _ = ATTACHMENT_DIRECTIONS;

    let saved = store
        .save_attachment_progress(progress("att-progress", "upload", &[true, false, false]))
        .expect("save progress");
    assert!(!saved.already_existed);
    assert_eq!(saved.confirmed_count(), 1);
    assert_eq!(saved.contiguous_prefix(), 1);
    assert!(!saved.is_complete());
    assert_eq!(saved.session_id.as_deref(), Some("sess-1"));

    // 幂等覆盖同一份内容。
    let replay = store
        .save_attachment_progress(progress("att-progress", "upload", &[true, false, false]))
        .expect("replay");
    assert!(replay.already_existed);
    assert_eq!(rows(&store, "attachment_transfer_progress"), 1);

    let loaded = store
        .load_attachment_progress("att-progress", "upload")
        .expect("load")
        .expect("present");
    assert_eq!(loaded.confirmed, vec![true, false, false]);
    assert!(loaded.already_existed);
    // 方向隔离：download 方向还没有进度。
    assert!(store
        .load_attachment_progress("att-progress", "download")
        .expect("load download")
        .is_none());

    // 推进进度：位图前移。
    let advanced = store
        .save_attachment_progress(progress("att-progress", "upload", &[true, true, false]))
        .expect("advance");
    assert_eq!(advanced.contiguous_prefix(), 2);
    // 完成：全 true。
    let complete = store
        .save_attachment_progress(progress("att-progress", "upload", &[true, true, true]))
        .expect("complete");
    assert!(complete.is_complete());
    assert_eq!(complete.confirmed_count(), 3);

    // 清除：幂等。
    assert!(store
        .clear_attachment_progress("att-progress", "upload")
        .expect("clear"));
    assert!(!store
        .clear_attachment_progress("att-progress", "upload")
        .expect("clear again"));
    assert!(store
        .load_attachment_progress("att-progress", "upload")
        .expect("load")
        .is_none());
}

#[test]
fn attachment_progress_bounds_are_enforced() {
    let fixture = Fixture::new("bounds");
    let mut store = fixture.open();
    store
        .save_attachment(attachment("att-bounds", 3))
        .expect("save attachment");

    // 位图长度必须等于 chunk_count。
    let mut short = progress("att-bounds", "upload", &[true, false]);
    short.chunk_count = 3;
    assert_eq!(
        store.save_attachment_progress(short).unwrap_err().code(),
        "INVALID_INPUT"
    );
    // 方向白名单。
    let mut sideways = progress("att-bounds", "upload", &[true, false, false]);
    sideways.direction = "sideways".to_owned();
    assert_eq!(
        store.save_attachment_progress(sideways).unwrap_err().code(),
        "INVALID_INPUT"
    );
    assert_eq!(
        store
            .load_attachment_progress("att-bounds", "sideways")
            .unwrap_err()
            .code(),
        "INVALID_INPUT"
    );
    // chunk_count 必须与附件记录一致。
    let mut mismatched = progress("att-bounds", "upload", &[true, false, false, false]);
    mismatched.chunk_count = 4;
    assert_eq!(
        store.save_attachment_progress(mismatched).unwrap_err().code(),
        "INVALID_INPUT"
    );
    // 会话/blob 标识有界。
    let mut empty_session = progress("att-bounds", "upload", &[true, false, false]);
    empty_session.session_id = Some(String::new());
    assert_eq!(
        store.save_attachment_progress(empty_session).unwrap_err().code(),
        "INVALID_INPUT"
    );
    let mut long_blob = progress("att-bounds", "upload", &[true, false, false]);
    long_blob.blob_id = Some("x".repeat(257));
    assert_eq!(
        store.save_attachment_progress(long_blob).unwrap_err().code(),
        "INVALID_INPUT"
    );
    // 前缀摘要必须是小写 hex。
    let mut bad_digest = progress("att-bounds", "download", &[true, false, false]);
    bad_digest.prefix_digest = Some("zz".repeat(32));
    assert_eq!(
        store.save_attachment_progress(bad_digest).unwrap_err().code(),
        "INVALID_INPUT"
    );
    // 不存在的附件记录。
    assert_eq!(
        store
            .save_attachment_progress(progress("att-missing", "upload", &[true]))
            .unwrap_err()
            .code(),
        "ATTACHMENT_NOT_FOUND"
    );
    assert_eq!(rows(&store, "attachment_transfer_progress"), 0);
}

#[test]
fn attachment_progress_is_sealed_and_tamper_evident() {
    let fixture = Fixture::new("sealed");
    {
        let mut store = fixture.open();
        store
            .save_attachment(attachment("att-sealed", 2))
            .expect("save attachment");
        let mut input = progress("att-sealed", "upload", &[true, false]);
        input.session_id = Some("sess-SECRET-7f3a91".to_owned());
        store.save_attachment_progress(input).expect("save progress");
    }
    // 会话标识不明文落盘。
    for (suffix, bytes) in fixture.store_files() {
        assert!(
            !contains(&bytes, b"sess-SECRET-7f3a91"),
            "upload session leaked into {suffix}"
        );
        assert!(
            !contains(&bytes, b"obj:1:demo"),
            "blob id leaked into {suffix}"
        );
    }
    // 错误主密钥：打开即拒绝。
    assert_eq!(
        match open_store(
            StoreOptions::new(&fixture.path, KeyProvenance::Raw32),
            fixture.other_key()
        ) {
            Ok(_) => panic!("wrong key must be rejected"),
            Err(error) => error.code(),
        },
        "STATE_CORRUPTED"
    );

    // 篡改密封列 → STATE_CORRUPTED。
    {
        let connection = fixture.raw();
        connection
            .execute(
                "UPDATE attachment_transfer_progress SET protected_progress = X'00ff' WHERE attachment_id = 'att-sealed'",
                [],
            )
            .unwrap();
    }
    let store = fixture.open();
    assert_eq!(
        store
            .load_attachment_progress("att-sealed", "upload")
            .unwrap_err()
            .code(),
        "STATE_CORRUPTED"
    );
    drop(store);

    // 把密封列挪到另一方向（AAD 绑定 direction）→ 认证失败。
    let fixture2 = Fixture::new("moved");
    {
        let mut store = fixture2.open();
        store
            .save_attachment(attachment("att-mv", 2))
            .expect("save attachment");
        store
            .save_attachment_progress(progress("att-mv", "upload", &[true, true]))
            .expect("save progress");
        let sealed: Vec<u8> = store
            .connection()
            .query_row(
                "SELECT protected_progress FROM attachment_transfer_progress
                  WHERE attachment_id = 'att-mv' AND direction = 'upload'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO attachment_transfer_progress
                     (attachment_id, direction, chunk_count, protected_progress, updated_at)
                 VALUES ('att-mv', 'download', 2, ?1, 1)",
                [sealed],
            )
            .unwrap();
    }
    let store = fixture2.open();
    assert_eq!(
        store
            .load_attachment_progress("att-mv", "download")
            .unwrap_err()
            .code(),
        "STATE_CORRUPTED"
    );
    assert!(store
        .load_attachment_progress("att-mv", "upload")
        .expect("original still readable")
        .is_some());
}

#[test]
fn attachment_progress_rejects_mismatched_bitmap_and_chunk_count() {
    let fixture = Fixture::new("mismatch");
    let mut store = fixture.open();
    store
        .save_attachment(attachment("att-x", 3))
        .expect("save attachment");
    store
        .save_attachment_progress(progress("att-x", "upload", &[true, true, false]))
        .expect("save progress");

    // 位图列 chunk_count 被改写：与附件记录不一致 → STATE_CORRUPTED。
    {
        let connection = store.connection();
        connection
            .execute(
                "UPDATE attachment_transfer_progress SET chunk_count = 2
                  WHERE attachment_id = 'att-x' AND direction = 'upload'",
                [],
            )
            .expect("tamper chunk count");
    }
    assert_eq!(
        store
            .load_attachment_progress("att-x", "upload")
            .unwrap_err()
            .code(),
        "STATE_CORRUPTED"
    );
    // 清除后重新写入，再篡改位图为长度不足的字节串。
    assert!(store
        .clear_attachment_progress("att-x", "upload")
        .expect("clear"));
    store
        .save_attachment_progress(progress("att-x", "upload", &[true, false, false]))
        .expect("save again");
    // 通过密封重写不可能，这里改用“删除附件记录后进度不匹配”的路径：
    // 把 attachments.chunk_count 改成 4（原始记录 3）→ 进度与记录不符。
    {
        let guard = store;
        guard
            .connection()
            .execute(
                "UPDATE attachments SET chunk_count = 4 WHERE attachment_id = 'att-x'",
                [],
            )
            .expect("tamper attachment chunk count");
        assert_eq!(
            guard
                .load_attachment_progress("att-x", "upload")
                .unwrap_err()
                .code(),
            "STATE_CORRUPTED"
        );
    }
}

#[test]
fn deleting_an_attachment_clears_its_progress() {
    let fixture = Fixture::new("cascade");
    let mut store = fixture.open();
    store
        .save_attachment(attachment("att-cascade", 3))
        .expect("save attachment");
    store
        .save_attachment_progress(progress("att-cascade", "upload", &[true, false, false]))
        .expect("upload progress");
    store
        .save_attachment_progress(progress("att-cascade", "download", &[true, true, false]))
        .expect("download progress");
    assert_eq!(rows(&store, "attachment_transfer_progress"), 2);
    assert!(store
        .delete_attachment("att-cascade")
        .expect("delete attachment"));
    assert_eq!(
        rows(&store, "attachment_transfer_progress"),
        0,
        "ON DELETE CASCADE must clear both directions"
    );
}

#[test]
fn purge_stale_transfer_progress_removes_only_expired_entries() {
    let fixture = Fixture::new("purge");
    let mut store = fixture.open();
    store
        .save_attachment(attachment("att-old", 3))
        .expect("old attachment");
    store
        .save_attachment(attachment("att-new", 3))
        .expect("new attachment");
    store
        .save_attachment_progress(progress("att-old", "upload", &[true, false, false]))
        .expect("old progress");
    store
        .save_attachment_progress(progress("att-new", "upload", &[true, true, false]))
        .expect("new progress");
    store
        .save_attachment_progress(progress("att-new", "download", &[true, false, false]))
        .expect("new download progress");

    // 把 att-old 的进度改成 10 分钟前（其余保持刚写入）。
    let stale = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64
        - 600_000;
    store
        .connection()
        .execute(
            "UPDATE attachment_transfer_progress SET updated_at = ?1
              WHERE attachment_id = 'att-old'",
            [stale],
        )
        .expect("backdate progress");

    // max_age 必须为正。
    assert_eq!(
        store.purge_stale_transfer_progress(0).unwrap_err().code(),
        "INVALID_INPUT"
    );
    assert_eq!(
        store
            .purge_stale_transfer_progress(60_000)
            .expect("purge stale"),
        1,
        "only the stale entry is removed"
    );
    assert!(store
        .load_attachment_progress("att-old", "upload")
        .expect("load old")
        .is_none());
    assert!(store
        .load_attachment_progress("att-new", "upload")
        .expect("load new")
        .is_some());
    assert!(store
        .load_attachment_progress("att-new", "download")
        .expect("load new download")
        .is_some());

    // max_age 大于当前时间意味着“没有条目足够旧”：一条都不清（不是“清全部”）。
    assert_eq!(
        store
            .purge_stale_transfer_progress(10 * 365 * 24 * 3600 * 1000)
            .expect("nothing is older than that"),
        0
    );
    // 把剩余两条也改成过期后，一次清掉。
    store
        .connection()
        .execute(
            "UPDATE attachment_transfer_progress SET updated_at = ?1",
            [stale],
        )
        .expect("backdate the rest");
    assert_eq!(
        store.purge_stale_transfer_progress(60_000).expect("purge all"),
        2
    );
    assert_eq!(rows(&store, "attachment_transfer_progress"), 0);
    // 空表上再次清理是幂等的。
    assert_eq!(
        store.purge_stale_transfer_progress(1_000).expect("purge empty"),
        0
    );
}
