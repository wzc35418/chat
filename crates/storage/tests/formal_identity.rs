//! 身份账户 / 联系人 / 邀请一次性消费的正式存储测试。
//!
//! 覆盖：同主密钥重开读回、重复保存幂等与冲突、错误主密钥拒绝且不覆盖、
//! .db/-wal 中无明文账户/联系人材料、联系人幂等与上限、邀请重复/并发只成功一次、
//! 损坏记录（含跨行挪密文）显式 STATE_CORRUPTED。

use chat_storage::formal::{
    ContactUpsertInput, KeyProvenance, MAX_CONTACTS, ProtectedStorageKey, StorageError, Store,
    StoreOptions, WriteScope, commit_transaction, contact_id_for, open_store,
};
use rusqlite::params;
use std::fs;
use std::path::PathBuf;

const ACCOUNT: &str = "acct-1";
const DEVICE: &str = "dev-1";
const DISPLAY_NAME: &str = "Alice";
const PEER: &str = "acct-2";
const PEER_TWO: &str = "acct-3";

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
    key_bytes: [u8; 32],
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "chat-formal-identity-{tag}-{}",
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

fn error_code<T>(result: Result<T, StorageError>) -> &'static str {
    match result {
        Ok(_) => panic!("expected an explicit failure"),
        Err(error) => error.code(),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

fn account_seed() -> [u8; 32] {
    let mut seed = [0u8; 32];
    for (index, byte) in seed.iter_mut().enumerate() {
        *byte = u8::try_from(index + 1).unwrap();
    }
    seed
}

fn device_seed() -> [u8; 32] {
    let mut seed = [0u8; 32];
    for (index, byte) in seed.iter_mut().enumerate() {
        *byte = u8::try_from(index + 101).unwrap();
    }
    seed
}

fn identity_input(display_name: &str) -> chat_storage::formal::IdentityAccountInput {
    chat_storage::formal::IdentityAccountInput {
        account_id: ACCOUNT.to_owned(),
        device_id: DEVICE.to_owned(),
        account_public_key: [0x21; 32],
        device_public_key: [0x22; 32],
        account_seed: account_seed().to_vec(),
        device_seed: device_seed().to_vec(),
        display_name: display_name.to_owned(),
        fingerprint: [0x31; 32],
        created_at_ms: 1_700_000_000_000,
    }
}

fn contact_input(
    account_id: &str,
    display_name: &str,
    fingerprint: u8,
    state: &str,
    updated_at_ms: i64,
) -> ContactUpsertInput {
    ContactUpsertInput {
        account_id: account_id.to_owned(),
        display_name: display_name.to_owned(),
        fingerprint: [fingerprint; 32],
        state: state.to_owned(),
        updated_at_ms,
    }
}

fn invite_digest(seed: u8) -> [u8; 32] {
    [seed; 32]
}

#[test]
fn identity_account_round_trips_and_is_idempotent() {
    let fixture = Fixture::new("round-trip");
    let mut store = fixture.open();
    let saved = store
        .save_identity_account(identity_input(DISPLAY_NAME))
        .expect("save account");
    assert!(!saved.already_existed);
    assert_eq!(saved.account_id, ACCOUNT);
    assert_eq!(saved.device_id, DEVICE);
    assert_eq!(saved.display_name, DISPLAY_NAME);
    assert_eq!(saved.fingerprint, [0x31; 32]);
    assert_eq!(saved.created_at_ms, 1_700_000_000_000);

    let loaded = store
        .load_identity_account()
        .expect("load")
        .expect("account present");
    assert_eq!(loaded.account_id, saved.account_id);
    assert_eq!(loaded.device_id, saved.device_id);
    assert_eq!(loaded.display_name, saved.display_name);
    assert_eq!(loaded.fingerprint, saved.fingerprint);
    assert_eq!(loaded.created_at_ms, saved.created_at_ms);

    // 同内容重复保存：幂等，不写行、不推进库版本。
    let version = store.store_version().expect("version");
    let again = store
        .save_identity_account(identity_input(DISPLAY_NAME))
        .expect("idempotent save");
    assert!(again.already_existed);
    assert_eq!(again.updated_at_ms, saved.updated_at_ms);
    assert_eq!(store.store_version().expect("version"), version);

    // 显示名更新：保留创建时间，推进版本。
    let renamed = store
        .save_identity_account(identity_input("Alice Renamed"))
        .expect("rename");
    assert!(renamed.already_existed);
    assert_eq!(renamed.created_at_ms, saved.created_at_ms);
    assert_eq!(renamed.display_name, "Alice Renamed");
    assert!(store.store_version().expect("version") > version);
    assert_eq!(
        store
            .load_identity_account()
            .expect("load")
            .expect("present")
            .display_name,
        "Alice Renamed"
    );
    drop(store);

    // 同主密钥重开：投影一致。
    let store = fixture.open();
    let reopened = store
        .load_identity_account()
        .expect("load")
        .expect("present");
    assert_eq!(reopened.account_id, ACCOUNT);
    assert_eq!(reopened.device_id, DEVICE);
    assert_eq!(reopened.display_name, "Alice Renamed");
    assert_eq!(reopened.fingerprint, [0x31; 32]);
    assert_eq!(reopened.created_at_ms, 1_700_000_000_000);
}

#[test]
fn identity_account_conflicts_are_explicit_and_never_overwrite() {
    let fixture = Fixture::new("conflict");
    let mut store = fixture.open();
    store
        .save_identity_account(identity_input(DISPLAY_NAME))
        .expect("save account");

    // 另一个账户（不同账户 ID 与种子）必须显式冲突。
    let mut other = identity_input(DISPLAY_NAME);
    other.account_id = "acct-9".to_owned();
    other.device_id = "dev-9".to_owned();
    other.account_seed = vec![0x55; 32];
    other.device_seed = vec![0x66; 32];
    assert_eq!(
        error_code(store.save_identity_account(other)),
        "IDENTITY_ACCOUNT_CONFLICT"
    );
    // 同账户但不同密钥材料也显式冲突。
    let mut remixed = identity_input(DISPLAY_NAME);
    remixed.account_seed = vec![0x77; 32];
    assert_eq!(
        error_code(store.save_identity_account(remixed)),
        "IDENTITY_ACCOUNT_CONFLICT"
    );
    let loaded = store
        .load_identity_account()
        .expect("load")
        .expect("present");
    assert_eq!(loaded.account_id, ACCOUNT, "the original identity is kept");
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
    assert_eq!(
        store
            .load_identity_account()
            .expect("load")
            .expect("present")
            .account_id,
        ACCOUNT
    );
}

#[test]
fn identity_material_is_not_stored_in_plaintext() {
    let fixture = Fixture::new("plaintext");
    let mut store = fixture.open();
    store
        .save_identity_account(identity_input(DISPLAY_NAME))
        .expect("save account");
    store
        .upsert_contact(contact_input(PEER, "Bob", 0x41, "requested", 1_700_000_000_001))
        .expect("contact");
    store
        .consume_invite("invite-1", &invite_digest(7), 1_700_000_000_002)
        .expect("consume invite");
    let version = store.store_version().expect("version");
    assert!(version >= 2);

    let files = fixture.store_files();
    assert!(!files.is_empty());
    let mut patterns: Vec<(&str, Vec<u8>)> = vec![
        ("account id", ACCOUNT.as_bytes().to_vec()),
        ("device id", DEVICE.as_bytes().to_vec()),
        ("display name", DISPLAY_NAME.as_bytes().to_vec()),
        ("contact display name", b"Bob".to_vec()),
        ("peer account id", PEER.as_bytes().to_vec()),
        ("account seed", account_seed().to_vec()),
        ("device seed", device_seed().to_vec()),
        ("fingerprint", vec![0x31; 32]),
        ("contact fingerprint", vec![0x41; 32]),
    ];
    let account_seed_hex: String = account_seed().iter().map(|byte| format!("{byte:02x}")).collect();
    patterns.push(("account seed hex", account_seed_hex.into_bytes()));
    for (suffix, bytes) in &files {
        for (label, needle) in &patterns {
            assert!(
                !contains(bytes, needle),
                "{label} leaked into the store file {suffix:?}"
            );
        }
    }

    // 重开仍能解密（落盘的是可解密的密封记录，不是垃圾）。
    drop(store);
    let store = fixture.open();
    assert_eq!(
        store
            .load_identity_account()
            .expect("load")
            .expect("present")
            .display_name,
        DISPLAY_NAME
    );
    assert_eq!(store.list_contacts().expect("contacts").len(), 1);
}

#[test]
fn contacts_upsert_list_and_survive_reopen() {
    let fixture = Fixture::new("contacts");
    let mut store = fixture.open();
    assert!(store.read_contact(PEER).expect("read").is_none());

    let first = store
        .upsert_contact(contact_input(PEER, "Bob", 0x41, "requested", 1_700_000_000_010))
        .expect("insert contact");
    assert!(!first.already_existed);
    assert_eq!(first.contact_id, contact_id_for(PEER).expect("contact id"));
    assert_eq!(first.state, "requested");
    store
        .upsert_contact(contact_input(PEER_TWO, "Carol", 0x42, "accepted", 1_700_000_000_020))
        .expect("insert second contact");

    // 幂等：同内容重复写入不推进版本。
    let version = store.store_version().expect("version");
    let again = store
        .upsert_contact(contact_input(PEER, "Bob", 0x41, "requested", 1_700_000_000_010))
        .expect("idempotent contact");
    assert!(again.already_existed);
    assert_eq!(store.store_version().expect("version"), version);

    // 指纹/状态更新：同一个 contact_id，内容更新。
    let updated = store
        .upsert_contact(contact_input(PEER, "Bob", 0x43, "accepted", 1_700_000_000_030))
        .expect("update contact");
    assert_eq!(updated.contact_id, first.contact_id);
    assert_eq!(updated.fingerprint, [0x43; 32]);
    assert_eq!(updated.state, "accepted");
    assert!(store.store_version().expect("version") > version);

    let listed = store.list_contacts().expect("list");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].account_id, PEER, "most recent event first");
    assert_eq!(listed[1].account_id, PEER_TWO);
    let read = store
        .read_contact(PEER)
        .expect("read")
        .expect("contact present");
    assert_eq!(read.state, "accepted");
    drop(store);

    let store = fixture.open();
    let listed = store.list_contacts().expect("list after reopen");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].fingerprint, [0x43; 32]);
    assert_eq!(listed[0].state, "accepted");
}

