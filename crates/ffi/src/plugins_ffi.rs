//! 插件宿主（PLUGINS）的 UniFFI 门面：**纯内存**、不加载/不执行任何插件代码。
//!
//! 边界（与 docs/implementation/plugins.md 的记录一致）：
//!
//! - 本对象只做清单校验、安装/启动/停止/卸载、分发**计划**（`DispatchPlanView`）、完成校验、
//!   私有存储读写与列表；真正的 JAR 加载、WASM/JVM 运行时、信任确认、安装事务都**未实现**；
//! - `dispatch` 只产出计划（输入字节数/输出上限/截止时间/超时预算），执行插件代码由宿主
//!   后续阶段负责；`finish_dispatch` 只校验输出大小/截止时间与幂等；
//! - 错误码原样透出 `chat-plugins` 的稳定码（`FfiError::Store { code }`）：`PLUGIN_ID_CONFLICT` /
//!   `PLUGIN_NOT_INSTALLED` / `PLUGIN_NOT_RUNNING` / `PLUGIN_NOT_REGISTERED` /
//!   `CAPABILITY_NOT_DECLARED` / `CONTENT_TOO_LARGE` / `PLUGIN_OPERATION_CONFLICT` /
//!   `PLUGIN_OPERATION_UNKNOWN` / `PLUGIN_TASK_TIMEOUT` / `INVALID_STORAGE_KEY` /
//!   `PLUGIN_STORAGE_QUOTA_EXCEEDED` / `INVALID_PLUGIN_PACKAGE` / `SDK_INCOMPATIBLE` …
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use chat_plugins::{
    DispatchPlan, ExtensionPoint, PluginHost, PluginId, PluginLimits, PluginManifest,
    PluginRuntime, PluginVersion, parse_manifest,
};
use chat_protocol::OperationId;
use chat_storage::formal::{
    KeyProvenance, ProtectedStorageKey, StoreOptions, open_store,
};

use crate::local_store::FfiError;

fn failure(error: chat_plugins::PluginError) -> FfiError {
    FfiError::Store {
        code: error.code().to_owned(),
        reason: error.to_string(),
    }
}

fn invalid_input(reason: &str) -> FfiError {
    FfiError::Store {
        code: "INVALID_INPUT".to_owned(),
        reason: reason.to_owned(),
    }
}

fn core_closed() -> FfiError {
    FfiError::Store {
        code: "CORE_CLOSED".to_owned(),
        reason: "the plugin host object is closed".to_owned(),
    }
}

/// 主密钥（64 位十六进制 → 32 字节）；错误密钥绝不被"修正"。
fn decode_master_key(text: &str) -> Result<[u8; 32], FfiError> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return Err(invalid_input(
            "master key must be exactly 64 hexadecimal characters",
        ));
    }
    let mut key = [0_u8; 32];
    for (index, slot) in key.iter_mut().enumerate() {
        let high = decode_nibble(bytes[index * 2])?;
        let low = decode_nibble(bytes[index * 2 + 1])?;
        *slot = (high << 4) | low;
    }
    Ok(key)
}

fn decode_nibble(byte: u8) -> Result<u8, FfiError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(invalid_input("master key must be hexadecimal")),
    }
}

fn poisoned() -> FfiError {
    FfiError::Store {
        code: "INTERNAL_FAILURE".to_owned(),
        reason: "the plugin host state is poisoned".to_owned(),
    }
}

/// 清单校验结果（纯数据）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ManifestViewFfi {
    /// 规范插件 ID。
    pub plugin_id: String,
    /// 插件版本（`MAJOR.MINOR.PATCH`）。
    pub version: String,
    /// 目标 SDK 版本。
    pub sdk_version: u16,
    /// 目标平台（`desktop`）。
    pub platform: String,
    /// 入口类名（不含路径）。
    pub entry_class: String,
    /// 声明能力（`PrivateStorage` / `Command` / `MessageAction` / `SettingsPanel`）。
    pub capabilities: Vec<String>,
    /// 声明权限。
    pub permissions: Vec<String>,
    /// 可注册扩展点。
    pub extension_points: Vec<String>,
}

