//! 插件宿主纯逻辑层（PLUGINS 模块的最小闭环）。
//!
//! 本 crate 不依赖 WASM/JVM 运行时、不联网、无 `unsafe`，只提供可独立验证的规则：
//!
//! - [`manifest`]：PLUGINS-001 的清单 schema 解析/校验（ID、版本、SDK/平台、能力、
//!   扩展点、权限、入口类名）；重复 ID 由宿主安装时拒绝；
//! - [`registry`]：PLUGINS-002/003 的扩展点注册/注销/查询（重复注册幂等，冲突显式拒绝）；
//! - [`host`]：安装→启动→分发计划→完成校验→停止/卸载的闭环，含输入/输出上限、
//!   超时预算与 operationId 幂等；插件只能通过宿主签发的 [`PluginRuntime`] 访问自身
//!   扩展点与私有存储；
//! - [`storage`]：PLUGINS-004 的每插件命名空间键值隔离（键/值/条目/字节配额）；
//! - [`limits`]：全部资源预算（可收紧、不可放大）；
//! - [`error`]：插件层稳定错误码（与 PROTOCOL-014 的关系见模块文档）。
//!
//! 真正的 JAR 加载、信任确认、安装事务、主题配置等 P4 宿主能力**未实现**。
#![forbid(unsafe_code)]

pub mod error;
pub mod host;
pub mod install;
pub mod limits;
pub mod manifest;
pub mod package;
pub mod registry;
pub mod storage;

pub use error::{CODE_TABLE, PluginError, Result};
pub use host::{DispatchPlan, PluginHost, PluginRuntime};
pub use install::{InstallOutcome, InstallTransaction};
pub use limits::{HARD_MAX_COUNT, HARD_MAX_LIMIT_BYTES, HARD_MAX_TIMEOUT_MS, PluginLimits};
pub use manifest::{
    CURRENT_MANIFEST_SCHEMA, CURRENT_SDK_VERSION, ExtensionPoint, MAX_ENTRY_CLASS_BYTES,
    MAX_PERMISSIONS, MAX_PLUGIN_ID_BYTES, PluginCapability, PluginId, PluginManifest,
    PluginPermission, PluginPlatform, PluginVersion, parse_manifest,
};
pub use package::{
    MANIFEST_FILE, MAX_ENTRY_COUNT, MAX_ENTRY_NAME_BYTES, MAX_PACKAGE_BYTES,
    MAX_UNCOMPRESSED_BYTES, SPI_DIRECTORY, PackageEntryView, PackageInspection,
    inspect_plugin_package, list_package_entries, package_manifest_json,
};
pub use registry::{ExtensionPointRegistry, RegistrationOutcome};
pub use storage::{PluginStorage, StorageUsage};
