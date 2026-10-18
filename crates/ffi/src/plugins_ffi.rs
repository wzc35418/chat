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
    DispatchPlan, ExtensionPoint, PluginHost, PluginId, PluginLimits, PluginManifest, PluginRuntime,
    parse_manifest,
};
use chat_protocol::OperationId;

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

/// 已安装插件（纯数据）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PluginInfoFfi {
    /// 插件 ID。
    pub plugin_id: String,
    /// 是否正在运行。
    pub running: bool,
    /// 可注册扩展点。
    pub extension_points: Vec<String>,
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

struct Inner {
    host: PluginHost,
    limits: PluginLimits,
    runtimes: BTreeMap<String, PluginRuntime>,
    closed: bool,
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
            }),
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
        inner.host.install(manifest).map_err(failure)?;
        Ok(plugin_id)
    }

    /// 启动插件（签发运行时句柄）。
    pub fn start(&self, plugin_id: String) -> Result<(), FfiError> {
        let plugin = Self::plugin_id(&plugin_id)?;
        let mut inner = self.lock()?;
        if inner.closed {
            return Err(core_closed());
        }
        let runtime = inner.host.start(&plugin).map_err(failure)?;
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
        let runtime = Self::runtime(&inner, &plugin)?.clone();
        inner
            .host
            .data_put(&runtime, &key, &value)
            .map_err(failure)
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
                PluginInfoFfi {
                    plugin_id: plugin.as_str().to_owned(),
                    running,
                    extension_points,
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
        inner.closed = true;
    }
}
