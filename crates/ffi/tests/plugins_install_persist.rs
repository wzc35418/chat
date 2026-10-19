//! PLUGINS：插件包安装落库事务的 FFI 用例（真临时加密库）。
//!
//! 覆盖：安装→重开可见（版本 + 清单）；升级返回 upgraded_from 且重开一致；
//! 坏包/未信任 → 库与宿主都零变化；同 id 连续两次安装 → 一成一冲突且库只剩一行。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use chat_ffi::local_store::FfiError;
use chat_ffi::plugins_ffi::PluginsFfi;
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, StoreOptions, open_store};

const MASTER_KEY: [u8; 32] = [0x5a; 32];
const ENTRY_CLASS: &str = "com.example.Plugin";
const SPI_FILE: &str = "META-INF/services/com.example.Plugin";
const CLASS_FILE: &str = "com/example/Plugin.class";

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
            "chat-ffi-plugins-install-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> String {
        self.0.join("chat.db").to_string_lossy().into_owned()
    }

    fn open(&self) -> Arc<PluginsFfi> {
        PluginsFfi::open(self.database(), master_key_hex()).expect("open persistent plugins")
    }

    fn plugin_rows(&self) -> usize {
        let store = open_store(
            StoreOptions::new(&self.database(), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(MASTER_KEY),
        )
        .expect("open store");
        store.load_plugins().expect("load plugin rows").len()
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn master_key_hex() -> String {
    MASTER_KEY.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn manifest_json(version: &str) -> String {
    format!(
        "{{\"schemaVersion\":1,\"id\":\"notes.v1\",\"version\":\"{version}\",\"sdkVersion\":1,\
         \"platform\":\"desktop\",\"capabilities\":[\"Command\"],\"permissions\":[],\
         \"extensionPoints\":[\"Command\"],\"entryClass\":\"{ENTRY_CLASS}\"}}"
    )
}

/// 最小 ZIP（stored 条目、无 extra/comment；CRC 写 0）。
fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let local_offset = out.len() as u32;
        out.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
        out.extend_from_slice(&20_u16.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);

        central.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
        central.extend_from_slice(&20_u16.to_le_bytes());
        central.extend_from_slice(&20_u16.to_le_bytes());
        central.extend_from_slice(&0_u16.to_le_bytes());
        central.extend_from_slice(&0_u16.to_le_bytes());
        central.extend_from_slice(&0_u16.to_le_bytes());
        central.extend_from_slice(&0_u16.to_le_bytes());
        central.extend_from_slice(&0_u32.to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0_u16.to_le_bytes());
        central.extend_from_slice(&0_u16.to_le_bytes());
        central.extend_from_slice(&0_u16.to_le_bytes());
        central.extend_from_slice(&0_u16.to_le_bytes());
        central.extend_from_slice(&0_u32.to_le_bytes());
        central.extend_from_slice(&local_offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let central_offset = out.len() as u32;
    let central_size = central.len() as u32;
    out.extend_from_slice(&central);
    let count = entries.len() as u16;
    out.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out
}

fn package(version: &str) -> Vec<u8> {
    let manifest = manifest_json(version);
    zip(&[
        ("manifest.json", manifest.as_bytes()),
        (CLASS_FILE, b"\xca\xfe\xba\xbe"),
        (SPI_FILE, ENTRY_CLASS.as_bytes()),
    ])
}

fn code(error: FfiError) -> String {
    match error {
        FfiError::Store { code, .. } => code,
    }
}
#[test]
fn installing_a_package_persists_it_and_survives_a_reopen() {
    let dir = FixtureDir::new();
    let host = dir.open();
    let outcome = host
        .install_package(package("1.0.0"), true)
        .expect("install package");
    assert_eq!(outcome.plugin_id, "notes.v1");
    assert_eq!(outcome.version, "1.0.0");
    assert_eq!(outcome.upgraded_from, None);

    // 内存宿主立刻可见（版本 + 清单投影）。
    let listed = host.list_plugins().expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].version, "1.0.0");
    assert_eq!(listed[0].manifest.entry_class, ENTRY_CLASS);
    assert_eq!(listed[0].manifest.capabilities, vec!["Command"]);
    host.shutdown();

    // 重开：列表里仍是刚装上的版本与清单。
    let reopened = dir.open();
    let listed = reopened.list_plugins().expect("list after reopen");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].plugin_id, "notes.v1");
    assert_eq!(listed[0].version, "1.0.0");
    assert_eq!(listed[0].manifest.plugin_id, "notes.v1");
    assert_eq!(listed[0].manifest.entry_class, ENTRY_CLASS);
    reopened.shutdown();
    assert_eq!(dir.plugin_rows(), 1, "exactly one plugins row");
}