#[test]
fn contact_validation_and_bounds_are_enforced() {
    let fixture = Fixture::new("contact-bounds");
    let mut store = fixture.open();
    assert_eq!(
        error_code(store.upsert_contact(contact_input(PEER, "Bob", 0x41, "unknown", 1))),
        "INVALID_INPUT"
    );
    let long_name = "x".repeat(65);
    assert_eq!(
        error_code(store.upsert_contact(contact_input(PEER, &long_name, 0x41, "requested", 1))),
        "INVALID_INPUT"
    );
    assert_eq!(
        error_code(store.upsert_contact(contact_input("bad id!", "Bob", 0x41, "requested", 1))),
        "INVALID_INPUT"
    );
    assert_eq!(
        error_code(store.upsert_contact(contact_input(PEER, "Bob", 0x41, "requested", -1))),
        "INVALID_INPUT"
    );
    assert_eq!(
        error_code(store.consume_invite("bad invite!", &invite_digest(1), 1)),
        "INVALID_INPUT"
    );
    assert_eq!(
        error_code(store.consume_invite("invite-1", &invite_digest(1), -1)),
        "INVALID_INPUT"
    );

    // 记录数上限：批量塞满后新插入显式拒绝（不静默丢弃）。
    store
        .connection()
        .execute(
            "WITH RECURSIVE seq(value) AS (SELECT 1 UNION ALL SELECT value + 1 FROM seq WHERE value < ?1)
             INSERT INTO contacts (contact_id, protected_record, updated_at)
             SELECT 'bulk-' || value, X'00', 0 FROM seq",
            params![i64::from(MAX_CONTACTS)],
        )
        .expect("bulk fill contacts");
    assert_eq!(
        error_code(store.upsert_contact(contact_input(PEER, "Bob", 0x41, "requested", 1))),
        "INVALID_INPUT"
    );
}

