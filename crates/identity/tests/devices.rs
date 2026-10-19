//! IDENTITY-011/012：设备登记与撤销集合（真临时加密库）。
//!
//! 覆盖：登记/列出/幂等；撤销后 is_revoked=true 且默认列表不含（include_revoked 可见）；
//! 撤销本机被拒（DEVICE_SELF_REVOKE）；重启后仍在；多账户隔离；重复登记不复活已撤销设备。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_identity::devices::{
    device_is_revoked, load_device, register_device, registered_devices, revoke_registered_device,
};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store};

const MASTER_KEY: [u8; 32] = [0x5a; 32];
const ACCOUNT_A: &str = "acct-dev-a";
const ACCOUNT_B: &str = "acct-dev-b";
const NOW_MS: i64 = 1_700_000_000_000;

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
            "chat-identity-devices-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> PathBuf {
        self.0.join("chat.db")
    }

    fn open(&self) -> Store {
        open_store(
            StoreOptions::new(self.database(), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(MASTER_KEY),
        )
        .expect("open formal store")
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn key(seed: u8) -> [u8; 32] {
    [seed; 32]
}

#[test]
fn registering_a_device_is_idempotent_and_listable() {
    let dir = FixtureDir::new();
    let mut store = dir.open();
    let first = register_device(&mut store, "dev:1:aa", ACCOUNT_A, key(1), "笔记本").expect("register");
    assert_eq!(first.device_id, "dev:1:aa");
    assert_eq!(first.account_id, ACCOUNT_A);
    assert_eq!(first.label, "笔记本");
    assert!(!first.is_revoked());
    assert_eq!(first.device_public_key.len(), 64, "hex of 32 bytes");

    // 幂等：重复登记同一设备不新增行。
    let again = register_device(&mut store, "dev:1:aa", ACCOUNT_A, key(2), "改名").expect("re-register");
    assert_eq!(again.label, "改名");
    assert_eq!(again.device_public_key, "02".repeat(32), "the key is refreshed");
    assert_eq!(again.added_at_ms, first.added_at_ms, "the first registration time is kept");
    let listed = registered_devices(&store, ACCOUNT_A, false).expect("list");
    assert_eq!(listed.len(), 1);

    // 另一台设备。
    register_device(&mut store, "dev:1:bb", ACCOUNT_A, key(3), "").expect("second device");
    assert_eq!(registered_devices(&store, ACCOUNT_A, false).expect("list").len(), 2);
}

#[test]
fn revoking_a_device_hides_it_from_the_default_list_but_keeps_the_row() {
    let dir = FixtureDir::new();
    let mut store = dir.open();
    register_device(&mut store, "dev:1:aa", ACCOUNT_A, key(1), "").expect("a");
    register_device(&mut store, "dev:1:bb", ACCOUNT_A, key(2), "").expect("b");

    let revoked = revoke_registered_device(
        &mut store,
        ACCOUNT_A,
        "dev:1:aa",
        "dev:1:bb",
        "operator_request",
        NOW_MS,
    )
    .expect("revoke");
    assert!(revoked, "the first revocation writes the row");
    assert!(device_is_revoked(&store, "dev:1:aa").expect("lookup"));
    assert!(!device_is_revoked(&store, "dev:1:bb").expect("lookup"));

    // 默认列表不含已撤销；include_revoked 时可见且带原因。
    let visible = registered_devices(&store, ACCOUNT_A, false).expect("list");
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].device_id, "dev:1:bb");
    let all = registered_devices(&store, ACCOUNT_A, true).expect("list all");
    assert_eq!(all.len(), 2);
    let revoked_row = all
        .iter()
        .find(|device| device.device_id == "dev:1:aa")
        .expect("revoked row is kept");
    assert_eq!(revoked_row.revoked_at_ms, Some(NOW_MS));
    assert_eq!(revoked_row.revoked_reason.as_deref(), Some("operator_request"));

    // 幂等：重复撤销不再改写（返回 false）。
    let again = revoke_registered_device(
        &mut store,
        ACCOUNT_A,
        "dev:1:aa",
        "dev:1:bb",
        "another_reason",
        NOW_MS + 10,
    )
    .expect("idempotent revoke");
    assert!(!again);
    let row = load_device(&store, "dev:1:aa").expect("load").expect("row");
    assert_eq!(row.revoked_at_ms, Some(NOW_MS), "the first revocation time wins");

    // 重新登记**不会复活**已撤销设备。
    register_device(&mut store, "dev:1:aa", ACCOUNT_A, key(9), "reinstalled").expect("re-register");
    assert!(device_is_revoked(&store, "dev:1:aa").expect("lookup"));
    assert_eq!(registered_devices(&store, ACCOUNT_A, false).expect("list").len(), 1);
}

