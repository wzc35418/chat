//! PLUGINS：插件安装事务用例（测试内构造最小 ZIP，不执行任何插件代码）。
//!
//! 覆盖：正常安装、升级（from/to）、同版本冲突、降级拒绝、commit 前 rollback 后注册表为空、
//! 包校验失败不进事务、清单与 SPI 不一致拒绝、事务期间失败不留半状态。

use chat_plugins::{
    InstallOutcome, InstallTransaction, PluginHost, PluginId, PluginLimits, PluginVersion,
};

const ENTRY_CLASS: &str = "com.example.Plugin";
const SPI_FILE: &str = "META-INF/services/com.example.Plugin";
const CLASS_FILE: &str = "com/example/Plugin.class";

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

fn limits() -> PluginLimits {
    PluginLimits::default()
}

fn plugin_id() -> PluginId {
    PluginId::parse("notes.v1").expect("plugin id")
}
#[test]
fn a_valid_package_is_installed_atomically() {
    let mut host = PluginHost::with_default_limits();
    let mut transaction = InstallTransaction::begin(&package("1.0.0"), limits()).expect("begin");
    assert!(!transaction.is_staged());
    assert_eq!(transaction.inspection().entry_count, 3);
    assert_eq!(transaction.manifest().id, plugin_id());
    transaction.stage().expect("stage");
    assert!(transaction.is_staged());
    // 暂存阶段宿主仍然干净。
    assert!(!host.is_installed(&plugin_id()));

    let outcome = transaction.commit(&mut host).expect("commit");
    assert_eq!(
        outcome,
        InstallOutcome::Installed {
            plugin_id: plugin_id(),
            version: PluginVersion::new(1, 0, 0),
        }
    );
    assert!(transaction.is_finished());
    assert!(host.is_installed(&plugin_id()));
    assert_eq!(
        host.manifest(&plugin_id()).expect("manifest").version,
        PluginVersion::new(1, 0, 0)
    );
    // 事务已结束：再提交/再暂存都显式失败（不会二次写入）。
    assert!(transaction.commit(&mut host).is_err());
}

#[test]
fn a_higher_version_upgrades_and_reports_from_and_to() {
    let mut host = PluginHost::with_default_limits();
    let mut first = InstallTransaction::begin(&package("1.0.0"), limits()).expect("begin v1");
    first.stage().expect("stage v1");
    first.commit(&mut host).expect("install v1");

    let mut second = InstallTransaction::begin(&package("1.4.2"), limits()).expect("begin v2");
    second.stage().expect("stage v2");
    let outcome = second.commit(&mut host).expect("upgrade");
    assert_eq!(
        outcome,
        InstallOutcome::Upgraded {
            plugin_id: plugin_id(),
            from: PluginVersion::new(1, 0, 0),
            to: PluginVersion::new(1, 4, 2),
        }
    );
    assert_eq!(
        host.manifest(&plugin_id()).expect("manifest").version,
        PluginVersion::new(1, 4, 2)
    );
}

#[test]
fn the_same_version_is_reported_as_a_state_conflict() {
    let mut host = PluginHost::with_default_limits();
    let mut first = InstallTransaction::begin(&package("1.0.0"), limits()).expect("begin");
    first.stage().expect("stage");
    first.commit(&mut host).expect("install");

    let mut again = InstallTransaction::begin(&package("1.0.0"), limits()).expect("begin again");
    again.stage().expect("stage again");
    let error = again.commit(&mut host).expect_err("same version conflicts");
    assert_eq!(error.code(), "PLUGIN_STATE_CONFLICT");
    // 宿主状态不变。
    assert_eq!(
        host.manifest(&plugin_id()).expect("manifest").version,
        PluginVersion::new(1, 0, 0)
    );
}

