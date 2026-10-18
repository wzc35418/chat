//! 插件宿主持久化用例（真临时加密库）：安装/卸载/KV 在重开对象后仍然一致。
//!
//! 覆盖：安装 → 写 KV → shutdown → 重开 → 仍列出且 KV 可读；卸载 delete_data=true 清 KV、
//! false 保留；配额超限拒绝且不写入；损坏清单行跳过并计数。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_ffi::local_store::FfiError;
use chat_ffi::plugins_ffi::PluginsFfi;
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, StoreOptions, open_store};

const MASTER_KEY: [u8; 32] = [0x5a; 32];

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
            "chat-ffi-plugins-persist-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> String {
        self.0.join("chat.db").to_string_lossy().into_owned()
    }

    fn open(&self) -> std::sync::Arc<PluginsFfi> {
        PluginsFfi::open(self.database(), master_key_hex()).expect("open persistent plugins")
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

fn manifest(id: &str, capabilities: &[&str], extension_points: &[&str]) -> String {
    let capabilities = capabilities
        .iter()
        .map(|item| format!("\"{item}\""))
        .collect::<Vec<_>>()
        .join(",");
    let extension_points = extension_points
        .iter()
        .map(|item| format!("\"{item}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"schemaVersion\":1,\"id\":\"{id}\",\"version\":\"1.0.0\",\"sdkVersion\":1,\
         \"platform\":\"desktop\",\"capabilities\":[{capabilities}],\
         \"permissions\":[],\"extensionPoints\":[{extension_points}],\
         \"entryClass\":\"com.example.Plugin\"}}"
    )
}

fn code(error: FfiError) -> String {
    match error {
        FfiError::Store { code, .. } => code,
    }
}

#[test]
fn install_and_kv_survive_a_reopen() {
    let dir = FixtureDir::new();
    let first = dir.open();
    let plugin_id = first
        .install(manifest(
            "notes.v1",
            &["PrivateStorage", "Command"],
            &["Command"],
        ))
        .expect("install");
    first.start(plugin_id.clone()).expect("start");
    first
        .storage_set(plugin_id.clone(), "draft".to_owned(), b"hello".to_vec())
        .expect("kv write");
    // 状态行也落库（running）。
    assert_eq!(first.list_plugins().expect("list").len(), 1);
    first.shutdown();
    assert!(!first.is_open());

    // 重开对象：仍列出、KV 可读；运行状态不恢复（需要上层再 start）。
    let second = dir.open();
    let plugins = second.list_plugins().expect("list after reopen");
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0].plugin_id, "notes.v1");
    assert!(!plugins[0].running, "running state is not restored");
    assert_eq!(
        second
            .storage_get(plugin_id.clone(), "draft".to_owned())
            .expect("kv read after reopen"),
        Some(b"hello".to_vec())
    );
    // 重开后再启动可以继续用。
    second.start(plugin_id.clone()).expect("start after reopen");
    second
        .storage_set(plugin_id.clone(), "draft".to_owned(), b"hello2".to_vec())
        .expect("kv overwrite");
    assert_eq!(
        second
            .storage_get(plugin_id, "draft".to_owned())
            .expect("kv read"),
        Some(b"hello2".to_vec())
    );
}

#[test]
fn uninstall_with_or_without_data_removal() {
    let dir = FixtureDir::new();
    // 情形一：delete_data = false → 行标记未安装、KV 保留（重装后仍可读）。
    {
        let host = dir.open();
        let plugin_id = host
            .install(manifest("keep.v1", &["PrivateStorage", "Command"], &["Command"]))
            .expect("install keep");
        host.start(plugin_id.clone()).expect("start");
        host.storage_set(plugin_id.clone(), "k".to_owned(), b"kept".to_vec())
            .expect("write");
        assert!(host.uninstall(plugin_id.clone(), false).expect("uninstall"));
        assert!(host.list_plugins().expect("list").is_empty());
        host.shutdown();
    }
    {
        let host = dir.open();
        // 重装后保留的 KV 仍可读。
        host.install(manifest("keep.v1", &["PrivateStorage", "Command"], &["Command"]))
            .expect("reinstall");
        assert_eq!(
            host.storage_get("keep.v1".to_owned(), "k".to_owned())
                .expect("kv kept"),
            Some(b"kept".to_vec())
        );
        // 情形二：delete_data = true → 行与 KV 都清掉。
        assert!(host.uninstall("keep.v1".to_owned(), true).expect("uninstall"));
        host.shutdown();
    }
    {
        let host = dir.open();
        assert!(host.list_plugins().expect("list").is_empty());
        host.install(manifest("keep.v1", &["PrivateStorage", "Command"], &["Command"]))
            .expect("reinstall");
        assert_eq!(
            host.storage_get("keep.v1".to_owned(), "k".to_owned())
                .expect("kv gone"),
            None,
            "delete_data=true must clear the KV rows"
        );
    }
}

#[test]
fn quota_exceeded_is_rejected_and_not_written() {
    let dir = FixtureDir::new();
    let host = dir.open();
    let plugin_id = host
        .install(manifest("big.v1", &["PrivateStorage", "Command"], &["Command"]))
        .expect("install");
    host.start(plugin_id.clone()).expect("start");
    // 单值上限 64 KiB（storage 层），超出即拒绝。
    let too_big = vec![0x41_u8; 64 * 1024 + 1];
    let error = host
        .storage_set(plugin_id.clone(), "big".to_owned(), too_big)
        .expect_err("quota exceeded");
    assert_eq!(code(error), "PLUGIN_STORAGE_QUOTA_EXCEEDED");
    // 没有写进去。
    assert_eq!(
        host.storage_get(plugin_id.clone(), "big".to_owned())
            .expect("read"),
        None
    );
    // 合法大小仍可写。
    host.storage_set(plugin_id, "ok".to_owned(), vec![0x42_u8; 1024])
        .expect("within quota");
}

#[test]
fn corrupted_manifest_rows_are_skipped_and_counted() {
    let dir = FixtureDir::new();
    // 直接往库里写一条合法行 + 一条损坏清单行。
    {
        let mut store = open_store(
            StoreOptions::new(dir.database(), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(MASTER_KEY),
        )
        .expect("open store");
        store
            .upsert_plugin(
                "good.v1",
                &manifest("good.v1", &["PrivateStorage", "Command"], &["Command"]),
                "installed",
            )
            .expect("seed good row");
        store
            .upsert_plugin("bad.v1", "not-a-manifest", "installed")
            .expect("seed corrupted row");
    }
    let host = dir.open();
    assert_eq!(host.skipped_on_load().expect("skipped"), 1);
    let plugins = host.list_plugins().expect("list");
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0].plugin_id, "good.v1");
    // 损坏行不阻塞后续操作。
    let plugin_id = host
        .install(manifest("new.v1", &["PrivateStorage", "Command"], &["Command"]))
        .expect("install after skip");
    assert!(plugin_id.starts_with("new."));
    assert_eq!(host.skipped_on_load().expect("skipped"), 1);
}
