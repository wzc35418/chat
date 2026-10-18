//! 插件宿主最小闭环验收用例。
//!
//! 覆盖：清单非法/重复/未知扩展点/缺能力、安装与重复 ID、启动-调用-停止-卸载生命周期、
//! 扩展点查询、跨插件私有存储隔离、输入/输出/超时边界、operationId 幂等重放。

use chat_plugins::*;
use chat_protocol::OperationId;

fn manifest_json(id: &str, capabilities: &[&str], points: &[&str]) -> String {
    let capabilities: Vec<String> = capabilities
        .iter()
        .map(|item| format!("\"{item}\""))
        .collect();
    let points: Vec<String> = points.iter().map(|item| format!("\"{item}\"")).collect();
    format!(
        r#"{{"schemaVersion":1,"id":"{id}","version":"1.0.0","sdkVersion":1,"platform":"desktop","capabilities":[{}],"permissions":["UserSelectedContent"],"extensionPoints":[{}],"entryClass":"com.example.Notes"}}"#,
        capabilities.join(","),
        points.join(",")
    )
}

fn notes_manifest() -> String {
    manifest_json(
        "notes.v1",
        &["PrivateStorage", "Command", "MessageAction"],
        &["Command", "MessageAction"],
    )
}

fn operation(seed: u8) -> OperationId {
    OperationId::from_bytes([seed; 32])
}

fn plugin(id: &str) -> PluginId {
    PluginId::parse(id).unwrap()
}

/// 安装并启动一个插件，返回运行时句柄。
fn started_host(json: &str) -> (PluginHost, PluginRuntime) {
    let mut host = PluginHost::with_default_limits();
    let id = host.install_json(json).expect("install");
    let runtime = host.start(&id).expect("start");
    (host, runtime)
}

// ---------------------------------------------------------------- 清单/安装

#[test]
fn valid_manifest_installs_and_reports_capabilities() {
    let mut host = PluginHost::with_default_limits();
    let id = host.install_json(&notes_manifest()).unwrap();
    assert_eq!(id.as_str(), "notes.v1");
    assert!(host.is_installed(&id));
    let manifest = host.manifest(&id).expect("manifest");
    assert_eq!(manifest.version, PluginVersion::new(1, 0, 0));
    assert!(manifest.has_capability(PluginCapability::PrivateStorage));
    assert!(manifest.has_permission(PluginPermission::UserSelectedContent));
    assert!(manifest.permits_extension_point(ExtensionPoint::Command));
    assert_eq!(host.installed_plugins(), vec![id]);
}

