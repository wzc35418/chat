//! PLUGINS：插件包结构校验的 FFI 用例（测试内构造最小 ZIP，不经任何插件代码）。
//!
//! 覆盖：合法包 → PackageViewFfi 字段正确；缺清单/缺入口/SPI 不符/路径穿越/超量各自透出稳定码；
//! 关闭后 CORE_CLOSED。

use std::sync::Arc;

use chat_ffi::local_store::FfiError;
use chat_ffi::plugins_ffi::PluginsFfi;

const ENTRY_CLASS: &str = "com.example.Plugin";
const SPI_FILE: &str = "META-INF/services/com.example.Plugin";
const CLASS_FILE: &str = "com/example/Plugin.class";

fn manifest_json(entry_class: &str) -> String {
    format!(
        "{{\"schemaVersion\":1,\"id\":\"notes.v1\",\"version\":\"2.0.0\",\"sdkVersion\":1,\
         \"platform\":\"desktop\",\"capabilities\":[\"Command\"],\"permissions\":[],\
         \"extensionPoints\":[\"Command\"],\"entryClass\":\"{entry_class}\"}}"
    )
}

/// 最小 ZIP 构造器（stored 条目、无 extra/comment；CRC 写 0，本层不校验 CRC）。
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

fn code(error: FfiError) -> String {
    match error {
        FfiError::Store { code, .. } => code,
    }
}

fn valid_package() -> Vec<u8> {
    let manifest = manifest_json(ENTRY_CLASS);
    zip(&[
        ("manifest.json", manifest.as_bytes()),
        (CLASS_FILE, b"\xca\xfe\xba\xbe"),
        (SPI_FILE, ENTRY_CLASS.as_bytes()),
    ])
}

#[test]
fn inspect_package_reports_the_structure_and_rejects_broken_packages() {
    let host = PluginsFfi::new();
    let view = host.inspect_package(valid_package()).expect("valid package");
    assert_eq!(view.plugin_id, "notes.v1");
    assert_eq!(view.plugin_version, "2.0.0");
    assert_eq!(view.entry_class, ENTRY_CLASS);
    assert_eq!(view.entry_file, CLASS_FILE);
    assert_eq!(view.spi_file, SPI_FILE);
    assert_eq!(view.entry_count, 3);
    assert!(view.compressed_bytes > 0);

    // 缺清单。
    let missing = zip(&[(CLASS_FILE, b"x"), (SPI_FILE, ENTRY_CLASS.as_bytes())]);
    assert_eq!(
        code(host.inspect_package(missing).expect_err("missing manifest")),
        "PLUGIN_PACKAGE_MISSING_MANIFEST"
    );
    // 缺入口类。
    let no_entry = zip(&[
        ("manifest.json", manifest_json(ENTRY_CLASS).as_bytes()),
        (SPI_FILE, ENTRY_CLASS.as_bytes()),
    ]);
    assert_eq!(
        code(host.inspect_package(no_entry).expect_err("missing entry")),
        "PLUGIN_PACKAGE_MISSING_ENTRY"
    );
    // SPI 不符。
    let bad_spi = zip(&[
        ("manifest.json", manifest_json(ENTRY_CLASS).as_bytes()),
        (CLASS_FILE, b"x"),
        (SPI_FILE, b"com.example.Other"),
    ]);
    assert_eq!(
        code(host.inspect_package(bad_spi).expect_err("spi mismatch")),
        "PLUGIN_PACKAGE_SPI_MISMATCH"
    );
    // 路径穿越。
    let traversal = zip(&[
        ("manifest.json", manifest_json(ENTRY_CLASS).as_bytes()),
        (CLASS_FILE, b"x"),
        (SPI_FILE, ENTRY_CLASS.as_bytes()),
        ("../evil.class", b"x"),
    ]);
    assert_eq!(
        code(host.inspect_package(traversal).expect_err("unsafe path")),
        "PLUGIN_PACKAGE_UNSAFE_PATH"
    );
    // 超量（> 8 MiB）。
    let huge = vec![0_u8; chat_plugins::MAX_PACKAGE_BYTES + 1];
    assert_eq!(
        code(host.inspect_package(huge).expect_err("too large")),
        "PLUGIN_PACKAGE_TOO_LARGE"
    );
    // 不是 ZIP。
    assert_eq!(
        code(host.inspect_package(b"nope".to_vec()).expect_err("corrupted")),
        "PLUGIN_PACKAGE_CORRUPTED"
    );

    // 关闭后 CORE_CLOSED（仍然不执行任何插件代码）。
    host.shutdown();
    assert_eq!(
        code(host.inspect_package(valid_package()).expect_err("closed")),
        "CORE_CLOSED"
    );
    let _: Arc<PluginsFfi> = host;
}