#[test]
fn revoking_the_local_device_is_rejected() {
    let dir = FixtureDir::new();
    let mut store = dir.open();
    register_device(&mut store, "dev:1:local", ACCOUNT_A, key(1), "").expect("register");
    let error = revoke_registered_device(
        &mut store,
        ACCOUNT_A,
        "dev:1:local",
        "dev:1:local",
        "operator_request",
        NOW_MS,
    )
    .expect_err("self revocation is rejected");
    assert_eq!(error.code(), "DEVICE_SELF_REVOKE");
    assert!(!device_is_revoked(&store, "dev:1:local").expect("unchanged"));
}

#[test]
fn the_registry_survives_a_restart() {
    let dir = FixtureDir::new();
    {
        let mut store = dir.open();
        register_device(&mut store, "dev:1:aa", ACCOUNT_A, key(1), "笔记本").expect("a");
        register_device(&mut store, "dev:1:bb", ACCOUNT_A, key(2), "").expect("b");
        revoke_registered_device(&mut store, ACCOUNT_A, "dev:1:aa", "dev:1:bb", "lost", NOW_MS)
            .expect("revoke");
    }
    // 重开：已撤销状态与历史行都在。
    let store = dir.open();
    assert!(device_is_revoked(&store, "dev:1:aa").expect("lookup"));
    let all = registered_devices(&store, ACCOUNT_A, true).expect("list");
    assert_eq!(all.len(), 2);
    let row = all
        .iter()
        .find(|device| device.device_id == "dev:1:aa")
        .expect("row");
    assert_eq!(row.label, "笔记本");
    assert_eq!(row.revoked_reason.as_deref(), Some("lost"));
    assert_eq!(registered_devices(&store, ACCOUNT_A, false).expect("list").len(), 1);
}

#[test]
fn accounts_are_isolated_and_cross_account_revocation_is_rejected() {
    let dir = FixtureDir::new();
    let mut store = dir.open();
    register_device(&mut store, "dev:1:aa", ACCOUNT_A, key(1), "").expect("a");
    register_device(&mut store, "dev:1:bb", ACCOUNT_B, key(2), "").expect("b");

    assert_eq!(registered_devices(&store, ACCOUNT_A, false).expect("a").len(), 1);
    assert_eq!(registered_devices(&store, ACCOUNT_B, false).expect("b").len(), 1);
    assert_eq!(
        registered_devices(&store, ACCOUNT_A, false).expect("a")[0].device_id,
        "dev:1:aa"
    );

    // 用 A 的身份撤销 B 的设备：显式拒绝，且 B 的设备不受影响。
    let error = revoke_registered_device(
        &mut store,
        ACCOUNT_A,
        "dev:1:bb",
        "dev:1:aa",
        "operator_request",
        NOW_MS,
    )
    .expect_err("cross-account revocation is rejected");
    assert_eq!(error.code(), "INVALID_INPUT");
    assert!(!device_is_revoked(&store, "dev:1:bb").expect("lookup"));
}
