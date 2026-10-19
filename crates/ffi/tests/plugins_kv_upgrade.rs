//! PLUGINS：插件私有 KV 升级迁移与残留清理（真临时加密库）。
//!
//! 覆盖：升级默认 Keep（旧键仍可读）；Namespace 策略把旧键复制到 v<from>/ 前缀（原键仍在）；
//! uninstalled 行清理前后对比；阈值边界（严格小于）；单次上限截断。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use chat_ffi::plugins_ffi::PluginsFfi;
use chat_storage::formal::{
    KeyProvenance, KvMigrationPolicy, MAX_PLUGIN_PURGE, ProtectedStorageKey, STATE_UNINSTALLED,
    Store, StoreOptions, open_store,
};

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
            "chat-ffi-plugins-kv-{}-{nanos}-{counter}",
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

    fn store(&self) -> Store {
        open_store(
            StoreOptions::new(&self.database(), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(MASTER_KEY),
        )
        .expect("open store")
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
         \"platform\":\"desktop\",\"capabilities\":[\"PrivateStorage\",\"Command\"],\
         \"permissions\":[],\"extensionPoints\":[\"Command\"],\"entryClass\":\"{ENTRY_CLASS}\"}}"
    )
}

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

#[test]
fn keep_policy_keeps_the_kv_readable_after_an_upgrade() {
    let dir = FixtureDir::new();
    let host = dir.open();
    host.install_package(package("1.0.0"), true).expect("install v1");
    host.start("notes.v1".to_owned()).expect("start");
    host.storage_set("notes.v1".to_owned(), "draft".to_owned(), b"hello".to_vec())
        .expect("kv write");
    let upgraded = host
        .install_package(package("1.1.0"), true)
        .expect("upgrade");
    assert_eq!(upgraded.upgraded_from.as_deref(), Some("1.0.0"));
    // Keep（默认）：旧键仍可读，值不变。
    assert_eq!(
        host.storage_get("notes.v1".to_owned(), "draft".to_owned())
            .expect("kv read after upgrade"),
        Some(b"hello".to_vec())
    );
    host.shutdown();

    // 重开仍然可读（KV 与插件行都在库里）。
    let reopened = dir.open();
    assert_eq!(
        reopened
            .storage_get("notes.v1".to_owned(), "draft".to_owned())
            .expect("kv read after reopen"),
        Some(b"hello".to_vec())
    );
    assert_eq!(reopened.list_plugins().expect("list")[0].version, "1.1.0");
}

#[test]
fn namespace_policy_copies_old_values_and_keeps_the_original_keys() {
    let dir = FixtureDir::new();
    let host = dir.open();
    host.install_package(package("1.0.0"), true).expect("install v1");
    host.start("notes.v1".to_owned()).expect("start");
    host.storage_set("notes.v1".to_owned(), "a".to_owned(), b"A".to_vec())
        .expect("write a");
    host.storage_set("notes.v1".to_owned(), "b".to_owned(), b"B".to_vec())
        .expect("write b");
    host.shutdown();

    let mut store = dir.store();
    let report = store
        .plugin_kv_rename_or_keep("notes.v1", "1.0.0", "2.0.0", KvMigrationPolicy::Namespace)
        .expect("namespace migration");
    assert_eq!(report.copied, 2);
    assert_eq!(report.kept, 2);
    assert_eq!(report.prefix.as_deref(), Some("v1.0.0/"));
    // 新前缀可读，原键仍在（绝不静默删除）。
    assert_eq!(
        store.plugin_kv_get("notes.v1", "v1.0.0/a").expect("read"),
        Some(b"A".to_vec())
    );
    assert_eq!(store.plugin_kv_get("notes.v1", "a").expect("read"), Some(b"A".to_vec()));
    // 幂等重跑：不再复制。
    let again = store
        .plugin_kv_rename_or_keep("notes.v1", "1.0.0", "2.0.0", KvMigrationPolicy::Namespace)
        .expect("idempotent migration");
    assert_eq!(again.copied, 0);
    assert_eq!(store.plugin_kv_keys("notes.v1").expect("keys").len(), 4);
}

