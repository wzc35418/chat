//! 安装版可用的本机库解析与应用信息（打包后也能找到 host 动态库、显示真实版本）。
//!
//! 语义：
//!
//! - `app_info()` 只读编译期常量与 `std::env::consts` —— 不联网、不读库、不会失败；
//! - 本机库候选路径按固定顺序尝试：**可执行文件目录 → 其 native/ → 其 lib/ → 环境变量覆盖**；
//! - 全部失败时返回的错误**列出所有尝试过的路径**（绝不静默回退到"系统默认库"）。

use std::path::{Path, PathBuf};

/// 环境变量：显式指定 host 动态库所在目录（**最后**尝试，作为部署方的显式覆盖）。
pub const NATIVE_LIBRARY_DIR_ENV: &str = "CHAT_HOST_LIBRARY_DIR";

/// FFI 契约版本（桥接记录/方法签名变化时递增；与库版本独立）。
pub const FFI_CONTRACT_VERSION: &str = "1.0";

/// 应用与桥接的版本信息（纯数据，不含任何密钥）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppInfoFfi {
    /// 库版本（`CARGO_PKG_VERSION`，形如 `0.1.0`）。
    pub version: String,
    /// 构建配置：`debug` 或 `release`。
    pub build_profile: String,
    /// FFI 契约版本（与库版本独立）。
    pub ffi_version: String,
    /// 平台：`<OS>/<ARCH>`（来自 `std::env::consts`）。
    pub platform: String,
}

/// 本机库解析失败：保留**全部**尝试过的路径（调用方据此定位部署问题）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeLibraryError {
    /// 尝试过的路径（按尝试顺序）。
    pub tried: Vec<PathBuf>,
    /// 期望的文件名。
    pub file_name: String,
}

impl NativeLibraryError {
    /// 逐行列出所有尝试路径（诊断文本）。
    pub fn detail(&self) -> String {
        let mut text = format!("native library `{}` not found; tried:", self.file_name);
        for path in &self.tried {
            text.push_str("\n  - ");
            text.push_str(&path.display().to_string());
        }
        text
    }
}

impl std::fmt::Display for NativeLibraryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail())
    }
}

impl std::error::Error for NativeLibraryError {}

/// 应用/桥接版本信息（编译期常量 + `std::env::consts`，不会失败）。
#[uniffi::export]
pub fn app_info() -> AppInfoFfi {
    AppInfoFfi {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        build_profile: if cfg!(debug_assertions) {
            "debug".to_owned()
        } else {
            "release".to_owned()
        },
        ffi_version: FFI_CONTRACT_VERSION.to_owned(),
        platform: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
    }
}

/// 本机库候选目录（**纯函数**：按固定顺序去重，便于单测用真文件系统覆盖）。
///
/// 顺序：`exe_dir` → `exe_dir/native` → `exe_dir/lib` → `env_override`（最后一个）。
pub fn native_library_directories(
    exe_dir: Option<&Path>,
    env_override: Option<&Path>,
) -> Vec<PathBuf> {
    let mut directories: Vec<PathBuf> = Vec::new();
    if let Some(directory) = exe_dir {
        directories.push(directory.to_path_buf());
        directories.push(directory.join("native"));
        directories.push(directory.join("lib"));
    }
    if let Some(directory) = env_override {
        directories.push(directory.to_path_buf());
    }
    let mut unique: Vec<PathBuf> = Vec::new();
    for directory in directories {
        if !unique.contains(&directory) {
            unique.push(directory);
        }
    }
    unique
}

/// 在候选目录里按序找文件；命中返回**第一个**存在的路径。
pub fn resolve_in_directories(
    directories: &[PathBuf],
    file_name: &str,
) -> Result<PathBuf, NativeLibraryError> {
    let mut tried: Vec<PathBuf> = Vec::with_capacity(directories.len());
    for directory in directories {
        let candidate = directory.join(file_name);
        if candidate.is_file() {
            return Ok(candidate);
        }
        tried.push(candidate);
    }
    Err(NativeLibraryError {
        tried,
        file_name: file_name.to_owned(),
    })
}

/// 解析本机库路径（可执行文件目录 + 环境变量覆盖，按固定顺序尝试）。
pub fn resolve_native_library(file_name: &str) -> Result<PathBuf, NativeLibraryError> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let env_override = std::env::var_os(NATIVE_LIBRARY_DIR_ENV).map(PathBuf::from);
    let directories = native_library_directories(exe_dir.as_deref(), env_override.as_deref());
    resolve_in_directories(&directories, file_name)
}

/// 失败时把**所有尝试路径**放进错误里（失败码 `NATIVE_LIBRARY_NOT_FOUND`）。
fn native_failure(error: NativeLibraryError) -> crate::local_store::FfiError {
    crate::local_store::FfiError::Store {
        code: "NATIVE_LIBRARY_NOT_FOUND".to_owned(),
        reason: error.detail(),
    }
}

/// 解析 host 动态库路径（可执行文件目录 → native/ → lib/ → 环境变量覆盖）。
///
/// 全失败返回 `NATIVE_LIBRARY_NOT_FOUND`，且 `reason` 里**列出所有尝试过的路径**。
#[uniffi::export]
pub fn native_library_path(file_name: String) -> Result<String, crate::local_store::FfiError> {
    resolve_native_library(&file_name)
        .map(|path| path.display().to_string())
        .map_err(native_failure)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_info_reports_compile_time_values() {
        let info = app_info();
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert!(info.build_profile == "debug" || info.build_profile == "release");
        assert_eq!(info.ffi_version, FFI_CONTRACT_VERSION);
        assert!(info.platform.contains('/'));
    }

    #[test]
    fn candidate_directories_are_ordered_and_deduplicated() {
        let directories = native_library_directories(
            Some(Path::new("/app/bin")),
            Some(Path::new("/opt/chat/lib")),
        );
        assert_eq!(
            directories,
            vec![
                PathBuf::from("/app/bin"),
                PathBuf::from("/app/bin/native"),
                PathBuf::from("/app/bin/lib"),
                PathBuf::from("/opt/chat/lib"),
            ]
        );
        let same =
            native_library_directories(Some(Path::new("/app/bin")), Some(Path::new("/app/bin")));
        assert_eq!(
            same,
            vec![
                PathBuf::from("/app/bin"),
                PathBuf::from("/app/bin/native"),
                PathBuf::from("/app/bin/lib"),
            ]
        );
        let only_env = native_library_directories(None, Some(Path::new("/opt/chat/lib")));
        assert_eq!(only_env, vec![PathBuf::from("/opt/chat/lib")]);
    }

    #[test]
    fn a_missing_library_lists_every_attempted_path() {
        let directories = vec![
            PathBuf::from("/tmp/chat-none/bin"),
            PathBuf::from("/tmp/chat-none/lib"),
        ];
        let error =
            resolve_in_directories(&directories, "libchat_ffi.so").expect_err("must fail");
        assert_eq!(error.tried.len(), 2, "every candidate is recorded");
        let detail = error.detail();
        // 用平台自己的路径拼接/显示（Windows 是反斜杠）。
        for directory in &directories {
            let expected = directory.join("libchat_ffi.so").display().to_string();
            assert!(detail.contains(&expected), "the error must mention {expected}");
        }
    }
}
