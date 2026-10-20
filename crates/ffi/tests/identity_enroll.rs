//! FIX-R179：把本机身份登记进另一个真实加密库（真库，不联网）。
//!
//! 覆盖：enroll 到空库后 storage 读到同一 device_id 与**相同 seeds**；再 enroll 幂等（库版本不涨）；
//! 目标库已绑定另一账户 → IDENTITY_ACCOUNT_CONFLICT 且目标库不变；关闭后 CORE_CLOSED；
//! 纯内存身份 → INVALID_STATE_TRANSITION。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use chat_ffi::identity_ffi::IdentityFfi;
use chat_ffi::local_store::FfiError;
use chat_storage::formal::{
    IdentityAccountInput, KeyProvenance, ProtectedStorageKey, StoreOptions, open_store,
};

const MASTER_KEY_HEX: &str =
    "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";
const OTHER_KEY_HEX: &str =
    "3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c";
const NODE: &str = "node:1:0000000000000000000000000000000000000000000000000000000000000000";

struct FixtureDir(PathBuf);

impl FixtureDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "chat-ffi-enroll-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> String {
        self.0.join("chat.db").to_string_lossy().into_owned()
    }

    fn message_database(&self) -> String {
        self.0.join("messages.db").to_string_lossy().into_owned()
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn code(error: FfiError) -> String {
    match error {
        FfiError::Store { code, .. } => code,
    }
}

fn open(database: &str) -> chat_storage::formal::Store {
    open_store(
        StoreOptions::new(database, KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes([0x5a; 32]),
    )
    .expect("open store")
}

#[test]
fn enrolling_into_an_empty_store_makes_the_identity_usable_there() {
    let dir = FixtureDir::new();
    let identity = IdentityFfi::open_persisted(dir.database(), MASTER_KEY_HEX.to_owned(), NODE.to_owned())
        .expect("open source identity");
    let account = identity.account();
    let view = identity
        .enroll_into_store(dir.message_database(), MASTER_KEY_HEX.to_owned())
        .expect("enroll into the message store");
    assert_eq!(view.device_id, account.device_id);
    assert_eq!(view.account_id, account.account_id);
    assert!(!view.display_name.is_empty());
    assert_eq!(view.fingerprint.len(), 64, "hex of the 32-byte fingerprint");
    // 释放源库写锁后才能直接读源库做对比。
    identity.shutdown();

    let source_seeds = {
        let source = open(&dir.database());
        source.load_identity_seeds().expect("seeds").expect("enrolled")
    };
    // 目标库真的能用：seeds 可读且与源**逐字节相同**。
    let target = open(&dir.message_database());
    let seeds = target.load_identity_seeds().expect("seeds").expect("identity present");
    assert_eq!(seeds.device_id, source_seeds.device_id);
    assert_eq!(seeds.account_id, source_seeds.account_id);
    assert_eq!(seeds.account_seed.to_vec(), source_seeds.account_seed.to_vec());
    assert_eq!(seeds.device_seed.to_vec(), source_seeds.device_seed.to_vec());
    let projection = target.load_identity_account().expect("projection").expect("present");
    assert_eq!(projection.device_id, view.device_id);
}

#[test]
fn enrolling_twice_is_idempotent() {
    let dir = FixtureDir::new();
    let identity = IdentityFfi::open_persisted(dir.database(), MASTER_KEY_HEX.to_owned(), NODE.to_owned())
        .expect("open source identity");
    identity
        .enroll_into_store(dir.message_database(), MASTER_KEY_HEX.to_owned())
        .expect("first enroll");
    let version_after_first = {
        let target = open(&dir.message_database());
        target.schema_version()
    };
    identity
        .enroll_into_store(dir.message_database(), MASTER_KEY_HEX.to_owned())
        .expect("second enroll is idempotent");
    let target = open(&dir.message_database());
    assert_eq!(target.schema_version(), version_after_first, "no row/schema change");
    let rows = target.load_identity_account().expect("projection").expect("present");
    assert_eq!(rows.device_id, identity.account().device_id);
    drop(target);
    identity.shutdown();
}

#[test]
fn a_store_bound_to_another_account_is_never_overwritten() {
    let dir = FixtureDir::new();
    let identity = IdentityFfi::open_persisted(dir.database(), MASTER_KEY_HEX.to_owned(), NODE.to_owned())
        .expect("open source identity");
    // 目标库里先放进**另一个账户**的身份。
    {
        let mut target = open(&dir.message_database());
        target
            .save_identity_account(IdentityAccountInput {
                account_id: "acct:1:".to_owned() + &"ab".repeat(32),
                device_id: "dev:1:".to_owned() + &"cd".repeat(32),
                account_public_key: [0x11; 32],
                device_public_key: [0x22; 32],
                account_seed: vec![0x33; 32],
                device_seed: vec![0x44; 32],
                display_name: "另一个账户".to_owned(),
                fingerprint: [0x55; 32],
                created_at_ms: 1_700_000_000_000,
            })
            .expect("seed the other identity");
    }
    let before = {
        let target = open(&dir.message_database());
        target.load_identity_account().expect("projection").expect("present")
    };

    let error = identity
        .enroll_into_store(dir.message_database(), MASTER_KEY_HEX.to_owned())
        .expect_err("a bound store must not be overwritten");
    assert_eq!(code(error), "IDENTITY_ACCOUNT_CONFLICT");

    // 目标库逐字段不变。
    let target = open(&dir.message_database());
    let after = target.load_identity_account().expect("projection").expect("present");
    assert_eq!(after, before);
    drop(target);
    identity.shutdown();
}

#[test]
fn enrolling_requires_a_persisted_open_identity() {
    // 纯内存身份：没有源库可读种子 → INVALID_STATE_TRANSITION。
    let dir = FixtureDir::new();
    let memory = IdentityFfi::create("内存身份".to_owned(), NODE.to_owned()).expect("create");
    assert_eq!(
        code(memory
            .enroll_into_store(dir.message_database(), MASTER_KEY_HEX.to_owned())
            .expect_err("memory identity cannot enroll")),
        "INVALID_STATE_TRANSITION"
    );

    // 关闭后 → CORE_CLOSED。
    let identity = IdentityFfi::open_persisted(dir.database(), MASTER_KEY_HEX.to_owned(), NODE.to_owned())
        .expect("open source identity");
    identity.shutdown();
    assert_eq!(
        code(identity
            .enroll_into_store(dir.message_database(), MASTER_KEY_HEX.to_owned())
            .expect_err("closed object")),
        "CORE_CLOSED"
    );
    let _: Arc<IdentityFfi> = identity;
}

#[test]
fn a_wrong_target_master_key_is_reported_as_a_storage_error() {
    let dir = FixtureDir::new();
    let identity = IdentityFfi::open_persisted(dir.database(), MASTER_KEY_HEX.to_owned(), NODE.to_owned())
        .expect("open source identity");
    // 目标库用另一把主密钥创建，再用错误密钥打开 → 存储层错误码原样透出。
    {
        let mut other = open_store(
            StoreOptions::new(dir.message_database(), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes([0x3c; 32]),
        )
        .expect("open with the other key");
        other
            .save_identity_account(IdentityAccountInput {
                account_id: identity.account().account_id.clone(),
                device_id: identity.account().device_id.clone(),
                account_public_key: [0x11; 32],
                device_public_key: [0x22; 32],
                account_seed: vec![0x33; 32],
                device_seed: vec![0x44; 32],
                display_name: "同一账户".to_owned(),
                fingerprint: [0x55; 32],
                created_at_ms: 1_700_000_000_000,
            })
            .expect("seed with the other key");
    }
    let error = identity
        .enroll_into_store(dir.message_database(), MASTER_KEY_HEX.to_owned())
        .expect_err("wrong master key");
    let error_code = code(error);
    assert_ne!(error_code, "OK");
    assert!(!error_code.is_empty());
    assert_ne!(error_code, "IDENTITY_ACCOUNT_CONFLICT", "a key failure is not a conflict");
    identity.shutdown();
    let _ = OTHER_KEY_HEX;
}