/// 分发计划（不含插件代码，只含预算与绑定）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DispatchPlanViewFfi {
    /// 目标插件。
    pub plugin_id: String,
    /// 扩展点。
    pub extension_point: String,
    /// 幂等键（PROTOCOL-001 operation 文本）。
    pub operation_id: String,
    /// 输入字节数。
    pub input_bytes: u64,
    /// 输出上限（字节）。
    pub output_limit_bytes: u64,
    /// 生效截止时间（Unix 毫秒）。
    pub deadline_unix_ms: u64,
    /// 生效超时预算（毫秒）。
    pub timeout_ms: u64,
}

/// 安装事务的结果（纯数据）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct InstallOutcomeFfi {
    /// 插件 ID。
    pub plugin_id: String,
    /// 装上的版本（MAJOR.MINOR.PATCH）。
    pub version: String,
    /// 升级时是旧版本；首次安装为 None。
    pub upgraded_from: Option<String>,
}

/// 插件包结构校验结果（PLUGINS：不执行任何插件代码）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PackageViewFfi {
    /// 清单插件 ID。
    pub plugin_id: String,
    /// 清单版本（MAJOR.MINOR.PATCH）。
    pub plugin_version: String,
    /// 入口类名。
    pub entry_class: String,
    /// 入口类的 .class 条目路径。
    pub entry_file: String,
    /// SPI 描述符路径（META-INF/services/&lt;entryClass&gt;）。
    pub spi_file: String,
    /// 条目总数。
    pub entry_count: u32,
    /// 压缩字节合计。
    pub compressed_bytes: u64,
    /// 解压字节合计。
    pub uncompressed_bytes: u64,
    /// 清单声明的权限（纯数据；空 = 无敏感权限）。
    pub permissions: Vec<String>,
}

/// 已安装插件（纯数据）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PluginInfoFfi {
    /// 插件 ID。
    pub plugin_id: String,
    /// 是否正在运行。
    pub running: bool,
    /// 可注册扩展点。
    pub extension_points: Vec<String>,
    /// 已安装版本（`MAJOR.MINOR.PATCH`）。
    pub version: String,
    /// 已安装清单的公开投影（重开对象后同样可见）。
    pub manifest: ManifestViewFfi,
}

/// 清单缺失时的占位投影（正常路径不会出现：host 的已装列表明细一致）。
fn empty_manifest_view() -> ManifestViewFfi {
    ManifestViewFfi {
        plugin_id: String::new(),
        version: String::new(),
        sdk_version: 0,
        platform: String::new(),
        entry_class: String::new(),
        capabilities: Vec::new(),
        permissions: Vec::new(),
        extension_points: Vec::new(),
    }
}

/// 版本文本（MAJOR.MINOR.PATCH）。
fn version_text(version: PluginVersion) -> String {
    format!("{}.{}.{}", version.major, version.minor, version.patch)
}

fn manifest_view(manifest: &PluginManifest) -> ManifestViewFfi {
    ManifestViewFfi {
        plugin_id: manifest.id.as_str().to_owned(),
        version: format!(
            "{}.{}.{}",
            manifest.version.major, manifest.version.minor, manifest.version.patch
        ),
        sdk_version: manifest.sdk_version,
        platform: manifest.platform.as_str().to_owned(),
        entry_class: manifest.entry_class.clone(),
        capabilities: manifest
            .capabilities
            .iter()
            .map(|capability| capability.as_str().to_owned())
            .collect(),
        permissions: manifest
            .permissions
            .iter()
            .map(|permission| permission.as_str().to_owned())
            .collect(),
        extension_points: manifest
            .extension_points
            .iter()
            .map(|point| point.as_str().to_owned())
            .collect(),
    }
}

fn plan_view(plan: &DispatchPlan) -> DispatchPlanViewFfi {
    DispatchPlanViewFfi {
        plugin_id: plan.plugin_id().as_str().to_owned(),
        extension_point: plan.extension_point().as_str().to_owned(),
        operation_id: plan.operation_id().encode(),
        input_bytes: plan.input_bytes() as u64,
        output_limit_bytes: plan.output_limit_bytes() as u64,
        deadline_unix_ms: plan.deadline_unix_ms(),
        timeout_ms: plan.timeout_ms(),
    }
}

/// 插件在库里的状态文本。
const STATE_INSTALLED: &str = "installed";
const STATE_RUNNING: &str = "running";
const STATE_STOPPED: &str = "stopped";
/// 保留数据的卸载：行留着但标记为未安装（KV 保留，重装后仍可读）。
const STATE_UNINSTALLED: &str = "uninstalled";

