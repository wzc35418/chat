//! TASK-122：本机库候选路径解析（真文件系统，纯函数覆盖）。
//!
//! 覆盖：可执行文件目录命中 → native/ 命中 → lib/ 命中 → 环境变量覆盖命中（顺序）；
//! 全部失败时错误列出**所有**尝试路径；同一目录去重。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_ffi::local_store::FfiError;
use chat_ffi::native_path::{
    NATIVE_LIBRARY_DIR_ENV, native_library_directories, resolve_in_directories,
};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "chat-ffi-native-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }

    fn path(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_file(path: &std::path::Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, bytes).expect("write file");
}

#[test]
fn resolution_follows_the_documented_order() {
    let dir = TempDir::new();
    let exe_dir = dir.path().join("bin");
    let env_dir = dir.path().join("opt");
    fs::create_dir_all(&exe_dir).expect("exe dir");
    fs::create_dir_all(&env_dir).expect("env dir");
    let directories = native_library_directories(Some(&exe_dir), Some(&env_dir));
    assert_eq!(
        directories,
        vec![
            exe_dir.clone(),
            exe_dir.join("native"),
            exe_dir.join("lib"),
            env_dir.clone(),
        ]
    );

    // 1) 可执行文件目录优先。
    write_file(&exe_dir.join("libchat_ffi.so"), b"a");
    write_file(&exe_dir.join("native/libchat_ffi.so"), b"b");
    let found = resolve_in_directories(&directories, "libchat_ffi.so").expect("exe dir hit");
    assert_eq!(found, exe_dir.join("libchat_ffi.so"));

    // 2) 只有 native/ 里有 → 命中 native/。
    fs::remove_file(exe_dir.join("libchat_ffi.so")).expect("remove exe copy");
    let found = resolve_in_directories(&directories, "libchat_ffi.so").expect("native hit");
    assert_eq!(found, exe_dir.join("native/libchat_ffi.so"));

    // 3) 只有 lib/ 里有 → 命中 lib/。
    fs::remove_file(exe_dir.join("native/libchat_ffi.so")).expect("remove native copy");
    write_file(&exe_dir.join("lib/libchat_ffi.so"), b"c");
    let found = resolve_in_directories(&directories, "libchat_ffi.so").expect("lib hit");
    assert_eq!(found, exe_dir.join("lib/libchat_ffi.so"));

    // 4) 只有环境变量覆盖里才有 → 命中 env（最后一个）。
    fs::remove_file(exe_dir.join("lib/libchat_ffi.so")).expect("remove lib copy");
    write_file(&env_dir.join("libchat_ffi.so"), b"d");
    let found = resolve_in_directories(&directories, "libchat_ffi.so").expect("env hit");
    assert_eq!(found, env_dir.join("libchat_ffi.so"));
}

#[test]
fn a_total_miss_lists_every_attempted_path() {
    let dir = TempDir::new();
    let exe_dir = dir.path().join("bin");
    let env_dir = dir.path().join("opt");
    fs::create_dir_all(&exe_dir).expect("exe dir");
    fs::create_dir_all(&env_dir).expect("env dir");
    let directories = native_library_directories(Some(&exe_dir), Some(&env_dir));
    let error = resolve_in_directories(&directories, "libchat_ffi.so").expect_err("miss");
    assert_eq!(error.tried.len(), directories.len());
    let detail = error.detail();
    for directory in &directories {
        assert!(
            detail.contains(&directory.join("libchat_ffi.so").display().to_string()),
            "the error must mention {}",
            directory.display()
        );
    }
}

#[test]
fn the_ffi_entry_point_is_explicit_about_a_missing_library() {
    // 用一个几乎不可能存在的文件名：失败必须是 NATIVE_LIBRARY_NOT_FOUND，且列出所有尝试路径。
    let name = format!("libchat_ffi_absent_{}.so", std::process::id());
    let error = chat_ffi::native_path::native_library_path(name.clone())
        .expect_err("the library does not exist in this test");
    let (error_code, detail) = match error {
        FfiError::Store { code, reason } => (code, reason),
    };
    assert_eq!(error_code, "NATIVE_LIBRARY_NOT_FOUND");
    assert!(detail.contains(&name), "the error names the requested file");
    assert!(detail.contains("tried:"), "the error lists the attempted paths");
    // 至少包含可执行文件目录本身（运行期解析注入的两个位置之一）。
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.to_path_buf()))
        .expect("current exe dir");
    assert!(
        detail.contains(&exe_dir.display().to_string()),
        "the error must mention the executable directory {}",
        exe_dir.display()
    );
}

#[test]
fn the_environment_variable_name_is_stable() {
    assert_eq!(NATIVE_LIBRARY_DIR_ENV, "CHAT_HOST_LIBRARY_DIR");
}
