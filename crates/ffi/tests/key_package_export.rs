//! GROUPS-004 FFI 用例：导出本机 MLS KeyPackage（真库 + 真 OpenMLS）。
//!
//! 覆盖：非空且 >100 字节；两次导出不同（单次使用语义）；导出的包能被**另一个库**用来建群，
//! 并让导出方（同一设备实例）成功加入；纯内存对象 INVALID_STATE_TRANSITION；关闭后 CORE_CLOSED。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use chat_ffi::groups_ffi::GroupFfi;
use chat_ffi::identity_ffi::IdentityFfi;
use chat_ffi::local_store::FfiError;

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
            "chat-ffi-kp-export-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> String {
        self.0.join("chat.db").to_string_lossy().into_owned()
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
fn exported_key_packages_are_distinct_and_single_use() {
    let dir = FixtureDir::new();
    let identity = IdentityFfi::open_persisted(dir.database(), MASTER_KEY_HEX.to_owned(), NODE.to_owned())
        .expect("open persisted identity");
    let first = identity.export_key_package().expect("first export");
    let second = identity.export_key_package().expect("second export");
    assert!(first.len() > 100, "an MLS KeyPackage is far bigger than 100 bytes");
    assert!(second.len() > 100);
    assert_ne!(first, second, "every export is a fresh single-use key package");
    // 只含公钥材料：WELCOME 之外不该出现任何私钥字样（这里只做结构性的最小断言）。
    assert!(!first.is_empty());
}

#[test]
fn the_exported_key_package_builds_a_group_the_exporter_can_join() {
    let exporter_dir = FixtureDir::new();
    let exporter = IdentityFfi::open_persisted(
        exporter_dir.database(),
        MASTER_KEY_HEX.to_owned(),
        NODE.to_owned(),
    )
    .expect("open the exporter identity");
    let credential = exporter.account().device_id;
    let key_package = exporter.export_key_package().expect("export key package");

    // 另一个库（A 侧）用这份 KeyPackage 建群。
    let creator_dir = FixtureDir::new();
    let creator = GroupFfi::open(creator_dir.database(), MASTER_KEY_HEX.to_owned(), None)
        .expect("open the creator group ffi");
    let created = creator
        .group_create_with_invitee_key_package(
            "端到端邀请群".to_owned(),
            credential.clone(),
            key_package.clone(),
            "alice".to_owned(),
        )
        .expect("create the group with the exported key package");
    let view = created.view.clone();
    let welcome = created.welcome_bytes.clone();
    let invite = created.invite.clone();
    assert_eq!(view.members.len(), 2, "creator + invited device");
    assert!(!welcome.is_empty());
    assert_eq!(invite.invitee_credential, credential);
    creator.shutdown();

    // 导出方用**同一个设备实例**接受 Welcome（KeyPackage 的私钥材料就在那里）。
    let joined = exporter
        .join_group_from_welcome("端到端邀请群".to_owned(), welcome, invite)
        .expect("the exporter joins its own invite");
    assert_eq!(joined.group_id, view.group_id);
    assert_eq!(joined.epoch, view.epoch, "both sides agree on the epoch");
    let mut members = joined.members.clone();
    members.sort();
    assert_eq!(members, vec!["alice".to_owned(), credential.clone()]);
    exporter.shutdown();
}

#[test]
fn key_package_export_needs_the_persistent_identity_and_an_open_object() {
    // 纯内存对象：没有库 → INVALID_STATE_TRANSITION。
    let memory = IdentityFfi::create("内存身份".to_owned(), NODE.to_owned()).expect("create");
    assert_eq!(
        code(memory.export_key_package().expect_err("memory export")),
        "INVALID_STATE_TRANSITION"
    );

    // 持久化对象关闭后 → CORE_CLOSED。
    let dir = FixtureDir::new();
    let identity = IdentityFfi::open_persisted(dir.database(), MASTER_KEY_HEX.to_owned(), NODE.to_owned())
        .expect("open persisted identity");
    assert!(!identity.export_key_package().expect("export before close").is_empty());
    identity.shutdown();
    assert_eq!(
        code(identity.export_key_package().expect_err("closed export")),
        "CORE_CLOSED"
    );
    let _: Arc<IdentityFfi> = identity;
}