struct Inner {
    host: PluginHost,
    limits: PluginLimits,
    runtimes: BTreeMap<String, PluginRuntime>,
    closed: bool,
    /// 持久化模式下的正式库（`None` = 纯内存模式）。
    store: Option<chat_storage::formal::Store>,
    /// 打开时因清单损坏被跳过的行数（持久化模式；不静默丢弃）。
    skipped_on_load: u32,
}

fn storage_failure(error: chat_storage::formal::StorageError) -> FfiError {
    let code = if error.to_string().contains("byte limit")
        || error.to_string().contains("entry limit")
    {
        "PLUGIN_STORAGE_QUOTA_EXCEEDED"
    } else {
        error.code()
    };
    FfiError::Store {
        code: code.to_owned(),
        reason: error.to_string(),
    }
}

/// 插件宿主 FFI 对象（纯内存：每个实例一个独立的宿主状态）。
#[derive(uniffi::Object)]
pub struct PluginsFfi {
    inner: Mutex<Inner>,
}

impl PluginsFfi {
    fn lock(&self) -> Result<MutexGuard<'_, Inner>, FfiError> {
        self.inner.lock().map_err(|_| poisoned())
    }

    fn plugin_id(text: &str) -> Result<PluginId, FfiError> {
        PluginId::parse(text).map_err(failure)
    }

    fn extension_point(text: &str) -> Result<ExtensionPoint, FfiError> {
        ExtensionPoint::parse(text).ok_or_else(|| {
            FfiError::Store {
                code: "UNKNOWN_EXTENSION_POINT".to_owned(),
                reason: "unknown extension point".to_owned(),
            }
        })
    }

    fn operation_id(text: &str) -> Result<OperationId, FfiError> {
        OperationId::parse(text).map_err(|_| invalid_input("operation id must be PROTOCOL-001 operation text"))
    }

    /// 持久化模式的能力门：插件必须已安装且声明了 PrivateStorage。
    fn require_private_storage(inner: &Inner, plugin: &PluginId) -> Result<(), FfiError> {
        let manifest = inner.host.manifest(plugin).ok_or_else(|| FfiError::Store {
            code: "PLUGIN_NOT_INSTALLED".to_owned(),
            reason: "plugin is not installed".to_owned(),
        })?;
        if !manifest.has_capability(chat_plugins::PluginCapability::PrivateStorage) {
            return Err(FfiError::Store {
                code: "CAPABILITY_NOT_DECLARED".to_owned(),
                reason: "required capability is not declared".to_owned(),
            });
        }
        Ok(())
    }

    /// 取已启动插件的运行时句柄（未启动 → `PLUGIN_NOT_RUNNING`）。
    fn runtime<'a>(
        inner: &'a Inner,
        plugin: &PluginId,
    ) -> Result<&'a PluginRuntime, FfiError> {
        inner.runtimes.get(plugin.as_str()).ok_or_else(|| FfiError::Store {
            code: "PLUGIN_NOT_RUNNING".to_owned(),
            reason: "plugin is not running".to_owned(),
        })
    }
}