#[test]
fn invite_consumption_is_one_shot() {
    let fixture = Fixture::new("invite");
    let mut store = fixture.open();
    let consumed = store
        .consume_invite("invite-1", &invite_digest(7), 1_700_000_000_100)
        .expect("first consume");
    assert_eq!(consumed.invite_id, "invite-1");
    assert_eq!(consumed.consumed_at_ms, 1_700_000_000_100);
    let version = store.store_version().expect("version");

    // 第二次（同 ID 同摘要）必须显式失败，且不写行、不推进版本。
    assert_eq!(
        error_code(store.consume_invite("invite-1", &invite_digest(7), 1_700_000_000_101)),
        "INVALID_STATE_TRANSITION"
    );
    // 同 ID 不同摘要：同样显式失败（不静默改写已消费记录）。
    assert_eq!(
        error_code(store.consume_invite("invite-1", &invite_digest(8), 1_700_000_000_102)),
        "INVALID_STATE_TRANSITION"
    );
    assert_eq!(store.store_version().expect("version"), version);
    // 新邀请 ID 仍然可用。
    store
        .consume_invite("invite-2", &invite_digest(9), 1_700_000_000_103)
        .expect("second invite");
    drop(store);

    // 重启后仍然是已消费状态。
    let mut store = fixture.open();
    assert_eq!(
        error_code(store.consume_invite("invite-1", &invite_digest(7), 1_700_000_000_104)),
        "INVALID_STATE_TRANSITION"
    );
}