#[test]
fn upgrading_a_package_reports_upgraded_from_and_persists() {
    let dir = FixtureDir::new();
    let host = dir.open();
    host.install_package(package("1.0.0"), true).expect("install v1");
    let upgraded = host
        .install_package(package("1.5.0"), true)
        .expect("upgrade");
    assert_eq!(upgraded.version, "1.5.0");
    assert_eq!(upgraded.upgraded_from.as_deref(), Some("1.0.0"));
    assert_eq!(host.list_plugins().expect("list")[0].version, "1.5.0");
    host.shutdown();

    // 重开一致，且库里仍然只有一行（升级是 upsert）。
    let reopened = dir.open();
    assert_eq!(reopened.list_plugins().expect("list")[0].version, "1.5.0");
    reopened.shutdown();
    assert_eq!(dir.plugin_rows(), 1);
}

#[test]
fn broken_or_untrusted_packages_change_nothing() {
    let dir = FixtureDir::new();
    let host = dir.open();
    host.install_package(package("1.0.0"), true).expect("install v1");

    // 未信任：显式 PLUGIN_TRUST_REQUIRED，且版本不变、库不变。
    let untrusted = host
        .install_package(package("2.0.0"), false)
        .expect_err("untrusted package");
    assert_eq!(code(untrusted), "PLUGIN_TRUST_REQUIRED");
    assert_eq!(host.list_plugins().expect("list")[0].version, "1.0.0");

    // 坏包：不进事务，宿主与库都不变。
    let broken = host
        .install_package(b"not a zip".to_vec(), true)
        .expect_err("broken package");
    assert_eq!(code(broken), "PLUGIN_PACKAGE_CORRUPTED");
    assert_eq!(host.list_plugins().expect("list")[0].version, "1.0.0");

    // 降级：PLUGIN_VERSION_DOWNGRADE，版本仍是最新的那个。
    let downgrade = host
        .install_package(package("0.9.0"), true)
        .expect_err("downgrade");
    assert_eq!(code(downgrade), "PLUGIN_VERSION_DOWNGRADE");
    assert_eq!(host.list_plugins().expect("list")[0].version, "1.0.0");
    host.shutdown();
    assert_eq!(dir.plugin_rows(), 1);
}

#[test]
fn installing_the_same_version_twice_conflicts_and_keeps_a_single_row() {
    let dir = FixtureDir::new();
    let host = dir.open();
    host.install_package(package("1.0.0"), true).expect("first install");
    let conflict = host
        .install_package(package("1.0.0"), true)
        .expect_err("same version conflicts");
    assert_eq!(code(conflict), "PLUGIN_STATE_CONFLICT");
    assert_eq!(host.list_plugins().expect("list").len(), 1);
    host.shutdown();
    assert_eq!(dir.plugin_rows(), 1, "the plugins table keeps exactly one row");

    // 重开仍然只有一条、版本不变。
    let reopened = dir.open();
    let listed = reopened.list_plugins().expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].version, "1.0.0");
}

#[test]
fn install_package_requires_the_persistent_mode() {
    let host = PluginsFfi::new();
    let error = host
        .install_package(package("1.0.0"), true)
        .expect_err("memory mode refuses");
    assert_eq!(code(error), "INVALID_STATE_TRANSITION");
    // 校验清单仍然可用（不落库）。
    assert!(host.list_plugins().expect("list").is_empty());
}

