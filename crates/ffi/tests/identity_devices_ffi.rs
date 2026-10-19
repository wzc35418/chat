//! IDENTITY-011/012 FFI 用例：设备集合的列出/撤销（真临时加密库）。
//!
//! 覆盖：建身份 → list 含本机 is_local=true；撤销本机被拒（DEVICE_SELF_REVOKE）；
//! 登记第二台 → 两条 → 撤销 → 默认一条且 include_revoked 可见原因；关闭后 CORE_CLOSED。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use chat_ffi::identity_ffi::IdentityFfi;
use chat_ffi::local_store::FfiError;
use chat_storage::formal::{
    DeviceRegistrationInput, KeyProvenance, ProtectedStorageKey, StoreOptions, open_store,
};

const MASTER_KEY_HEX: &str =
    "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";
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
            "chat-ffi-devices-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> String {
        self.0.join("chat.db").to_string_lossy().into_owned()
    }

    fn open(&self) -> Arc<IdentityFfi> {
        IdentityFfi::open_persisted(self.database(), MASTER_KEY_HEX.to_owned(), NODE.to_owned())
            .expect("open persisted identity")
    }

    fn store(&self) -> chat_storage::formal::Store {
        open_store(
            StoreOptions::new(self.database(), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes([0x5a; 32]),
        )
        .expect("open store")
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

#[test]
fn the_local_device_is_listed_and_cannot_revoke_itself() {
    let dir = FixtureDir::new();
    let identity = dir.open();
    let devices = identity.list_devices(false).expect("list devices");
    assert_eq!(devices.len(), 1, "the local device is registered lazily");
    let local = &devices[0];
    assert!(local.is_local);
    assert_eq!(local.device_id, identity.account().device_id);
    assert_eq!(local.account_id, identity.account().account_id);
    assert_eq!(local.device_public_key_hex.len(), 64);
    assert!(local.revoked_at_ms.is_none());

    // 撤销本机 → DEVICE_SELF_REVOKE，且状态不变。
    let local_id = local.device_id.clone();
    let error = identity
        .revoke_device(local_id.clone(), "operator_request".to_owned())
        .expect_err("self revocation is rejected");
    assert_eq!(code(error), "DEVICE_SELF_REVOKE");
    assert!(!identity.device_is_revoked(local_id.clone()).expect("lookup"));
    assert_eq!(identity.list_devices(true).expect("list").len(), 1);
}

#[test]
fn a_second_device_can_be_revoked_and_is_hidden_by_default() {
    let dir = FixtureDir::new();
    // 先建身份并让本机设备登记进集合，同时记下账户 ID（库是单写者，必须先释放句柄）。
    let account_id = {
        let identity = dir.open();
        assert_eq!(identity.list_devices(false).expect("list").len(), 1);
        let account_id = identity.account().account_id;
        identity.shutdown();
        account_id
    };
    // 通过 storage 直接登记第二台设备（这是"另一台机器"的登记行）。
    let second_id = "dev:1:2222222222222222222222222222222222222222222222222222222222222222";
    {
        let mut store = dir.store();
        store
            .upsert_identity_device(&DeviceRegistrationInput {
                device_id: second_id.to_owned(),
                account_id,
                device_public_key: [0x22; 32],
                label: "旧手机".to_owned(),
            })
            .expect("register the second device");
    }

    let identity = dir.open();
    let listed = identity.list_devices(false).expect("list");
    assert_eq!(listed.len(), 2);
    let second = listed
        .iter()
        .find(|device| device.device_id == second_id)
        .expect("second device listed");
    assert!(!second.is_local);
    assert_eq!(second.label, "旧手机");

    let revoked = identity
        .revoke_device(second_id.to_owned(), "lost_device".to_owned())
        .expect("revoke the second device");
    assert!(revoked, "the first revocation writes the row");
    assert!(identity.device_is_revoked(second_id.to_owned()).expect("lookup"));
    // 默认列表不含已撤销；include_revoked 可见原因。
    assert_eq!(identity.list_devices(false).expect("list").len(), 1);
    let all = identity.list_devices(true).expect("list all");
    assert_eq!(all.len(), 2);
    let revoked_row = all
        .iter()
        .find(|device| device.device_id == second_id)
        .expect("revoked row is kept");
    assert_eq!(revoked_row.revoked_reason.as_deref(), Some("lost_device"));
    assert!(revoked_row.revoked_at_ms.is_some());
    // 幂等：重复撤销不再改写。
    assert!(
        !identity
            .revoke_device(second_id.to_owned(), "again".to_owned())
            .expect("idempotent revoke")
    );
}

#[test]
fn revoking_an_unregistered_device_is_an_invalid_input() {
    let dir = FixtureDir::new();
    let identity = dir.open();
    let unknown = "dev:1:3333333333333333333333333333333333333333333333333333333333333333";
    let error = identity
        .revoke_device(unknown.to_owned(), "operator_request".to_owned())
        .expect_err("unknown device");
    assert_eq!(code(error), "INVALID_INPUT");
    assert!(!identity.device_is_revoked(unknown.to_owned()).expect("lookup"));
}

#[test]
fn a_closed_identity_object_rejects_device_calls() {
    let dir = FixtureDir::new();
    let identity = dir.open();
    assert_eq!(identity.list_devices(false).expect("list").len(), 1);
    identity.shutdown();
    assert_eq!(
        code(identity.list_devices(false).expect_err("closed list")),
        "CORE_CLOSED"
    );
    assert_eq!(
        code(
            identity
                .revoke_device("dev:1:4444".to_owned(), "x".to_owned())
                .expect_err("closed revoke")
        ),
        "CORE_CLOSED"
    );
    assert_eq!(
        code(
            identity
                .device_is_revoked("dev:1:4444".to_owned())
                .expect_err("closed lookup")
        ),
        "CORE_CLOSED"
    );
}

#[test]
fn device_calls_require_the_persistent_mode() {
    let identity = IdentityFfi::create("内存身份".to_owned(), NODE.to_owned()).expect("create");
    assert_eq!(
        code(identity.list_devices(false).expect_err("memory mode")),
        "INVALID_STATE_TRANSITION"
    );
}
