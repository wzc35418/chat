//! MLS 群状态与 provider 记录的正式存储测试。
//!
//! 覆盖：往返读写、epoch 单调（旧 epoch 不得覆盖新 epoch）、epoch 前进时旧记录退役、
//! 记录级读写与 epoch 校验、错误主密钥/篡改/挪行 STATE_CORRUPTED、明文扫描、有界拒绝。

use chat_storage::formal::{
    KeyProvenance, MAX_MLS_RECORD_KEY_BYTES, MlsGroupStateInput, MlsRecordInput,
    ProtectedStorageKey, StorageError, Store, StoreOptions, WriteScope, commit_transaction,
    open_store,
};
use std::fs;
use std::path::PathBuf;

const GROUP: &str = "group-1";

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
    key_bytes: [u8; 32],
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "chat-formal-mls-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
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

    fn open(&self) -> Store {
        open_store(
            StoreOptions::new(&self.path, KeyProvenance::Raw32),
            self.key(),
        )
        .expect("open formal store")
    }

    fn store_files(&self) -> Vec<(String, Vec<u8>)> {
        let mut files = Vec::new();
        for suffix in ["", "-wal"] {
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

fn error_code<T>(result: Result<T, StorageError>) -> &'static str {
    match result {
        Ok(_) => panic!("expected an explicit failure"),
        Err(error) => error.code(),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

fn snapshot(epoch: u64, state: &str, records: &[(&str, &str)]) -> MlsGroupStateInput {
    MlsGroupStateInput {
        group_id: GROUP.to_owned(),
        epoch,
        state: state.as_bytes().to_vec(),
        records: records
            .iter()
            .map(|(key, value)| MlsRecordInput {
                record_key: key.as_bytes().to_vec(),
                value: value.as_bytes().to_vec(),
            })
            .collect(),
    }
}

#[test]
fn mls_group_state_round_trips_and_epoch_is_monotonic() {
    let fixture = Fixture::new("round-trip");
    let mut store = fixture.open();
    assert!(
        store
            .load_mls_group_state(GROUP)
            .expect("load empty")
            .is_none()
    );

    let first = store
        .save_mls_group_state(snapshot(
            1,
            "group-state-epoch-1",
            &[("group-state", "secret-epoch-1"), ("own-leaf", "leaf-1")],
        ))
        .expect("save epoch 1");
    assert_eq!(first.epoch, 1);
    assert_eq!(first.state_version, 1);
    assert_eq!(first.state, b"group-state-epoch-1");
    let loaded = store
        .load_mls_group_state(GROUP)
        .expect("load")
        .expect("present");
    assert_eq!(loaded, first);
    let records = store.load_mls_records(GROUP).expect("records");
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].epoch, 1);
    assert_eq!(
        store
            .load_mls_record(GROUP, b"group-state")
            .expect("record")
            .expect("present")
            .value,
        b"secret-epoch-1"
    );

    // 同 epoch 重复保存 = 快照替换，版本递增。
    let second = store
        .save_mls_group_state(snapshot(1, "group-state-epoch-1b", &[("own-leaf", "leaf-1b")]))
        .expect("replace same epoch");
    assert_eq!(second.state_version, 2);
    assert_eq!(store.load_mls_records(GROUP).expect("records").len(), 1);

    // epoch 前进：旧记录整组退役。
    let advanced = store
        .save_mls_group_state(snapshot(
            2,
            "group-state-epoch-2",
            &[("group-state", "secret-epoch-2")],
        ))
        .expect("advance epoch");
    assert_eq!(advanced.state_version, 3);
    assert!(
        store
            .load_mls_record(GROUP, b"own-leaf")
            .expect("stale record is gone")
            .is_none()
    );
    let records = store.load_mls_records(GROUP).expect("records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].epoch, 2);
    assert_eq!(records[0].value, b"secret-epoch-2");

    // 旧 epoch 不得覆盖新 epoch。
    let stale = store
        .save_mls_group_state(snapshot(1, "stale", &[("group-state", "stale-secret")]));
    assert_eq!(error_code(stale), "STALE_EPOCH");
    assert_eq!(
        store
            .load_mls_group_state(GROUP)
            .expect("load")
            .expect("present")
            .epoch,
        2,
        "stale save must not overwrite"
    );
    drop(store);

    // 重开（同主密钥）读回同一状态。
    let store = fixture.open();
    let reopened = store
        .load_mls_group_state(GROUP)
        .expect("load")
        .expect("present");
    assert_eq!(reopened.epoch, 2);
    assert_eq!(reopened.state, b"group-state-epoch-2");
    assert_eq!(
        store
            .load_mls_record(GROUP, b"group-state")
            .expect("record")
            .expect("present")
            .value,
        b"secret-epoch-2"
    );
}

#[test]
fn mls_record_updates_are_epoch_checked() {
    let fixture = Fixture::new("record-epoch");
    let mut store = fixture.open();
    // 未初始化群：显式拒绝。
    assert_eq!(
        error_code(store.save_mls_record(GROUP, 1, b"record", b"value")),
        "INVALID_INPUT"
    );
    store
        .save_mls_group_state(snapshot(2, "state", &[]))
        .expect("save group");

    let saved = store
        .save_mls_record(GROUP, 2, b"record-1", b"value-1")
        .expect("save record");
    assert_eq!(saved.epoch, 2);
    // 同 epoch 覆盖更新。
    store
        .save_mls_record(GROUP, 2, b"record-1", b"value-2")
        .expect("update record");
    assert_eq!(
        store
            .load_mls_record(GROUP, b"record-1")
            .expect("load")
            .expect("present")
            .value,
        b"value-2"
    );
    // 旧 epoch 记录写入被拒绝。
    assert_eq!(
        error_code(store.save_mls_record(GROUP, 1, b"record-2", b"value")),
        "STALE_EPOCH"
    );
    // 未保存群状态就直接用更高 epoch 写记录被拒绝（必须先写群状态）。
    assert_eq!(
        error_code(store.save_mls_record(GROUP, 3, b"record-2", b"value")),
        "INVALID_INPUT"
    );
    // epoch 前进后，旧记录读不出来（先写群状态会让记录退役）。
    store
        .save_mls_group_state(snapshot(3, "state-3", &[]))
        .expect("advance");
    assert!(
        store
            .load_mls_record(GROUP, b"record-1")
            .expect("old record gone")
            .is_none()
    );
}

#[test]
fn mls_state_rejects_unbounded_input() {
    let fixture = Fixture::new("bounds");
    let mut store = fixture.open();
    let mut too_many = snapshot(1, "state", &[]);
    too_many.records = (0..4097)
        .map(|index| MlsRecordInput {
            record_key: index.to_string().into_bytes(),
            value: b"v".to_vec(),
        })
        .collect();
    assert_eq!(
        error_code(store.save_mls_group_state(too_many)),
        "INVALID_INPUT"
    );

    let long_key = vec![0x41u8; MAX_MLS_RECORD_KEY_BYTES + 1];
    let mut oversized = snapshot(1, "state", &[]);
    oversized.records.push(MlsRecordInput {
        record_key: long_key,
        value: b"v".to_vec(),
    });
    assert_eq!(
        error_code(store.save_mls_group_state(oversized)),
        "INVALID_INPUT"
    );

    let mut duplicate = snapshot(1, "state", &[("key", "a")]);
    duplicate.records.push(MlsRecordInput {
        record_key: b"key".to_vec(),
        value: b"b".to_vec(),
    });
    assert_eq!(
        error_code(store.save_mls_group_state(duplicate)),
        "INVALID_INPUT"
    );

    let mut empty_state = snapshot(1, "", &[]);
    empty_state.state.clear();
    assert_eq!(
        error_code(store.save_mls_group_state(empty_state)),
        "INVALID_INPUT"
    );

    // 非法群 ID 与畸形记录键。
    let mut bad_group = snapshot(1, "state", &[]);
    bad_group.group_id = "bad group id!".to_owned();
    assert_eq!(
        error_code(store.save_mls_group_state(bad_group)),
        "INVALID_INPUT"
    );
    assert_eq!(
        error_code(store.load_mls_record(GROUP, &[])),
        "INVALID_INPUT"
    );
    assert_eq!(
        error_code(store.load_mls_records("bad id!")),
        "INVALID_INPUT"
    );
    // 身份作用域不能写 MLS 表是设计约束，但作用域本身要能正常开启并提交。
    let transaction = store.begin_transaction(WriteScope::mls_state()).expect("tx");
    let count: i64 = transaction
        .connection()
        .query_row("SELECT COUNT(*) FROM mls_group_state", [], |row| row.get(0))
        .expect("count");
    assert_eq!(count, 0);
    commit_transaction(transaction).expect("commit empty mls tx");
}

#[test]
fn wrong_key_and_tampered_mls_state_are_state_corrupted() {
    let fixture = Fixture::new("tamper");
    let mut store = fixture.open();
    store
        .save_mls_group_state(snapshot(
            1,
            "group-state-1",
            &[("group-state", "secret-1"), ("leaf", "leaf-1")],
        ))
        .expect("save");
    drop(store);

    // 错误主密钥：拒绝打开且不覆盖数据。
    let error = match open_store(
        StoreOptions::new(&fixture.path, KeyProvenance::Raw32),
        fixture.other_key(),
    ) {
        Ok(_) => panic!("wrong key must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "STATE_CORRUPTED");

    let store = fixture.open();
    // 篡改群状态密封记录。
    store
        .connection()
        .execute(
            "UPDATE mls_group_state SET protected_state = X'00ff00ff' WHERE group_id = ?1",
            [GROUP],
        )
        .expect("tamper state");
    assert_eq!(
        error_code(store.load_mls_group_state(GROUP)),
        "STATE_CORRUPTED"
    );
}

#[test]
fn relocated_record_ciphertext_is_rejected() {
    let fixture = Fixture::new("relocate");
    let mut store = fixture.open();
    store
        .save_mls_group_state(snapshot(
            1,
            "group-state-1",
            &[("record-a", "secret-a"), ("record-b", "secret-b")],
        ))
        .expect("save");
    // 把 record-a 的密文挪到 record-b：AAD 绑定记录键，必须认证失败。
    let sealed: Vec<u8> = store
        .connection()
        .query_row(
            "SELECT protected_value FROM mls_record WHERE group_id = ?1 AND record_key = ?2",
            rusqlite::params![GROUP, b"record-a".to_vec()],
            |row| row.get(0),
        )
        .expect("read sealed");
    store
        .connection()
        .execute(
            "UPDATE mls_record SET protected_value = ?3 WHERE group_id = ?1 AND record_key = ?2",
            rusqlite::params![GROUP, b"record-b".to_vec(), sealed],
        )
        .expect("relocate");
    assert_eq!(
        error_code(store.load_mls_record(GROUP, b"record-b")),
        "STATE_CORRUPTED"
    );
    assert_eq!(error_code(store.load_mls_records(GROUP)), "STATE_CORRUPTED");
    // record-a 本身仍能解出。
    assert_eq!(
        store
            .load_mls_record(GROUP, b"record-a")
            .expect("record")
            .expect("present")
            .value,
        b"secret-a"
    );
}

#[test]
fn mls_state_material_is_not_stored_in_plaintext() {
    let fixture = Fixture::new("plaintext");
    let mut store = fixture.open();
    store
        .save_mls_group_state(snapshot(
            1,
            "GROUP-STATE-MARKER-PLAINTEXT",
            &[("provider-key-marker", "EPOCH-SECRET-MARKER-PLAINTEXT")],
        ))
        .expect("save");
    let files = fixture.store_files();
    assert!(!files.is_empty());
    // 群状态与 provider 记录值必须密封；记录键是明文的不透明标识（见迁移注释）。
    for (suffix, bytes) in files {
        for marker in [
            "GROUP-STATE-MARKER-PLAINTEXT".as_bytes(),
            "EPOCH-SECRET-MARKER-PLAINTEXT".as_bytes(),
        ] {
            assert!(
                !contains(&bytes, marker),
                "MLS secret leaked into {suffix:?}"
            );
        }
    }
    drop(store);
    let store = fixture.open();
    assert_eq!(
        store
            .load_mls_group_state(GROUP)
            .expect("load")
            .expect("present")
            .state,
        b"GROUP-STATE-MARKER-PLAINTEXT"
    );
}