#[test]
fn a_lower_version_is_rejected_with_a_dedicated_code() {
    let mut host = PluginHost::with_default_limits();
    let mut first = InstallTransaction::begin(&package("2.0.0"), limits()).expect("begin v2");
    first.stage().expect("stage v2");
    first.commit(&mut host).expect("install v2");

    let mut older = InstallTransaction::begin(&package("1.9.9"), limits()).expect("begin v1");
    older.stage().expect("stage v1");
    let error = older.commit(&mut host).expect_err("downgrade is rejected");
    assert_eq!(error.code(), "PLUGIN_VERSION_DOWNGRADE");
    assert_eq!(
        host.manifest(&plugin_id()).expect("manifest").version,
        PluginVersion::new(2, 0, 0),
        "a rejected downgrade must not touch the installed version"
    );
}

#[test]
fn rolling_back_before_commit_leaves_the_registry_empty() {
    let mut host = PluginHost::with_default_limits();
    let mut transaction = InstallTransaction::begin(&package("1.0.0"), limits()).expect("begin");
    transaction.stage().expect("stage");
    transaction.rollback();
    assert!(transaction.is_finished());
    // 回滚后不能再提交（不会偷偷装进去）。
    assert!(transaction.commit(&mut host).is_err());
    assert!(!host.is_installed(&plugin_id()));
    assert!(host.installed_plugins().is_empty());
    // 注册表也没有任何注册项。
    assert!(!host.registry().supports(&plugin_id(), chat_plugins::ExtensionPoint::Command));
}

#[test]
fn a_broken_package_never_enters_the_transaction() {
    // 缺清单。
    let missing = zip(&[(CLASS_FILE, b"x"), (SPI_FILE, ENTRY_CLASS.as_bytes())]);
    let error = InstallTransaction::begin(&missing, limits()).expect_err("missing manifest");
    assert_eq!(error.code(), "PLUGIN_PACKAGE_MISSING_MANIFEST");
    // 不是 ZIP。
    let error = InstallTransaction::begin(b"not a zip", limits()).expect_err("corrupted");
    assert_eq!(error.code(), "PLUGIN_PACKAGE_CORRUPTED");
    // 清单与 SPI 不一致。
    let mismatch = zip(&[
        ("manifest.json", manifest_json("1.0.0").as_bytes()),
        (CLASS_FILE, b"x"),
        (SPI_FILE, b"com.example.Other"),
    ]);
    let error = InstallTransaction::begin(&mismatch, limits()).expect_err("spi mismatch");
    assert_eq!(error.code(), "PLUGIN_PACKAGE_SPI_MISMATCH");
}

#[test]
fn staging_is_required_before_commit() {
    let mut host = PluginHost::with_default_limits();
    let mut transaction = InstallTransaction::begin(&package("1.0.0"), limits()).expect("begin");
    let error = transaction.commit(&mut host).expect_err("commit without stage");
    assert_eq!(error.code(), "PLUGIN_PACKAGE_CORRUPTED");
    assert!(!host.is_installed(&plugin_id()));
}

#[test]
fn upgrading_a_running_plugin_keeps_it_running() {
    let mut host = PluginHost::with_default_limits();
    let mut first = InstallTransaction::begin(&package("1.0.0"), limits()).expect("begin v1");
    first.stage().expect("stage v1");
    first.commit(&mut host).expect("install v1");
    host.start(&plugin_id()).expect("start v1");
    assert!(host.is_running(&plugin_id()));

    let mut second = InstallTransaction::begin(&package("2.0.0"), limits()).expect("begin v2");
    second.stage().expect("stage v2");
    second.commit(&mut host).expect("upgrade to v2");
    assert!(host.is_running(&plugin_id()), "the upgrade re-registers and restarts");
    assert!(host.registry().supports(&plugin_id(), chat_plugins::ExtensionPoint::Command));
    assert_eq!(
        host.manifest(&plugin_id()).expect("manifest").version,
        PluginVersion::new(2, 0, 0)
    );
}