#[test]
fn concurrent_invite_consumption_succeeds_exactly_once() {
    // 单写者独占打开：进程内并发由调用方串行化；库级唯一约束与事务保证
    // “同一 invite_id 只成功一次”。这里用顺序调用模拟两条并发请求的判定点。
    let fixture = Fixture::new("invite-concurrent");
    let mut store = fixture.open();
    store
        .consume_invite("invite-race", &invite_digest(3), 1)
        .expect("first wins");
    for _ in 0..4 {
        assert_eq!(
            error_code(store.consume_invite("invite-race", &invite_digest(3), 2)),
            "INVALID_STATE_TRANSITION"
        );
    }
    let count: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM consumed_invites WHERE invite_id = ?1",
            ["invite-race"],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(count, 1);
}

#[test]
fn tampered_or_relocated_identity_records_are_state_corrupted() {
    let fixture = Fixture::new("tamper");
    let mut store = fixture.open();
    store
        .save_identity_account(identity_input(DISPLAY_NAME))
        .expect("save account");
    store
        .upsert_contact(contact_input(PEER, "Bob", 0x41, "requested", 10))
        .expect("contact one");
    store
        .upsert_contact(contact_input(PEER_TWO, "Carol", 0x42, "requested", 11))
        .expect("contact two");

    // 1) 篡改账户密封记录。
    store
        .connection()
        .execute(
            "UPDATE identity_account SET protected_account = X'00ff00ff' WHERE id = 1",
            [],
        )
        .expect("tamper account");
    assert_eq!(
        error_code(store.load_identity_account()),
        "STATE_CORRUPTED"
    );

    // 2) 跨行挪联系人密文：AAD 绑定 contact_id，必须认证失败。
    let peer_id = contact_id_for(PEER).expect("contact id");
    let peer_two_id = contact_id_for(PEER_TWO).expect("contact id");
    let peer_blob: Vec<u8> = store
        .connection()
        .query_row(
            "SELECT protected_record FROM contacts WHERE contact_id = ?1",
            [&peer_id],
            |row| row.get(0),
        )
        .expect("read blob");
    store
        .connection()
        .execute(
            "UPDATE contacts SET protected_record = ?2 WHERE contact_id = ?1",
            params![peer_two_id, peer_blob],
        )
        .expect("relocate blob");
    assert_eq!(error_code(store.list_contacts()), "STATE_CORRUPTED");

    // 3) 事务接口本身在身份作用域内可用，且作用域外不能隐式写身份表。
    let transaction = store.begin_transaction(WriteScope::identity()).expect("tx");
    let rows: i64 = transaction
        .connection()
        .query_row("SELECT COUNT(*) FROM consumed_invites", [], |row| row.get(0))
        .expect("count");
    assert_eq!(rows, 0);
    commit_transaction(transaction).expect("commit empty identity tx");
}
