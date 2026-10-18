//! 附件 FileKey 持久化（迁移 0007）集成测试：幂等保存、外键归属、密封与篡改、
//! 级联删除与有界输入。

use chat_storage::formal::{
    ATTACHMENT_FILE_ID_BYTES, ATTACHMENT_FILE_KEY_BYTES, AttachmentRecordInput,
    AttachmentSecretInput, KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store,
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
            "chat-att-secret-{tag}-{}",
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

fn attachment(attachment_id: &str) -> AttachmentRecordInput {
    AttachmentRecordInput {
        attachment_id: attachment_id.to_owned(),
        state: "imported".to_owned(),
        plain_size: 2048,
        chunk_size: 262_144,
        chunk_count: 1,
        content_digest: "ab".repeat(32),
        ciphertext_digest: "cd".repeat(32),
        display_name: "密钥测试.pdf".to_owned(),
        created_at_ms: 1_700_000_000_000,
    }
}

fn secret(attachment_id: &str, seed: u8) -> AttachmentSecretInput {
    AttachmentSecretInput {
        attachment_id: attachment_id.to_owned(),
        file_id: [seed; ATTACHMENT_FILE_ID_BYTES],
        file_key: vec![seed.wrapping_add(1); ATTACHMENT_FILE_KEY_BYTES],
        created_at_ms: 1_700_000_000_000,
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

#[test]
fn attachment_secret_roundtrip_is_idempotent() {
    let fixture = Fixture::new("roundtrip");
    let mut store = fixture.open();
    store.save_attachment(attachment("att-secret")).expect("save record");

    let saved = store.save_attachment_secret(secret("att-secret", 7)).expect("save secret");
    assert!(!saved.already_existed);
    assert_eq!(saved.file_id, [7u8; ATTACHMENT_FILE_ID_BYTES]);
    assert_eq!(saved.file_key.as_slice(), vec![8u8; ATTACHMENT_FILE_KEY_BYTES].as_slice());
    assert_eq!(saved.version, 1);

    // 同 file_id + 同密钥：幂等重放（不新增行、不改时间）。
    let replay = store.save_attachment_secret(secret("att-secret", 7)).expect("replay");
    assert!(replay.already_existed);
    assert_eq!(replay.updated_at_ms, saved.updated_at_ms);
    assert_eq!(rows(&store, "attachment_secrets"), 1);

    // 换密钥：显式冲突，绝不静默覆盖（否则缓存块永久无法解密）。
    let error = store
        .save_attachment_secret(secret("att-secret", 9))
        .unwrap_err();
    assert_eq!(error.code(), "OPERATION_ID_CONFLICT");
    // 同密钥换 file_id 也是冲突。
    let mut moved = secret("att-secret", 7);
    moved.file_id = [1u8; ATTACHMENT_FILE_ID_BYTES];
    assert_eq!(
        store.save_attachment_secret(moved).unwrap_err().code(),
        "OPERATION_ID_CONFLICT"
    );

    let loaded = store
        .load_attachment_secret("att-secret")
        .expect("load")
        .expect("present");
    assert_eq!(loaded.file_id, saved.file_id);
    assert_eq!(loaded.file_key.as_slice(), saved.file_key.as_slice());
    assert!(loaded.already_existed);
    assert!(store
        .load_attachment_secret("att-missing")
        .expect("load missing")
        .is_none());

    assert!(store.delete_attachment_secret("att-secret").expect("delete"));
    assert!(!store.delete_attachment_secret("att-secret").expect("delete again"));
    assert!(store.load_attachment_secret("att-secret").expect("load").is_none());
    // 记录本身仍在（只删密钥）。
    assert!(store.load_attachment("att-secret").expect("record").is_some());
}

#[test]
fn attachment_secret_requires_an_attachment_record() {
    let fixture = Fixture::new("foreign");
    let mut store = fixture.open();
    let error = store
        .save_attachment_secret(secret("att-missing", 3))
        .unwrap_err();
    assert_eq!(error.code(), "ATTACHMENT_NOT_FOUND");
    assert_eq!(rows(&store, "attachment_secrets"), 0);
}

#[test]
fn attachment_secret_is_sealed_and_tamper_evident() {
    let fixture = Fixture::new("sealed");
    let key_bytes = vec![0x5au8; ATTACHMENT_FILE_KEY_BYTES];
    let file_id = [0xa5u8; ATTACHMENT_FILE_ID_BYTES];
    {
        let mut store = fixture.open();
        store.save_attachment(attachment("att-sealed-secret")).expect("save record");
        store
            .save_attachment_secret(AttachmentSecretInput {
                attachment_id: "att-sealed-secret".to_owned(),
                file_id,
                file_key: key_bytes.clone(),
                created_at_ms: 1_700_000_000_000,
            })
            .expect("save secret");
        let loaded = store
            .load_attachment_secret("att-sealed-secret")
            .expect("load")
            .expect("present");
        assert_eq!(loaded.file_key.as_slice(), key_bytes.as_slice());
    }

    // 磁盘字节里不出现 FileKey 或 file_id 的明文。
    for (suffix, bytes) in fixture.store_files() {
        assert!(
            !contains(&bytes, &key_bytes),
            "file key leaked into {suffix} file"
        );
        assert!(
            !contains(&bytes, &file_id),
            "file id leaked into {suffix} file"
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
                "UPDATE attachment_secrets SET protected_secret = X'00ff' WHERE attachment_id = 'att-sealed-secret'",
                [],
            )
            .unwrap();
    }
    let store = fixture.open();
    assert_eq!(
        store
            .load_attachment_secret("att-sealed-secret")
            .unwrap_err()
            .code(),
        "STATE_CORRUPTED"
    );
    drop(store);

    // 把密封列挪到另一行（AAD 绑定 attachment_id）→ 认证失败。
    let fixture2 = Fixture::new("moved");
    {
        let mut store = fixture2.open();
        store.save_attachment(attachment("att-a")).expect("record a");
        store.save_attachment(attachment("att-b")).expect("record b");
        store.save_attachment_secret(secret("att-a", 1)).expect("secret a");
        let sealed: Vec<u8> = store
            .connection()
            .query_row(
                "SELECT protected_secret FROM attachment_secrets WHERE attachment_id = 'att-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO attachment_secrets
                     (attachment_id, version, protected_secret, created_at, updated_at)
                 VALUES ('att-b', 1, ?1, 1, 1)",
                [sealed],
            )
            .unwrap();
    }
    let store = fixture2.open();
    assert_eq!(
        store
            .load_attachment_secret("att-b")
            .unwrap_err()
            .code(),
        "STATE_CORRUPTED"
    );
    // 原行仍可读（没有把数据写坏）。
    assert!(store.load_attachment_secret("att-a").expect("load").is_some());
}

#[test]
fn deleting_an_attachment_cascades_its_secret() {
    let fixture = Fixture::new("cascade");
    let mut store = fixture.open();
    store.save_attachment(attachment("att-cascade")).expect("record");
    store.save_attachment_secret(secret("att-cascade", 4)).expect("secret");
    assert_eq!(rows(&store, "attachment_secrets"), 1);

    assert!(store.delete_attachment("att-cascade").expect("delete attachment"));
    assert_eq!(
        rows(&store, "attachment_secrets"),
        0,
        "ON DELETE CASCADE must remove the file key with its record"
    );
    assert!(store
        .load_attachment_secret("att-cascade")
        .expect("load")
        .is_none());
}

#[test]
fn attachment_secret_input_bounds_are_enforced() {
    let fixture = Fixture::new("bounds");
    let mut store = fixture.open();
    store.save_attachment(attachment("att-bounds")).expect("record");

    let mut short_key = secret("att-bounds", 1);
    short_key.file_key = vec![0u8; ATTACHMENT_FILE_KEY_BYTES - 1];
    assert_eq!(
        store.save_attachment_secret(short_key).unwrap_err().code(),
        "INVALID_INPUT"
    );
    let mut long_key = secret("att-bounds", 1);
    long_key.file_key = vec![0u8; ATTACHMENT_FILE_KEY_BYTES + 1];
    assert_eq!(
        store.save_attachment_secret(long_key).unwrap_err().code(),
        "INVALID_INPUT"
    );
    let mut negative = secret("att-bounds", 1);
    negative.created_at_ms = -1;
    assert_eq!(
        store.save_attachment_secret(negative).unwrap_err().code(),
        "INVALID_INPUT"
    );
    let mut bad_id = secret("bad id", 1);
    bad_id.attachment_id = "bad id".to_owned();
    assert_eq!(
        store.save_attachment_secret(bad_id).unwrap_err().code(),
        "INVALID_INPUT"
    );
    // 边界值可以接受。
    let mut maximum = secret("att-bounds", 1);
    maximum.file_key = vec![7u8; ATTACHMENT_FILE_KEY_BYTES];
    assert!(store.save_attachment_secret(maximum).is_ok());
}
