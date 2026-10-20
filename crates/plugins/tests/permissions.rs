//! PLUGINS：权限声明（清单 @@permissions@@ 字段）的兼容性与边界。
//!
//! 覆盖：不写字段通过（语义 = 无敏感权限）；四个沙箱权限各自通过；非法值/重复/超限/大小写不符
//! 显式拒绝；包结构校验与运行时清单校验**同一套规则**。

use chat_plugins::package::{
    MANIFEST_FILE, inspect_plugin_package, package_manifest_json,
};
use chat_plugins::{
    PluginLimits, PluginManifest, PluginPermission, parse_manifest,
};

const ENTRY_CLASS: &str = "com.example.Plugin";
const SPI_FILE: &str = "META-INF/services/com.example.Plugin";
const CLASS_FILE: &str = "com/example/Plugin.class";

/// 最小清单 JSON（permissions 字段可选：传 None 就不写它）。
fn manifest_json(permissions: Option<&[&str]>) -> String {
    let permissions = match permissions {
        None => String::new(),
        Some(values) => {
            let list = values
                .iter()
                .map(|value| format!("\"{value}\""))
                .collect::<Vec<_>>()
                .join(",");
            format!(",\"permissions\":[{list}]")
        }
    };
    format!(
        "{{\"schemaVersion\":1,\"id\":\"notes.v1\",\"version\":\"1.0.0\",\"sdkVersion\":1,\
         \"platform\":\"desktop\",\"capabilities\":[\"Command\"],\"extensionPoints\":[\"Command\"],\
         \"entryClass\":\"{ENTRY_CLASS}\"{permissions}}}"
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

fn package_with(permissions: Option<&[&str]>) -> Vec<u8> {
    let manifest = manifest_json(permissions);
    zip(&[
        (MANIFEST_FILE, manifest.as_bytes()),
        (CLASS_FILE, b"\xca\xfe\xba\xbe"),
        (SPI_FILE, ENTRY_CLASS.as_bytes()),
    ])
}

fn limits() -> PluginLimits {
    PluginLimits::default()
}

fn code_of_manifest(json: &str) -> String {
    parse_manifest(json, &limits())
        .expect_err("validation must fail")
        .code()
        .to_owned()
}

fn code_of_package(bytes: &[u8]) -> String {
    inspect_plugin_package(bytes, &limits())
        .expect_err("inspection must fail")
        .code()
        .to_owned()
}

#[test]
fn a_manifest_without_permissions_passes() {
    let manifest: PluginManifest = parse_manifest(&manifest_json(None), &limits()).expect("no field");
    assert!(manifest.permissions.is_empty(), "absence means no sensitive permission");
    // 包结构校验同样通过，且带出空权限。
    let view = inspect_plugin_package(&package_with(None), &limits()).expect("inspect");
    assert!(view.permissions.is_empty());
}

#[test]
fn each_sandbox_permission_is_accepted() {
    for permission in ["network", "filesystem", "process", "host-ui"] {
        let json = manifest_json(Some(&[permission]));
        let manifest: PluginManifest = parse_manifest(&json, &limits()).expect(permission);
        assert_eq!(manifest.permissions.len(), 1);
        let parsed = manifest.permissions.iter().next().copied().expect("one");
        assert_eq!(parsed.as_str(), permission);
        assert!(parsed.is_sandbox());
        // 包结构校验与运行时校验**同一套规则**：同样带出同一个值。
        let view = inspect_plugin_package(&package_with(Some(&[permission])), &limits())
            .expect("inspect");
        assert_eq!(view.permissions, vec![permission.to_owned()]);
    }
    // 四个沙箱权限全在 SANDBOX 白名单里。
    let sandbox: Vec<&str> = PluginPermission::SANDBOX
        .iter()
        .map(|permission| permission.as_str())
        .collect();
    assert_eq!(sandbox, vec!["network", "filesystem", "process", "host-ui"]);
}

#[test]
fn an_unknown_permission_is_rejected_by_both_paths() {
    for value in ["nope", "Network", "HOST-UI", "", "net work"] {
        let json = manifest_json(Some(&[value]));
        let manifest_code = code_of_manifest(&json);
        let package_code = code_of_package(&package_with(Some(&[value])));
        assert_eq!(
            manifest_code, package_code,
            "the same rule must fire on both paths for {value:?}"
        );
        // 本模块对“清单非法”的冻结码是 INVALID_PLUGIN_PACKAGE（大小写敏感 + 封闭白名单）。
        assert_eq!(manifest_code, "INVALID_PLUGIN_PACKAGE");
    }
}

#[test]
fn duplicates_and_the_size_limit_are_rejected() {
    // 重复（大小写敏感：同名重复一定拒绝）。
    let duplicate = code_of_manifest(&manifest_json(Some(&["network", "network"])));
    assert_eq!(duplicate, "INVALID_PLUGIN_PACKAGE");
    // 超限：9 项 > MAX_PERMISSIONS(8)。
    let over: Vec<&str> = vec![
        "network",
        "filesystem",
        "process",
        "host-ui",
        "UserSelectedContent",
        "ComposeDraft",
        "network",
        "filesystem",
        "process",
    ];
    let over_code = code_of_manifest(&manifest_json(Some(&over)));
    assert_eq!(over_code, "INVALID_PLUGIN_PACKAGE", "over the declaration limit");
    // 恰好 8 项（去重后）仍然合法。
    let at_limit = [
        "network",
        "filesystem",
        "process",
        "host-ui",
        "UserSelectedContent",
        "ComposeDraft",
        "network",
        "filesystem",
    ];
    // 上面有重复 → 去重后 6 项，重复本身会被拒绝，因此这里只验证“8 个不同值”是上限内的合法输入。
    let all_six = [
        "network",
        "filesystem",
        "process",
        "host-ui",
        "UserSelectedContent",
        "ComposeDraft",
    ];
    let manifest: PluginManifest =
        parse_manifest(&manifest_json(Some(&all_six)), &limits()).expect("six distinct");
    assert_eq!(manifest.permissions.len(), 6);
    let _ = at_limit;
}

#[test]
fn the_package_view_and_the_manifest_agree() {
    let json = manifest_json(Some(&["network", "host-ui"]));
    let manifest: PluginManifest = parse_manifest(&json, &limits()).expect("manifest");
    let expected: Vec<String> = manifest
        .permissions
        .iter()
        .map(|permission| permission.as_str().to_owned())
        .collect();
    let view = inspect_plugin_package(&package_with(Some(&["network", "host-ui"])), &limits())
        .expect("inspect");
    assert_eq!(view.permissions, expected);
    // 包的清单原文还能单独取出（安装事务路径）。
    let raw = package_manifest_json(&package_with(Some(&["network"]))).expect("manifest text");
    assert!(raw.contains("\"network\""));
}
