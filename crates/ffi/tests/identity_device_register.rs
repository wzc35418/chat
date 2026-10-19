//! IDENTITY-011 FFI 用例：register_device（真临时加密库）。
//!
//! 覆盖：登记第二台 → 两条 → 撤销 → 再登记仍撤销；重复登记 added_at_ms 不变；
//! 公钥非 64 hex / device_id 非法 → INVALID_INPUT；关闭后 CORE_CLOSED；is_local 判定。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use chat_ffi::identity_ffi::IdentityFfi;
use chat_ffi::local_store::FfiError;

const MASTER_KEY_HEX: &str =
    "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";
const NODE: &str = "node:1:0000000000000000000000000000000000000000000000000000000000000000";
const SECOND_DEVICE: &str = "dev:1:2222222222222222222222222222222222222222222222222222222222222222";
const SECOND_KEY: &str = "2222222222222222222222222222222222222222222222222222222222222222";

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
            "chat-ffi-device-register-{}-{nanos}-{counter}",
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
fn a_registered_device_can_be_listed_and_revoked() {
    let dir = FixtureDir::new();
    let identity = dir.open();
    let registered = identity
        .register_device(
            SECOND_DEVICE.to_owned(),
            SECOND_KEY.to_owned(),
            "旧手机".to_owned(),
        )
        .expect("register the second device");
    assert_eq!(registered.device_id, SECOND_DEVICE);
    assert_eq!(registered.device_public_key_hex, SECOND_KEY);
    assert_eq!(registered.label, "旧手机");
    assert!(!registered.is_local, "the second device is not the local one");
    assert!(registered.revoked_at_ms.is_none());

    let listed = identity.list_devices(false).expect("list");
    assert_eq!(listed.len(), 2, "the local device plus the registered one");

    // 撤销后默认列表只剩本机。
    assert!(
        identity
            .revoke_device(SECOND_DEVICE.to_owned(), "lost_device".to_owned())
            .expect("revoke")
    );
    assert_eq!(identity.list_devices(false).expect("list").len(), 1);
    assert!(identity.device_is_revoked(SECOND_DEVICE.to_owned()).expect("lookup"));

    // 再登记同一台：**不复活**，返回视图仍带撤销信息。
    let again = identity
        .register_device(
            SECOND_DEVICE.to_owned(),
            SECOND_KEY.to_owned(),
            "再登记".to_owned(),
        )
        .expect("re-register a revoked device");
    assert!(again.revoked_at_ms.is_some(), "revocation is not undone");
    assert_eq!(again.revoked_reason.as_deref(), Some("lost_device"));
    assert_eq!(again.label, "再登记", "the label is refreshed");
    assert!(identity.device_is_revoked(SECOND_DEVICE.to_owned()).expect("still revoked"));
    assert_eq!(identity.list_devices(false).expect("list").len(), 1);
}

#[test]
fn repeated_registration_keeps_the_first_added_at() {
    let dir = FixtureDir::new();
    let identity = dir.open();
    let first = identity
        .register_device(SECOND_DEVICE.to_owned(), SECOND_KEY.to_owned(), "手机".to_owned())
        .expect("first register");
    let second = identity
        .register_device(SECOND_DEVICE.to_owned(), SECOND_KEY.to_owned(), "改名".to_owned())
        .expect("second register");
    assert_eq!(second.added_at_ms, first.added_at_ms, "the first registration time is kept");
    assert_eq!(second.label, "改名");
    assert_eq!(identity.list_devices(true).expect("list").len(), 2, "no duplicate row");
}

#[test]
fn the_local_device_registers_as_is_local() {
    let dir = FixtureDir::new();
    let identity = dir.open();
    let account = identity.account();
    let local = identity
        .register_device(
            account.device_id.clone(),
            "11".repeat(32),
            "本机".to_owned(),
        )
        .expect("register the local device explicitly");
    assert!(local.is_local);
    assert_eq!(local.device_id, account.device_id);
}

#[test]
fn invalid_ids_and_keys_are_rejected() {
    let dir = FixtureDir::new();
    let identity = dir.open();
    // 公钥长度不对（63 / 65 hex、非 hex）。
    for bad_key in [
        "22".repeat(31),
        "22".repeat(33),
        "zz".repeat(32),
    ] {
        let error = identity
            .register_device(SECOND_DEVICE.to_owned(), bad_key, String::new())
            .expect_err("bad key");
        assert_eq!(code(error), "INVALID_INPUT");
    }
    // device_id 不是 PROTOCOL-001 设备文本。
    for bad_id in ["", "dev:1:nothex", "acct:1:2222222222222222222222222222222222222222222222222222222222222222"] {
        let error = identity
            .register_device(bad_id.to_owned(), SECOND_KEY.to_owned(), String::new())
            .expect_err("bad device id");
        assert_eq!(code(error), "INVALID_INPUT");
    }
    // 拒绝之后集合里只有本机一台（没有任何半写入）。
    assert_eq!(identity.list_devices(true).expect("list").len(), 1);
}

#[test]
fn a_closed_identity_object_rejects_registration() {
    let dir = FixtureDir::new();
    let identity = dir.open();
    identity.shutdown();
    let error = identity
        .register_device(SECOND_DEVICE.to_owned(), SECOND_KEY.to_owned(), String::new())
        .expect_err("closed");
    assert_eq!(code(error), "CORE_CLOSED");
}

#[test]
fn registration_requires_the_persistent_mode() {
    let identity = IdentityFfi::create("内存身份".to_owned(), NODE.to_owned()).expect("create");
    let error = identity
        .register_device(SECOND_DEVICE.to_owned(), SECOND_KEY.to_owned(), String::new())
        .expect_err("memory mode");
    assert_eq!(code(error), "INVALID_STATE_TRANSITION");
}
