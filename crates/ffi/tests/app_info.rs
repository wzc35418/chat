//! TASK-122：应用信息与版本（真编译期常量，无 I/O）。
//!
//! 覆盖：字段非空、version 形状正确（MAJOR.MINOR.PATCH）、平台形如 <OS>/<ARCH>、
//! FFI 契约版本与库版本各自独立但都不为空。

use chat_ffi::native_path::{FFI_CONTRACT_VERSION, app_info};

fn is_semverish(text: &str) -> bool {
    let core = text.split(['-', '+']).next().unwrap_or_default();
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

#[test]
fn app_info_reports_real_build_metadata() {
    let info = app_info();
    assert!(!info.version.is_empty());
    assert!(is_semverish(&info.version), "version must look like MAJOR.MINOR.PATCH: {}", info.version);
    assert_eq!(info.version, env!("CARGO_PKG_VERSION"), "the version comes from the crate metadata");
    assert!(
        info.build_profile == "debug" || info.build_profile == "release",
        "unexpected build profile: {}",
        info.build_profile
    );
    assert!(!info.ffi_version.is_empty());
    assert_eq!(info.ffi_version, FFI_CONTRACT_VERSION);
    // 平台必须形如 <OS>/<ARCH>，且两段都非空。
    let mut parts = info.platform.split('/');
    let os = parts.next().unwrap_or_default();
    let arch = parts.next().unwrap_or_default();
    assert!(!os.is_empty(), "OS segment must not be empty");
    assert!(!arch.is_empty(), "ARCH segment must not be empty");
    assert!(parts.next().is_none(), "platform has exactly two segments");
    assert_eq!(os, std::env::consts::OS);
    assert_eq!(arch, std::env::consts::ARCH);
}

#[test]
fn app_info_is_stable_across_calls() {
    // 纯编译期常量：重复调用逐字段相同（导出给绑定层的就是这个函数）。
    assert_eq!(app_info(), app_info());
}