#[uniffi::export]
impl PluginsFfi {
    /// 构造一个空的宿主（纯内存，可用限额由 chat-plugins 的默认预算决定）。
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        let host = PluginHost::with_default_limits();
        let limits = host.limits();
        Arc::new(Self {
            inner: Mutex::new(Inner {
                host,
                limits,
                runtimes: BTreeMap::new(),
                closed: false,
                store: None,
                skipped_on_load: 0,
            }),
        })
    }

    /// 打开**持久化模式**：把已安装清单与插件私有 KV 落到正式加密库。
    ///
    /// - 库必须已存在且主密钥正确（storage 冻结码）；
    /// - 库里已安装的插件会被重新装回宿主（**运行状态不恢复**：重开后在 @stopped@ 状态，
    ///   需要上层再 @start@）；
    /// - 清单损坏的行会被跳过并计数（@skipped`），绝不静默丢弃。
    #[uniffi::constructor]
    pub fn open(database_path: String, master_key_hex: String) -> Result<Arc<Self>, FfiError> {
        let store = open_store(
            StoreOptions::new(&database_path, KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(decode_master_key(&master_key_hex)?),
        )
        .map_err(storage_failure)?;
        let host = PluginHost::with_default_limits();
        let limits = host.limits();
        let mut inner = Inner {
            host,
            limits,
            runtimes: BTreeMap::new(),
            closed: false,
            store: None,
            skipped_on_load: 0,
        };
        let rows = store.load_plugins().map_err(storage_failure)?;
        for row in rows {
            if row.state == STATE_UNINSTALLED {
                // 保留数据的卸载：行留着（KV 保留），但不装回宿主。
                continue;
            }
            match parse_manifest(&row.manifest_json, &inner.limits) {
                Ok(manifest) => {
                    if inner.host.install(manifest).is_err() {
                        inner.skipped_on_load = inner.skipped_on_load.saturating_add(1);
                    }
                }
                Err(_) => inner.skipped_on_load = inner.skipped_on_load.saturating_add(1),
            }
        }
        inner.store = Some(store);
        Ok(Arc::new(Self {
            inner: Mutex::new(inner),
        }))
    }

    /// 打开时被跳过的损坏清单行数（纯内存模式恒为 0）。
    pub fn skipped_on_load(&self) -> Result<u32, FfiError> {
        let inner = self.lock()?;
        if inner.closed {
            return Err(core_closed());
        }
        Ok(inner.skipped_on_load)
    }

    /// PLUGINS：校验插件包（ZIP/JAR）结构 —— **不执行任何插件代码**。
    ///
    /// 校验：ZIP 结构（central directory）、条目名（无穿越/无重名）、规模上限
    /// （包 8 MiB / 解压 64 MiB / 条目 4096）、`manifest.json` 能过清单校验、
    /// 入口类的 `.class` 存在、SPI 描述符存在且实现名与 entryClass 一致。
    ///
    /// 清单与 SPI 必须是 **stored（method 0）** 条目：本层不做 inflate（见 plugins.md 限制）。
    pub fn inspect_package(&self, bytes: Vec<u8>) -> Result<PackageViewFfi, FfiError> {
        let inner = self.lock()?;
        if inner.closed {
            return Err(core_closed());
        }
        let view = chat_plugins::inspect_plugin_package(&bytes, &inner.limits).map_err(failure)?;
        Ok(PackageViewFfi {
            plugin_id: view.plugin_id,
            plugin_version: view.plugin_version,
            entry_class: view.entry_class,
            entry_file: view.entry_file,
            spi_file: view.spi_file,
            entry_count: view.entry_count,
            compressed_bytes: view.compressed_bytes,
            uncompressed_bytes: view.uncompressed_bytes,
            permissions: view.permissions,
        })
    }

    /// 校验清单 JSON（不安装、不执行）。
    pub fn manifest_validate(&self, manifest_json: String) -> Result<ManifestViewFfi, FfiError> {
        let inner = self.lock()?;
        if inner.closed {
            return Err(core_closed());
        }
        let manifest = parse_manifest(&manifest_json, &inner.limits).map_err(failure)?;
        Ok(manifest_view(&manifest))
    }

    /// 安装清单（重复 ID → `PLUGIN_ID_CONFLICT`）。
    pub fn install(&self, manifest_json: String) -> Result<String, FfiError> {
        let mut inner = self.lock()?;
        if inner.closed {
            return Err(core_closed());
        }
        let manifest = parse_manifest(&manifest_json, &inner.limits).map_err(failure)?;
        let plugin_id = manifest.id.as_str().to_owned();
        inner.host.install(manifest.clone()).map_err(failure)?;
        if let Some(store) = inner.store.as_mut() {
            store
                .upsert_plugin(&plugin_id, &manifest_json, STATE_INSTALLED)
                .map_err(storage_failure)?;
        }
        Ok(plugin_id)
    }

    /// PLUGINS：安装插件包（持久化模式）——校验 + 落库 + 内存宿主登记，**原子**。
    ///
    /// 顺序与回滚：
    /// 1. `InstallTransaction::begin` 做包结构/清单/SPI 校验（失败零写入）；
    /// 2. 未信任（`trusted == false`）→ `PLUGIN_TRUST_REQUIRED`（库与宿主都不变）；
    /// 3. 版本判定（读库里已装行）：同版本 → `PLUGIN_STATE_CONFLICT`、更低 → `PLUGIN_VERSION_DOWNGRADE`；
    /// 4. **一次 storage 事务**写 0010 `plugins` 行（`upsert_plugin`）；
    /// 5. 再提交到内存宿主；宿主提交失败 → 把库恢复成提交前那一行（升级）或删掉新行（首次安装）。
    pub fn install_package(
        &self,
        bytes: Vec<u8>,
        trusted: bool,
    ) -> Result<InstallOutcomeFfi, FfiError> {
        let mut inner = self.lock()?;
        if inner.closed {
            return Err(core_closed());
        }
        if !trusted {
            return Err(FfiError::Store {
                code: "PLUGIN_TRUST_REQUIRED".to_owned(),
                reason: "the user has not trusted this plugin package".to_owned(),
            });
        }
        let limits = inner.limits;
        let mut transaction =
            chat_plugins::InstallTransaction::begin(&bytes, limits).map_err(failure)?;
        let manifest_json = chat_plugins::package_manifest_json(&bytes).map_err(failure)?;
        let manifest = transaction.manifest().clone();
        let plugin_id = manifest.id.as_str().to_owned();
        let new_version_text = version_text(manifest.version);

        let Inner { host, store, .. } = &mut *inner;
        let Some(store) = store.as_mut() else {
            return Err(crate::local_store::FfiError::Store {
                code: "INVALID_STATE_TRANSITION".to_owned(),
                reason: "install_package requires the persistent plugin host".to_owned(),
            });
        };
        // 版本基线取自**库**里的已装行（重开后仍然一致）。
        let rows = store.load_plugins().map_err(storage_failure)?;
        let previous = rows.into_iter().find(|row| row.id == plugin_id);
        let mut previous_version_text: Option<String> = None;
        let upgraded_from = match &previous {
            None => None,
            Some(row) => {
                let installed = parse_manifest(&row.manifest_json, &limits).map_err(failure)?;
                if installed.version == manifest.version {
                    return Err(failure(chat_plugins::PluginError::RegistrationConflict));
                }
                if installed.version > manifest.version {
                    return Err(failure(chat_plugins::PluginError::VersionDowngrade {
                        from: installed.version,
                        to: manifest.version,
                    }));
                }
                let text = version_text(installed.version);
                previous_version_text = Some(text.clone());
                Some(text)
            }
        };
        transaction.stage().map_err(failure)?;
        // ① 落库：单条 upsert 就是一个 storage 事务（同时刷新 state/updated_at_ms）。
        store
            .upsert_plugin(&plugin_id, &manifest_json, STATE_INSTALLED)
            .map_err(storage_failure)?;
        // ①b 升级路径的私有 KV：**Keep**（默认）原样保留；失败就整体回滚到旧的插件行。
        if let Some(from_version) = previous_version_text.as_deref() {
            if let Err(error) = store.plugin_kv_rename_or_keep(
                &plugin_id,
                from_version,
                &new_version_text,
                chat_storage::formal::KvMigrationPolicy::Keep,
            ) {
                if let Some(row) = &previous {
                    let _ = store.upsert_plugin(&row.id, &row.manifest_json, &row.state);
                } else {
                    let _ = store.delete_plugin(&plugin_id);
                }
                return Err(storage_failure(error));
            }
        }
        // ② 内存宿主提交；失败则把库恢复成提交前的样子。
        match transaction.commit(host) {
            Ok(_) => Ok(InstallOutcomeFfi {
                plugin_id,
                version: new_version_text,
                upgraded_from,
            }),
            Err(error) => {
                if let Some(row) = &previous {
                    // 升级失败：把旧行写回去（同一 upsert 语义）。
                    let _ = store.upsert_plugin(&row.id, &row.manifest_json, &row.state);
                } else {
                    // 首次安装失败：删掉刚写入的行。
                    let _ = store.delete_plugin(&plugin_id);
                }
                Err(failure(error))
            }
        }
    }

    /// 启动插件（签发运行时句柄）。
    pub fn start(&self, plugin_id: String) -> Result<(), FfiError> {
        let plugin = Self::plugin_id(&plugin_id)?;
        let mut inner = self.lock()?;
        if inner.closed {
            return Err(core_closed());
        }
        let runtime = inner.host.start(&plugin).map_err(failure)?;
        if let Some(store) = inner.store.as_mut() {
            store
                .set_plugin_state(&plugin_id, STATE_RUNNING)
                .map_err(storage_failure)?;
        }
        inner.runtimes.insert(plugin_id, runtime);
        Ok(())
    }

    /// 生成分发计划（**不执行插件代码**）。
    pub fn dispatch(
        &self,
        plugin_id: String,
        extension_point: String,
        operation_id: String,
        input: Vec<u8>,
        now_unix_ms: u64,
        deadline_unix_ms: u64,
    ) -> Result<DispatchPlanViewFfi, FfiError> {
        let plugin = Self::plugin_id(&plugin_id)?;
        let point = Self::extension_point(&extension_point)?;
        let operation = Self::operation_id(&operation_id)?;
        let mut inner = self.lock()?;
        if inner.closed {
            return Err(core_closed());
        }
        // 运行时句柄是 Clone 的轻量签名：先克隆再借用宿主，避免同时借用同一结构。
        let runtime = Self::runtime(&inner, &plugin)?.clone();
        let plan = inner
            .host
            .dispatch(&runtime, point, operation, &input, now_unix_ms, deadline_unix_ms)
            .map_err(failure)?;
        Ok(plan_view(&plan))
    }

    /// 校验插件回报的输出（大小/截止时间/幂等）。
    pub fn finish_dispatch(
        &self,
        plugin_id: String,
        operation_id: String,
        output: Vec<u8>,
        now_unix_ms: u64,
    ) -> Result<(), FfiError> {
        let plugin = Self::plugin_id(&plugin_id)?;
        let operation = Self::operation_id(&operation_id)?;
        let mut inner = self.lock()?;
        if inner.closed {
            return Err(core_closed());
        }
        let runtime = Self::runtime(&inner, &plugin)?.clone();
        inner
            .host
            .finish_dispatch(&runtime, operation, &output, now_unix_ms)
            .map_err(failure)
    }

    /// 停止插件；返回是否真的从运行态切到停止态。
    pub fn stop(&self, plugin_id: String) -> Result<bool, FfiError> {
        let plugin = Self::plugin_id(&plugin_id)?;
        let mut inner = self.lock()?;
        if inner.closed {
            return Err(core_closed());
        }
        let stopped = inner.host.stop(&plugin).map_err(failure)?;
        if stopped {
            if let Some(store) = inner.store.as_mut() {
                store
                    .set_plugin_state(&plugin_id, STATE_STOPPED)
                    .map_err(storage_failure)?;
            }
        }
        inner.runtimes.remove(&plugin_id);
        Ok(stopped)
    }

    /// 卸载插件（可选同时删除私有数据）。
    pub fn uninstall(&self, plugin_id: String, remove_data: bool) -> Result<bool, FfiError> {
        let plugin = Self::plugin_id(&plugin_id)?;
        let mut inner = self.lock()?;
        if inner.closed {
            return Err(core_closed());
        }
        let removed = inner
            .host
            .uninstall(&plugin, remove_data)
            .map_err(failure)?;
        inner.runtimes.remove(&plugin_id);
        if removed {
            if let Some(store) = inner.store.as_mut() {
                if remove_data {
                    // 真删行：plugin_kv 随外键级联一起清空。
                    store.delete_plugin(&plugin_id).map_err(storage_failure)?;
                } else {
                    // 保留数据：行留着但标记为未安装（KV 不动，重装后仍可读）。
                    store
                        .set_plugin_state(&plugin_id, STATE_UNINSTALLED)
                        .map_err(storage_failure)?;
                }
            }
        }
        Ok(removed)
    }

    /// 读插件私有存储（需要 `PrivateStorage` 能力，只能读自身命名空间）。
    pub fn storage_get(
        &self,
        plugin_id: String,
        key: String,
    ) -> Result<Option<Vec<u8>>, FfiError> {
        let plugin = Self::plugin_id(&plugin_id)?;
        let inner = self.lock()?;
        if inner.closed {
            return Err(core_closed());
        }
        // 持久化模式：能力仍要声明（清单来自宿主），值直接从库里读（重开后也能读）。
        if let Some(store) = inner.store.as_ref() {
            Self::require_private_storage(&inner, &plugin)?;
            return store
                .plugin_kv_get(plugin.as_str(), &key)
                .map_err(storage_failure);
        }
        let runtime = Self::runtime(&inner, &plugin)?;
        inner
            .host
            .data_get(runtime, &key)
            .map(|value| value.map(|bytes| bytes.to_vec()))
            .map_err(failure)
    }

    /// 写插件私有存储（需要 `PrivateStorage` 能力）。
    pub fn storage_set(
        &self,
        plugin_id: String,
        key: String,
        value: Vec<u8>,
    ) -> Result<(), FfiError> {
        let plugin = Self::plugin_id(&plugin_id)?;
        let mut inner = self.lock()?;
        if inner.closed {
            return Err(core_closed());
        }
        if inner.store.is_some() {
            // 持久化模式：能力先由宿主校验（清单/安装态/能力），再由库做有界配额。
            Self::require_private_storage(&inner, &plugin)?;
            let store = inner.store.as_mut().expect("checked above");
            return store
                .plugin_kv_set(plugin.as_str(), &key, &value)
                .map_err(storage_failure);
        }
        let runtime = Self::runtime(&inner, &plugin)?.clone();
        inner
            .host
            .data_put(&runtime, &key, &value)
            .map_err(failure)
    }

    /// PLUGINS：清理**已卸载**的插件残留（行 + 私有 KV）。
    ///
    /// - 只删 `state = uninstalled` 且 `updated_at_ms < older_than_ms`（严格小于）的行；
    /// - 每行删除时 `plugin_kv` 由外键级联清空（0010 的 ON DELETE CASCADE）；
    /// - 单次最多 256 行，返回实际删除数；纯内存对象 → `INVALID_STATE_TRANSITION`。
    pub fn purge_uninstalled_plugins(&self, older_than_ms: u64) -> Result<u32, FfiError> {
        let mut inner = self.lock()?;
        if inner.closed {
            return Err(core_closed());
        }
        let Inner { store, .. } = &mut *inner;
        let Some(store) = store.as_mut() else {
            return Err(FfiError::Store {
                code: "INVALID_STATE_TRANSITION".to_owned(),
                reason: "purge_uninstalled_plugins requires the persistent plugin host"
                    .to_owned(),
            });
        };
        let threshold = i64::try_from(older_than_ms).map_err(|_| FfiError::Store {
            code: "INVALID_INPUT".to_owned(),
            reason: "the purge threshold is out of range".to_owned(),
        })?;
        let removed = store
            .purge_uninstalled_plugins(threshold, chat_storage::formal::MAX_PLUGIN_PURGE)
            .map_err(storage_failure)?;
        Ok(u32::try_from(removed).unwrap_or(u32::MAX))
    }

    /// 已安装插件列表（纯数据）。
    pub fn list_plugins(&self) -> Result<Vec<PluginInfoFfi>, FfiError> {
        let inner = self.lock()?;
        if inner.closed {
            return Err(core_closed());
        }
        Ok(inner
            .host
            .installed_plugins()
            .into_iter()
            .map(|plugin| {
                let running = inner.host.is_running(&plugin);
                let extension_points = inner
                    .host
                    .manifest(&plugin)
                    .map(|manifest| {
                        manifest
                            .extension_points
                            .iter()
                            .map(|point| point.as_str().to_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                let manifest = inner.host.manifest(&plugin);
                PluginInfoFfi {
                    plugin_id: plugin.as_str().to_owned(),
                    running,
                    extension_points,
                    version: manifest.map(|view| version_text(view.version)).unwrap_or_default(),
                    manifest: manifest
                        .map(manifest_view)
                        .unwrap_or_else(empty_manifest_view),
                }
            })
            .collect())
    }

    /// 是否仍处于打开状态。
    pub fn is_open(&self) -> bool {
        match self.inner.lock() {
            Ok(inner) => !inner.closed,
            Err(_) => false,
        }
    }

    /// 关闭：停止所有运行中的插件并标记关闭；幂等，之后所有调用返回 `CORE_CLOSED`。
    pub fn shutdown(&self) {
        let mut inner = match self.inner.lock() {
            Ok(inner) => inner,
            Err(_) => return,
        };
        if inner.closed {
            return;
        }
        let running: Vec<String> = inner.runtimes.keys().cloned().collect();
        for plugin_id in running {
            if let Ok(plugin) = PluginId::parse(&plugin_id) {
                let _ = inner.host.stop(&plugin);
            }
        }
        inner.runtimes.clear();
        // 释放正式库句柄（独占写者）：这样同一个库可以被重新 open。
        inner.store = None;
        inner.closed = true;
    }
}
