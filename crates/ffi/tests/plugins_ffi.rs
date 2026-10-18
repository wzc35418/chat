//! 插件宿主 FFI 用例（纯内存，不加载/不执行任何插件代码）。
//!
//! 覆盖：安装→启动→分发计划→完成校验→停止→卸载；重复安装冲突；未声明能力访问存储被拒；
//! 跨插件存储隔离；输入/输出超限拒绝；幂等重放与冲突；关闭后 CORE_CLOSED。

use chat_ffi::local_store::FfiError;
use chat_ffi::plugins_ffi::PluginsFfi;

const NOW_MS: u64 = 1_700_000_000_000;

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

fn operation(seed: u8) -> String {
    // PROTOCOL-001 operation 文本：op:1:<64 位 hex>。
    format!("op:1:{}", format!("{seed:02x}").repeat(32))
}

fn code(error: FfiError) -> String {
    match error {
        FfiError::Store { code, .. } => code,
    }
}

#[test]
fn install_start_dispatch_finish_stop_uninstall() {
    let host = PluginsFfi::new();
    let json = manifest("notes.v1", &["PrivateStorage", "Command"], &["Command"]);

    // 清单校验：只解析、不安装。
    let view = host.manifest_validate(json.clone()).expect("validate");
    assert_eq!(view.plugin_id, "notes.v1");
    assert_eq!(view.version, "1.0.0");
    assert_eq!(view.platform, "desktop");
    // BTreeSet 顺序（枚举序）：PrivateStorage 在 Command 之前。
    assert_eq!(view.capabilities, vec!["PrivateStorage", "Command"]);
    assert_eq!(view.extension_points, vec!["Command"]);
    assert!(host.list_plugins().expect("list").is_empty(), "validation must not install");

    // 安装 → 启动。
    let plugin_id = host.install(json).expect("install");
    assert_eq!(plugin_id, "notes.v1");
    let plugins = host.list_plugins().expect("list");
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0].plugin_id, "notes.v1");
    assert!(!plugins[0].running);
    host.start(plugin_id.clone()).expect("start");
    assert!(host.list_plugins().expect("list")[0].running);

    // 分发计划（不执行插件代码）：只有预算与绑定。
    let plan = host
        .dispatch(
            plugin_id.clone(),
            "Command".to_owned(),
            operation(0x11),
            b"input".to_vec(),
            NOW_MS,
            NOW_MS + 5_000,
        )
        .expect("dispatch plan");
    assert_eq!(plan.plugin_id, "notes.v1");
    assert_eq!(plan.extension_point, "Command");
    assert_eq!(plan.operation_id, operation(0x11));
    assert_eq!(plan.input_bytes, 5);
    assert!(plan.output_limit_bytes > 0);
    assert_eq!(plan.deadline_unix_ms, NOW_MS + 5_000);
    assert_eq!(plan.timeout_ms, 5_000);

    // 完成校验（大小/截止时间/幂等）。
    host.finish_dispatch(
        plugin_id.clone(),
        operation(0x11),
        b"output".to_vec(),
        NOW_MS + 100,
    )
    .expect("finish");
    // 同输出重放幂等。
    host.finish_dispatch(
        plugin_id.clone(),
        operation(0x11),
        b"output".to_vec(),
        NOW_MS + 200,
    )
    .expect("idempotent finish");

    // 停止 → 卸载。
    assert!(host.stop(plugin_id.clone()).expect("stop"));
    assert!(!host.list_plugins().expect("list")[0].running);
    let error = host
        .dispatch(
            plugin_id.clone(),
            "Command".to_owned(),
            operation(0x12),
            b"x".to_vec(),
            NOW_MS,
            NOW_MS + 1_000,
        )
        .expect_err("a stopped plugin cannot dispatch");
    assert_eq!(code(error), "PLUGIN_NOT_RUNNING");
    assert!(host.uninstall(plugin_id.clone(), true).expect("uninstall"));
    assert!(host.list_plugins().expect("list").is_empty());
}

#[test]
fn duplicate_install_conflicts() {
    let host = PluginsFfi::new();
    let json = manifest("notes.v1", &["Command"], &["Command"]);
    host.install(json.clone()).expect("install");
    let error = host.install(json).expect_err("duplicate install");
    assert_eq!(code(error), "PLUGIN_ID_CONFLICT");
}

#[test]
fn storage_requires_the_declared_capability() {
    let host = PluginsFfi::new();
    // 没声明 PrivateStorage 的插件访问存储 → 拒绝。
    let json = manifest("nosy.v1", &["Command"], &["Command"]);
    let plugin_id = host.install(json).expect("install");
    host.start(plugin_id.clone()).expect("start");
    let error = host
        .storage_set(plugin_id.clone(), "k".to_owned(), b"v".to_vec())
        .expect_err("capability not declared");
    assert_eq!(code(error), "CAPABILITY_NOT_DECLARED");
    let error = host
        .storage_get(plugin_id.clone(), "k".to_owned())
        .expect_err("capability not declared");
    assert_eq!(code(error), "CAPABILITY_NOT_DECLARED");
}