#[test]
fn install_rejects_duplicates_and_invalid_manifests() {
    let mut host = PluginHost::with_default_limits();
    host.install_json(&notes_manifest()).unwrap();
    assert_eq!(
        host.install_json(&notes_manifest()).unwrap_err().code(),
        "PLUGIN_ID_CONFLICT"
    );
    // 非法 JSON / 未知扩展点 / 缺能力 / SDK 不兼容都显式拒绝，且不产生安装状态。
    for (json, code) in [
        ("{".to_owned(), "INVALID_PLUGIN_PACKAGE"),
        (
            manifest_json("x.one", &["Command"], &["Command", "NetworkHook"]),
            "UNKNOWN_EXTENSION_POINT",
        ),
        (
            manifest_json("x.two", &["PrivateStorage"], &["Command"]),
            "CAPABILITY_NOT_DECLARED",
        ),
        (
            manifest_json("x.three", &["Command"], &["Command"])
                .replace(r#""sdkVersion":1"#, r#""sdkVersion":9"#),
            "SDK_INCOMPATIBLE",
        ),
    ] {
        assert_eq!(host.install_json(&json).unwrap_err().code(), code);
    }
    assert_eq!(host.installed_plugins().len(), 1);
}

#[test]
fn custom_limits_are_enforced_on_install() {
    let mut limits = PluginLimits::DEFAULT;
    limits.max_manifest_bytes = 32;
    let mut host = PluginHost::new(limits).unwrap();
    assert_eq!(
        host.install_json(&notes_manifest()).unwrap_err().code(),
        "CONTENT_TOO_LARGE"
    );
    assert!(host.installed_plugins().is_empty());
    // 非法预算在创建宿主时拒绝。
    limits.max_input_bytes = 0;
    assert_eq!(
        PluginHost::new(limits).unwrap_err().code(),
        "INVALID_PLUGIN_LIMITS"
    );
}

// ---------------------------------------------------------------- 生命周期

#[test]
fn start_registers_points_and_repeated_start_is_idempotent() {
    let (host, runtime) = started_host(&notes_manifest());
    let id = runtime.plugin_id();
    assert_eq!(runtime.plugin_id().as_str(), "notes.v1");
    assert!(host.is_running(id));
    assert_eq!(host.registry().registration_count(), 1);
    assert_eq!(host.registry().extension_count(), 2);
    assert_eq!(
        host.registry().query(ExtensionPoint::Command),
        vec![id.clone()]
    );
    assert!(host.registry().supports(id, ExtensionPoint::MessageAction));
    // 重复启动不重复注册。
    let mut host = host;
    let again = host.start(id).unwrap();
    assert_eq!(again.plugin_id(), id);
    assert_eq!(host.registry().extension_count(), 2);
}

#[test]
fn start_and_stop_require_installed_plugin() {
    let mut host = PluginHost::with_default_limits();
    let missing = plugin("missing.v1");
    assert_eq!(
        host.start(&missing).unwrap_err().code(),
        "PLUGIN_NOT_INSTALLED"
    );
    assert_eq!(
        host.stop(&missing).unwrap_err().code(),
        "PLUGIN_NOT_INSTALLED"
    );
    let id = host.install_json(&notes_manifest()).unwrap();
    // 未启动时 stop 幂等返回 false，且卸载不存在插件也是幂等。
    assert!(!host.stop(&id).unwrap());
    assert!(!host.uninstall(&missing, false).unwrap());
}

#[test]
fn stop_revokes_registrations_and_stale_runtime_is_rejected() {
    let (mut host, runtime) = started_host(&notes_manifest());
    let id = runtime.plugin_id().clone();
    assert!(host.stop(&id).unwrap());
    assert!(!host.is_running(&id));
    assert!(host.registry().query(ExtensionPoint::Command).is_empty());
    // 旧句柄无法再分发或访问存储。
    assert_eq!(
        host.dispatch(
            &runtime,
            ExtensionPoint::Command,
            operation(1),
            b"args",
            1_000,
            2_000
        )
        .unwrap_err()
        .code(),
        "PLUGIN_NOT_RUNNING"
    );
    assert_eq!(
        host.data_put(&runtime, "k", b"v").unwrap_err().code(),
        "PLUGIN_NOT_RUNNING"
    );
    // 重新启动可再次分发。
    let runtime = host.start(&id).unwrap();
    assert!(
        host.dispatch(
            &runtime,
            ExtensionPoint::Command,
            operation(2),
            b"args",
            1_000,
            2_000
        )
        .is_ok()
    );
}

#[test]
fn uninstall_is_idempotent_and_can_keep_or_remove_data() {
    let mut host = PluginHost::with_default_limits();
    let id = host.install_json(&notes_manifest()).unwrap();
    let runtime = host.start(&id).unwrap();
    host.data_put(&runtime, "draft", b"secret").unwrap();
    assert!(host.uninstall(&id, false).unwrap());
    assert!(!host.is_installed(&id));
    assert!(!host.uninstall(&id, false).unwrap());
    // 重新安装同 ID 后数据按选择保留（remove_data=false）。
    let id = host.install_json(&notes_manifest()).unwrap();
    let runtime = host.start(&id).unwrap();
    assert_eq!(
        host.data_get(&runtime, "draft").unwrap(),
        Some(&b"secret"[..])
    );
    // remove_data=true 清空私有数据。
    assert!(host.uninstall(&id, true).unwrap());
    let id = host.install_json(&notes_manifest()).unwrap();
    let runtime = host.start(&id).unwrap();
    assert_eq!(host.data_get(&runtime, "draft").unwrap(), None);
}

// ---------------------------------------------------------------- 分发计划

#[test]
fn dispatch_returns_bounded_plan_and_rejects_unregistered_points() {
    let (mut host, runtime) = started_host(&notes_manifest());
    let plan = host
        .dispatch(
            &runtime,
            ExtensionPoint::Command,
            operation(1),
            b"args",
            1_000,
            2_000,
        )
        .unwrap();
    assert_eq!(plan.plugin_id().as_str(), "notes.v1");
    assert_eq!(plan.extension_point(), ExtensionPoint::Command);
    assert_eq!(plan.operation_id(), operation(1));
    assert_eq!(plan.input_bytes(), 4);
    assert_eq!(
        plan.output_limit_bytes(),
        PluginLimits::DEFAULT.max_output_bytes
    );
    assert_eq!(plan.deadline_unix_ms(), 2_000);
    assert_eq!(plan.timeout_ms(), 1_000);
    // 未注册该扩展点的插件不能分发（即使同一点有别的插件注册）。
    let other = manifest_json("other.v1", &["Command"], &["Command"]);
    let other_id = host.install_json(&other).unwrap();
    let other_runtime = host.start(&other_id).unwrap();
    assert_eq!(
        host.dispatch(
            &other_runtime,
            ExtensionPoint::MessageAction,
            operation(2),
            b"",
            1_000,
            2_000
        )
        .unwrap_err()
        .code(),
        "PLUGIN_NOT_REGISTERED"
    );
}

#[test]
fn input_and_output_sizes_are_bounded() {
    let mut limits = PluginLimits::DEFAULT;
    limits.max_input_bytes = 8;
    limits.max_output_bytes = 8;
    let mut host = PluginHost::new(limits).unwrap();
    let id = host.install_json(&notes_manifest()).unwrap();
    let runtime = host.start(&id).unwrap();
    // 恰好 8 字节输入允许。
    assert!(
        host.dispatch(
            &runtime,
            ExtensionPoint::Command,
            operation(1),
            b"12345678",
            0,
            100
        )
        .is_ok()
    );
    // 9 字节拒绝。
    assert_eq!(
        host.dispatch(
            &runtime,
            ExtensionPoint::Command,
            operation(2),
            b"123456789",
            0,
            100
        )
        .unwrap_err()
        .code(),
        "CONTENT_TOO_LARGE"
    );
    // 输出边界：8 字节允许，9 字节拒绝，且失败不记录结果。
    host.finish_dispatch(&runtime, operation(1), b"12345678", 50)
        .unwrap();
    assert_eq!(
        host.finish_dispatch(&runtime, operation(1), b"123456789", 50)
            .unwrap_err()
            .code(),
        "CONTENT_TOO_LARGE"
    );
}

#[test]
fn timeout_budget_is_expired_and_clamped() {
    let mut host = PluginHost::with_default_limits();
    let id = host.install_json(&notes_manifest()).unwrap();
    let runtime = host.start(&id).unwrap();
    // 截止时间已到 → 超时，不产生计划。
    assert_eq!(
        host.dispatch(
            &runtime,
            ExtensionPoint::Command,
            operation(1),
            b"",
            100,
            100
        )
        .unwrap_err()
        .code(),
        "PLUGIN_TASK_TIMEOUT"
    );
    // 超出宿主预算的截止时间被收紧到上限。
    let max = PluginLimits::DEFAULT.max_timeout_ms;
    let plan = host
        .dispatch(
            &runtime,
            ExtensionPoint::Command,
            operation(2),
            b"",
            1_000,
            1_000 + max * 3,
        )
        .unwrap();
    assert_eq!(plan.timeout_ms(), max);
    assert_eq!(plan.deadline_unix_ms(), 1_000 + max);
    // 完成校验：超过生效截止时间 → 超时。
    assert_eq!(
        host.finish_dispatch(&runtime, operation(2), b"", 1_000 + max + 1)
            .unwrap_err()
            .code(),
        "PLUGIN_TASK_TIMEOUT"
    );
    assert!(
        host.finish_dispatch(&runtime, operation(2), b"", 1_000 + max)
            .is_ok()
    );
}

#[test]
fn operation_id_replay_is_idempotent_and_conflicts_are_rejected() {
    let (mut host, runtime) = started_host(&notes_manifest());
    let first = host
        .dispatch(
            &runtime,
            ExtensionPoint::Command,
            operation(1),
            b"args",
            0,
            100,
        )
        .unwrap();
    let replay = host
        .dispatch(
            &runtime,
            ExtensionPoint::Command,
            operation(1),
            b"args",
            10,
            110,
        )
        .unwrap();
    assert_eq!(first, replay, "同一 operation 的相同请求必须幂等");
    // 同一 operation 绑定不同输入 → 冲突。
    assert_eq!(
        host.dispatch(
            &runtime,
            ExtensionPoint::Command,
            operation(1),
            b"other",
            0,
            100
        )
        .unwrap_err()
        .code(),
        "PLUGIN_OPERATION_CONFLICT"
    );
    // 同一 operation 绑定不同扩展点也冲突。
    assert_eq!(
        host.dispatch(
            &runtime,
            ExtensionPoint::MessageAction,
            operation(1),
            b"args",
            0,
            100
        )
        .unwrap_err()
        .code(),
        "PLUGIN_OPERATION_CONFLICT"
    );
    // 完成校验幂等：相同输出重复成功，不同输出冲突。
    host.finish_dispatch(&runtime, operation(1), b"ok", 50)
        .unwrap();
    host.finish_dispatch(&runtime, operation(1), b"ok", 60)
        .unwrap();
    assert_eq!(
        host.finish_dispatch(&runtime, operation(1), b"different", 60)
            .unwrap_err()
            .code(),
        "PLUGIN_OPERATION_CONFLICT"
    );
    // 未开始 / 已被清理的 operation。
    assert_eq!(
        host.finish_dispatch(&runtime, operation(9), b"", 10)
            .unwrap_err()
            .code(),
        "PLUGIN_OPERATION_UNKNOWN"
    );
}

#[test]
fn stop_clears_in_flight_operations() {
    let (mut host, runtime) = started_host(&notes_manifest());
    let id = runtime.plugin_id().clone();
    host.dispatch(&runtime, ExtensionPoint::Command, operation(1), b"", 0, 100)
        .unwrap();
    host.stop(&id).unwrap();
    let runtime = host.start(&id).unwrap();
    assert_eq!(
        host.finish_dispatch(&runtime, operation(1), b"", 10)
            .unwrap_err()
            .code(),
        "PLUGIN_OPERATION_UNKNOWN"
    );
    // 停止后重新分发同一 operationId 视为新请求。
    assert!(
        host.dispatch(
            &runtime,
            ExtensionPoint::Command,
            operation(1),
            b"",
            20,
            120
        )
        .is_ok()
    );
}

#[test]
fn a_plugin_cannot_finish_another_plugins_operation() {
    let mut host = PluginHost::with_default_limits();
    let first = host.install_json(&notes_manifest()).unwrap();
    let second = host
        .install_json(&manifest_json("other.v1", &["Command"], &["Command"]))
        .unwrap();
    let first_runtime = host.start(&first).unwrap();
    let second_runtime = host.start(&second).unwrap();
    host.dispatch(
        &first_runtime,
        ExtensionPoint::Command,
        operation(1),
        b"",
        0,
        100,
    )
    .unwrap();
    assert_eq!(
        host.finish_dispatch(&second_runtime, operation(1), b"", 10)
            .unwrap_err()
            .code(),
        "PLUGIN_OPERATION_UNKNOWN"
    );
}

// ---------------------------------------------------------------- 私有存储隔离

#[test]
fn storage_is_isolated_per_plugin() {
    let mut host = PluginHost::with_default_limits();
    let a = host.install_json(&notes_manifest()).unwrap();
    let b = host
        .install_json(&manifest_json(
            "other.v1",
            &["PrivateStorage", "Command"],
            &["Command"],
        ))
        .unwrap();
    let runtime_a = host.start(&a).unwrap();
    let runtime_b = host.start(&b).unwrap();
    host.data_put(&runtime_a, "shared", b"alpha").unwrap();
    host.data_put(&runtime_a, "a-only", b"1").unwrap();
    host.data_put(&runtime_b, "shared", b"beta").unwrap();
    assert_eq!(
        host.data_get(&runtime_a, "shared").unwrap(),
        Some(&b"alpha"[..])
    );
    assert_eq!(
        host.data_get(&runtime_b, "shared").unwrap(),
        Some(&b"beta"[..])
    );
    assert_eq!(host.data_get(&runtime_a, "b-only").unwrap(), None);
    assert_eq!(
        host.data_keys(&runtime_a).unwrap(),
        vec!["a-only".to_owned(), "shared".to_owned()]
    );
    assert_eq!(
        host.data_keys(&runtime_b).unwrap(),
        vec!["shared".to_owned()]
    );
    assert_eq!(host.data_usage(&runtime_a).unwrap().entries, 2);
    assert_eq!(host.data_usage(&runtime_b).unwrap().entries, 1);
    // 删除只影响自身命名空间。
    assert!(host.data_remove(&runtime_a, "shared").unwrap());
    assert_eq!(
        host.data_get(&runtime_b, "shared").unwrap(),
        Some(&b"beta"[..])
    );
}

#[test]
fn storage_requires_declared_capability() {
    let mut host = PluginHost::with_default_limits();
    // 只声明命令，不声明私有存储。
    let id = host
        .install_json(&manifest_json("cmds.v1", &["Command"], &["Command"]))
        .unwrap();
    let runtime = host.start(&id).unwrap();
    assert_eq!(
        host.data_put(&runtime, "k", b"v").unwrap_err().code(),
        "CAPABILITY_NOT_DECLARED"
    );
    assert_eq!(
        host.data_get(&runtime, "k").unwrap_err().code(),
        "CAPABILITY_NOT_DECLARED"
    );
    assert_eq!(
        host.data_keys(&runtime).unwrap_err().code(),
        "CAPABILITY_NOT_DECLARED"
    );
}

#[test]
fn storage_keys_values_and_quotas_are_enforced() {
    let mut limits = PluginLimits::DEFAULT;
    limits.max_storage_key_bytes = 8;
    limits.max_storage_value_bytes = 8;
    limits.max_storage_entries = 2;
    limits.max_storage_bytes = 32;
    let mut host = PluginHost::new(limits).unwrap();
    let id = host.install_json(&notes_manifest()).unwrap();
    let runtime = host.start(&id).unwrap();
    for key in ["", "has space", "trailing/", "a".repeat(9).as_str()] {
        assert_eq!(
            host.data_put(&runtime, key, b"v").unwrap_err().code(),
            "INVALID_STORAGE_KEY",
            "key {key:?}"
        );
    }
    assert_eq!(
        host.data_put(&runtime, "k", &[0u8; 9]).unwrap_err().code(),
        "CONTENT_TOO_LARGE"
    );
    host.data_put(&runtime, "k1", b"12345").unwrap();
    host.data_put(&runtime, "k2", b"12345").unwrap();
    assert_eq!(
        host.data_put(&runtime, "k3", b"12345").unwrap_err().code(),
        "PLUGIN_STORAGE_QUOTA_EXCEEDED"
    );
    assert_eq!(host.data_usage(&runtime).unwrap().entries, 2);
    // 覆盖已存在键不受条目配额影响。
    host.data_put(&runtime, "k1", b"1").unwrap();
    assert_eq!(host.data_get(&runtime, "k1").unwrap(), Some(&b"1"[..]));
}

// ---------------------------------------------------------------- 错误码登记

#[test]
fn error_codes_are_registered_and_not_retryable() {
    let error = PluginError::ContentTooLarge { size: 9, limit: 8 };
    assert_eq!(error.code(), "CONTENT_TOO_LARGE");
    assert_eq!(
        error.frozen_code(),
        Some(chat_protocol::ErrorCode::ContentTooLarge)
    );
    let error = PluginError::NotRegistered;
    assert_eq!(error.code(), "PLUGIN_NOT_REGISTERED");
    assert_eq!(error.frozen_code(), None);
    assert!(!error.retryable());
    // 码表唯一。
    let unique: std::collections::BTreeSet<&str> = CODE_TABLE.into_iter().collect();
    assert_eq!(unique.len(), CODE_TABLE.len());
}