#[test]
fn purging_uninstalled_plugins_removes_rows_and_their_kv() {
    let dir = FixtureDir::new();
    // 直接造两条 uninstalled 行（一条新、一条旧），再通过 FFI 清理。
    let mut store = dir.store();
    store
        .upsert_plugin("old.v1", &manifest_json("1.0.0"), STATE_UNINSTALLED)
        .expect("old row");
    store.plugin_kv_set("old.v1", "k", b"v").expect("old kv");
    store
        .upsert_plugin("new.v1", &manifest_json("1.0.0"), STATE_UNINSTALLED)
        .expect("new row");
    store.plugin_kv_set("new.v1", "k", b"v").expect("new kv");
    // 阈值取得足够大（upsert 用的是系统 now_ms）。
    let threshold = u64::try_from(i64::MAX / 2).expect("threshold");
    drop(store);

    let host = dir.open();
    let removed = host
        .purge_uninstalled_plugins(threshold)
        .expect("purge runs");
    assert_eq!(removed, 2, "both uninstalled rows are purged");
    host.shutdown();
    // 行与 KV 都没了。
    let store = dir.store();
    assert!(store.load_plugins().expect("load").is_empty());
    assert!(store.plugin_kv_keys("old.v1").expect("keys").is_empty());
    assert!(store.plugin_kv_get("new.v1", "k").expect("get").is_none());
}

#[test]
fn the_purge_threshold_is_strict_and_bounded() {
    let dir = FixtureDir::new();
    let now_ms = 1_700_000_000_000_i64;
    let mut store = dir.store();
    store
        .upsert_plugin("edge.v1", &manifest_json("1.0.0"), STATE_UNINSTALLED)
        .expect("row");
    let updated = store.load_plugins().expect("rows")[0].updated_at_ms;
    // 等值阈值：不删（严格小于）。
    assert_eq!(
        store
            .purge_uninstalled_plugins(updated, MAX_PLUGIN_PURGE)
            .expect("purge at the boundary"),
        0
    );
    // 阈值 +1：删。
    assert_eq!(
        store
            .purge_uninstalled_plugins(updated + 1, MAX_PLUGIN_PURGE)
            .expect("purge past the boundary"),
        1
    );
    // 上限越界显式拒绝。
    assert!(store.purge_uninstalled_plugins(updated, 0).is_err());
    assert!(
        store
            .purge_uninstalled_plugins(updated, MAX_PLUGIN_PURGE + 1)
            .is_err()
    );
    // 已安装（非 uninstalled）的行永不被清理。
    store
        .upsert_plugin("live.v1", &manifest_json("1.0.0"), "installed")
        .expect("installed row");
    assert_eq!(
        store
            .purge_uninstalled_plugins(now_ms + 1_000_000, MAX_PLUGIN_PURGE)
            .expect("purge keeps installed rows"),
        0
    );
}

#[test]
fn the_purge_batch_is_capped_at_256_rows() {
    let dir = FixtureDir::new();
    let mut store = dir.store();
    // 造 258 条 uninstalled 行；单次最多删 256。
    for index in 0..258_u32 {
        store
            .upsert_plugin(
                &format!("bulk{index}.v1"),
                &manifest_json("1.0.0"),
                STATE_UNINSTALLED,
            )
            .expect("bulk row");
    }
    let threshold = i64::MAX / 2; // 远大于当前时间：所有行都过阈值
    assert_eq!(
        store
            .purge_uninstalled_plugins(threshold, MAX_PLUGIN_PURGE)
            .expect("first batch"),
        256
    );
    assert_eq!(store.load_plugins().expect("rows").len(), 2, "remaining rows wait for the next batch");
    assert_eq!(
        store
            .purge_uninstalled_plugins(threshold, MAX_PLUGIN_PURGE)
            .expect("second batch"),
        2
    );
    assert!(store.load_plugins().expect("rows").is_empty());
}