#[test]
fn storage_is_isolated_between_plugins() {
    let host = PluginsFfi::new();
    // 注册 Command 扩展点需要 Command 能力，所以两个能力都要声明。
    let first = host
        .install(manifest("first.v1", &["PrivateStorage", "Command"], &["Command"]))
        .expect("install first");
    let second = host
        .install(manifest("second.v1", &["PrivateStorage", "Command"], &["Command"]))
        .expect("install second");
    host.start(first.clone()).expect("start first");
    host.start(second.clone()).expect("start second");

    host.storage_set(first.clone(), "shared-key".to_owned(), b"first-value".to_vec())
        .expect("first writes");
    assert_eq!(
        host.storage_get(first.clone(), "shared-key".to_owned())
            .expect("first reads"),
        Some(b"first-value".to_vec())
    );
    // 另一个插件看不到同名键（命名空间隔离）。
    assert_eq!(
        host.storage_get(second.clone(), "shared-key".to_owned())
            .expect("second reads"),
        None
    );
    host.storage_set(second.clone(), "shared-key".to_owned(), b"second-value".to_vec())
        .expect("second writes");
    assert_eq!(
        host.storage_get(first.clone(), "shared-key".to_owned())
            .expect("first still reads its own"),
        Some(b"first-value".to_vec())
    );
    // 卸载（保留数据）后另一个插件的值仍在。
    host.uninstall(first, false).expect("uninstall first");
    assert_eq!(
        host.storage_get(second, "shared-key".to_owned())
            .expect("second still reads"),
        Some(b"second-value".to_vec())
    );
}

#[test]
fn oversized_input_and_output_are_rejected() {
    let host = PluginsFfi::new();
    let plugin_id = host
        .install(manifest("big.v1", &["Command"], &["Command"]))
        .expect("install");
    host.start(plugin_id.clone()).expect("start");

    // 输入超限（默认 64 KiB）。
    let too_big = vec![0x41_u8; 64 * 1024 + 1];
    let error = host
        .dispatch(
            plugin_id.clone(),
            "Command".to_owned(),
            operation(0x21),
            too_big,
            NOW_MS,
            NOW_MS + 1_000,
        )
        .expect_err("input too large");
    assert_eq!(code(error), "CONTENT_TOO_LARGE");

    // 输出超限。
    host.dispatch(
        plugin_id.clone(),
        "Command".to_owned(),
        operation(0x22),
        b"ok".to_vec(),
        NOW_MS,
        NOW_MS + 1_000,
    )
    .expect("plan");
    let output_too_big = vec![0x42_u8; 64 * 1024 + 1];
    let error = host
        .finish_dispatch(plugin_id, operation(0x22), output_too_big, NOW_MS + 10)
        .expect_err("output too large");
    assert_eq!(code(error), "CONTENT_TOO_LARGE");
}

#[test]
fn dispatch_is_idempotent_and_conflicts_are_explicit() {
    let host = PluginsFfi::new();
    let plugin_id = host
        .install(manifest("idem.v1", &["Command"], &["Command"]))
        .expect("install");
    host.start(plugin_id.clone()).expect("start");

    let first = host
        .dispatch(
            plugin_id.clone(),
            "Command".to_owned(),
            operation(0x31),
            b"same".to_vec(),
            NOW_MS,
            NOW_MS + 1_000,
        )
        .expect("plan");
    // 同 operation 同输入 → 同一计划（幂等重放）。
    let replay = host
        .dispatch(
            plugin_id.clone(),
            "Command".to_owned(),
            operation(0x31),
            b"same".to_vec(),
            NOW_MS + 50,
            NOW_MS + 900,
        )
        .expect("replay");
    assert_eq!(replay.operation_id, first.operation_id);
    assert_eq!(replay.deadline_unix_ms, first.deadline_unix_ms);
    // 同 operation 不同输入 → 冲突。
    let error = host
        .dispatch(
            plugin_id.clone(),
            "Command".to_owned(),
            operation(0x31),
            b"other".to_vec(),
            NOW_MS,
            NOW_MS + 1_000,
        )
        .expect_err("operation conflict");
    assert_eq!(code(error), "PLUGIN_OPERATION_CONFLICT");
    // 未知 operation 的完成回报 → UNKNOWN。
    let error = host
        .finish_dispatch(plugin_id.clone(), operation(0x3f), b"out".to_vec(), NOW_MS)
        .expect_err("unknown operation");
    assert_eq!(code(error), "PLUGIN_OPERATION_UNKNOWN");
    // 未知扩展点 / 未注册扩展点。
    let error = host
        .dispatch(
            plugin_id.clone(),
            "SettingsPanel".to_owned(),
            operation(0x32),
            b"x".to_vec(),
            NOW_MS,
            NOW_MS + 1_000,
        )
        .expect_err("not registered");
    assert_eq!(code(error), "PLUGIN_NOT_REGISTERED");
    let error = host
        .dispatch(
            plugin_id.clone(),
            "Nope".to_owned(),
            operation(0x33),
            b"x".to_vec(),
            NOW_MS,
            NOW_MS + 1_000,
        )
        .expect_err("unknown extension point");
    assert_eq!(code(error), "UNKNOWN_EXTENSION_POINT");
}

#[test]
fn shutdown_is_idempotent_and_closes_the_object() {
    let host = PluginsFfi::new();
    let plugin_id = host
        .install(manifest("bye.v1", &["Command"], &["Command"]))
        .expect("install");
    host.start(plugin_id).expect("start");
    host.shutdown();
    assert!(!host.is_open());
    host.shutdown();
    assert!(!host.is_open());
    assert_eq!(
        code(host.list_plugins().expect_err("closed")),
        "CORE_CLOSED"
    );
    assert_eq!(
        code(
            host
                .manifest_validate(manifest("late.v1", &["Command"], &["Command"]))
                .expect_err("closed")
        ),
        "CORE_CLOSED"
    );
}
