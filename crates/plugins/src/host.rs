//! 插件宿主最小闭环（纯逻辑层）。
//!
//! 生命周期：安装清单 → 启动（按清单注册扩展点）→ 分发计划 → 完成校验 → 停止/卸载；
//! 期间插件只能通过宿主签发的 [`PluginRuntime`] 访问自己的扩展点与私有命名空间。
//!
//! 本模块**不执行插件代码**：[`PluginHost::dispatch`] 只做身份/注册/大小/超时预算与
//! 幂等检查并返回 [`DispatchPlan`]；[`PluginHost::finish_dispatch`] 校验宿主回报的
//! 输出大小与截止时间。真正的 JAR/ClassLoader 执行属于 P4 宿主集成，未在本 crate 实现。

use std::collections::{BTreeMap, VecDeque};

use chat_protocol::OperationId;

use crate::error::{PluginError, Result};
use crate::limits::PluginLimits;
use crate::manifest::{ExtensionPoint, PluginCapability, PluginId, PluginManifest, parse_manifest};
use crate::registry::{ExtensionPointRegistry, RegistrationOutcome};
use crate::storage::{PluginStorage, StorageUsage};

/// 宿主签发的插件运行时句柄。字段私有：插件无法伪造他人句柄。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginRuntime {
    plugin_id: PluginId,
}

impl PluginRuntime {
    /// 该句柄绑定的插件。
    pub fn plugin_id(&self) -> &PluginId {
        &self.plugin_id
    }
}

/// 一次扩展点调用的分发计划（不含插件代码）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchPlan {
    plugin_id: PluginId,
    extension_point: ExtensionPoint,
    operation_id: OperationId,
    input_bytes: usize,
    output_limit_bytes: usize,
    deadline_unix_ms: u64,
    timeout_ms: u64,
}

impl DispatchPlan {
    /// 目标插件。
    pub fn plugin_id(&self) -> &PluginId {
        &self.plugin_id
    }

    /// 扩展点。
    pub fn extension_point(&self) -> ExtensionPoint {
        self.extension_point
    }

    /// 幂等键。
    pub fn operation_id(&self) -> OperationId {
        self.operation_id
    }

    /// 输入字节数。
    pub fn input_bytes(&self) -> usize {
        self.input_bytes
    }

    /// 输出字节上限。
    pub fn output_limit_bytes(&self) -> usize {
        self.output_limit_bytes
    }

    /// 生效截止时间（可能被宿主预算收紧）。
    pub fn deadline_unix_ms(&self) -> u64 {
        self.deadline_unix_ms
    }

    /// 生效超时预算（毫秒）。
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }
}

#[derive(Clone, Debug)]
struct CachedOperation {
    plugin_id: PluginId,
    extension_point: ExtensionPoint,
    input: Vec<u8>,
    output_limit_bytes: usize,
    deadline_unix_ms: u64,
    timeout_ms: u64,
    output: Option<Vec<u8>>,
}

/// 插件宿主。所有状态在本机内存中，纯函数式推进；不联网、不做文件 I/O。
#[derive(Clone, Debug)]
pub struct PluginHost {
    limits: PluginLimits,
    manifests: BTreeMap<PluginId, PluginManifest>,
    runtimes: BTreeMap<PluginId, PluginRuntime>,
    registry: ExtensionPointRegistry,
    storage: PluginStorage,
    operations: BTreeMap<OperationId, CachedOperation>,
    operation_order: VecDeque<OperationId>,
}

impl PluginHost {
    /// 以显式预算创建宿主。
    pub fn new(limits: PluginLimits) -> Result<Self> {
        limits.validate()?;
        Ok(Self {
            limits,
            manifests: BTreeMap::new(),
            runtimes: BTreeMap::new(),
            registry: ExtensionPointRegistry::default(),
            storage: PluginStorage::default(),
            operations: BTreeMap::new(),
            operation_order: VecDeque::new(),
        })
    }

    /// 使用冻结默认预算创建宿主。
    pub fn with_default_limits() -> Self {
        Self::new(PluginLimits::DEFAULT).expect("default plugin limits are valid")
    }

    /// 当前预算。
    pub fn limits(&self) -> PluginLimits {
        self.limits
    }

    /// 安装已校验清单；重复 ID 显式拒绝（本切片不做升级）。
    pub fn install(&mut self, manifest: PluginManifest) -> Result<()> {
        if self.manifests.contains_key(&manifest.id) {
            return Err(PluginError::PluginIdConflict);
        }
        self.manifests.insert(manifest.id.clone(), manifest);
        Ok(())
    }

    /// 解析并安装清单 JSON，返回插件 ID。
    pub fn install_json(&mut self, json: &str) -> Result<PluginId> {
        let manifest = parse_manifest(json, &self.limits)?;
        let id = manifest.id.clone();
        self.install(manifest)?;
        Ok(id)
    }

    /// 是否已安装。
    pub fn is_installed(&self, plugin: &PluginId) -> bool {
        self.manifests.contains_key(plugin)
    }

    /// 已安装清单。
    pub fn manifest(&self, plugin: &PluginId) -> Option<&PluginManifest> {
        self.manifests.get(plugin)
    }

    /// 已安装插件列表（升序）。
    pub fn installed_plugins(&self) -> Vec<PluginId> {
        self.manifests.keys().cloned().collect()
    }

    /// 启动插件：按清单注册扩展点并签发运行时句柄。重复启动幂等。
    pub fn start(&mut self, plugin: &PluginId) -> Result<PluginRuntime> {
        let manifest = self
            .manifests
            .get(plugin)
            .ok_or(PluginError::NotInstalled)?
            .clone();
        if let Some(runtime) = self.runtimes.get(plugin) {
            return Ok(runtime.clone());
        }
        // 首次启动按清单注册；重复注册由注册表再兜底为幂等。
        let _outcome: RegistrationOutcome =
            self.registry.register(plugin, &manifest.extension_points)?;
        let runtime = PluginRuntime {
            plugin_id: plugin.clone(),
        };
        self.runtimes.insert(plugin.clone(), runtime.clone());
        Ok(runtime)
    }

    /// 是否运行中。
    pub fn is_running(&self, plugin: &PluginId) -> bool {
        self.runtimes.contains_key(plugin)
    }

    /// 取现有运行时句柄。
    pub fn runtime(&self, plugin: &PluginId) -> Option<&PluginRuntime> {
        self.runtimes.get(plugin)
    }

    /// 停止插件：撤销全部注册项并清空该插件的在途 operation。可重复调用。
    pub fn stop(&mut self, plugin: &PluginId) -> Result<bool> {
        if !self.manifests.contains_key(plugin) {
            return Err(PluginError::NotInstalled);
        }
        let was_running = self.runtimes.remove(plugin).is_some();
        self.registry.unregister(plugin);
        self.clear_operations_for(plugin);
        Ok(was_running)
    }

    /// 卸载插件；`remove_data` 为真时同时清空私有存储。可重复调用（幂等）。
    pub fn uninstall(&mut self, plugin: &PluginId, remove_data: bool) -> Result<bool> {
        let Some(_) = self.manifests.remove(plugin) else {
            return Ok(false);
        };
        self.runtimes.remove(plugin);
        self.registry.unregister(plugin);
        self.clear_operations_for(plugin);
        if remove_data {
            self.storage.clear(plugin);
        }
        Ok(true)
    }

    /// INSTALL：把已校验清单写入/替换到宿主（**不做版本判定**，由安装事务负责）。
    ///
    /// 只改 manifest 表：注册项与运行时句柄由安装事务在替换前后显式处理，
    /// 因此这里不会留下"清单换了但注册项还是旧的"的半状态。
    pub(crate) fn replace_manifest(&mut self, manifest: PluginManifest) -> Option<PluginManifest> {
        self.manifests.insert(manifest.id.clone(), manifest)
    }

    /// 扩展点注册表（只读）。
    pub fn registry(&self) -> &ExtensionPointRegistry {
        &self.registry
    }

    fn clear_operations_for(&mut self, plugin: &PluginId) {
        self.operations.retain(|_, op| op.plugin_id != *plugin);
        let operations = &self.operations;
        self.operation_order
            .retain(|operation_id| operations.contains_key(operation_id));
    }

    fn require_running(&self, runtime: &PluginRuntime) -> Result<&PluginManifest> {
        let manifest = self
            .manifests
            .get(runtime.plugin_id())
            .ok_or(PluginError::NotInstalled)?;
        if !self.runtimes.contains_key(runtime.plugin_id()) {
            return Err(PluginError::NotRunning);
        }
        Ok(manifest)
    }

    fn require_storage(&self, runtime: &PluginRuntime) -> Result<&PluginManifest> {
        let manifest = self.require_running(runtime)?;
        if !manifest.has_capability(PluginCapability::PrivateStorage) {
            return Err(PluginError::CapabilityNotDeclared);
        }
        Ok(manifest)
    }

    /// 生成受预算约束的分发计划；同一 `operation_id` 的相同请求幂等重放。
    ///
    /// 失败不产生计划、不占用 operation 缓存：
    /// - 未安装 → `PLUGIN_NOT_INSTALLED`；未运行（含已停止）→ `PLUGIN_NOT_RUNNING`；
    /// - 该插件未注册此扩展点（含注销后调用）→ `PLUGIN_NOT_REGISTERED`；
    /// - 输入超限 → `CONTENT_TOO_LARGE`；
    /// - 截止时间已到 → `PLUGIN_TASK_TIMEOUT`；超出宿主超时预算时收紧到预算上限；
    /// - 同一 operationId 绑定不同插件/扩展点/输入 → `PLUGIN_OPERATION_CONFLICT`。
    pub fn dispatch(
        &mut self,
        runtime: &PluginRuntime,
        extension_point: ExtensionPoint,
        operation_id: OperationId,
        input: &[u8],
        now_unix_ms: u64,
        deadline_unix_ms: u64,
    ) -> Result<DispatchPlan> {
        let plugin = runtime.plugin_id().clone();
        self.require_running(runtime)?;
        if !self.registry.supports(&plugin, extension_point) {
            return Err(PluginError::NotRegistered);
        }
        if input.len() > self.limits.max_input_bytes {
            return Err(PluginError::ContentTooLarge {
                size: input.len(),
                limit: self.limits.max_input_bytes,
            });
        }
        if deadline_unix_ms <= now_unix_ms {
            return Err(PluginError::TaskTimeout);
        }
        // 只收紧、不放宽：宿主预算之外的截止时间被压回预算上限。
        let remaining = deadline_unix_ms - now_unix_ms;
        let effective_deadline = if remaining > self.limits.max_timeout_ms {
            now_unix_ms + self.limits.max_timeout_ms
        } else {
            deadline_unix_ms
        };
        let timeout_ms = effective_deadline - now_unix_ms;

        if let Some(cached) = self.operations.get(&operation_id) {
            let same_request = cached.plugin_id == plugin
                && cached.extension_point == extension_point
                && cached.input == input;
            if !same_request {
                return Err(PluginError::OperationConflict);
            }
            return Ok(self.plan_from_cache(&operation_id, cached));
        }

        self.evict_operations_if_needed();
        let cached = CachedOperation {
            plugin_id: plugin.clone(),
            extension_point,
            input: input.to_vec(),
            output_limit_bytes: self.limits.max_output_bytes,
            deadline_unix_ms: effective_deadline,
            timeout_ms,
            output: None,
        };
        self.operation_order.push_back(operation_id);
        self.operations.insert(operation_id, cached.clone());
        Ok(self.plan_from_cache(&operation_id, &cached))
    }

    fn plan_from_cache(
        &self,
        operation_id: &OperationId,
        cached: &CachedOperation,
    ) -> DispatchPlan {
        DispatchPlan {
            plugin_id: cached.plugin_id.clone(),
            extension_point: cached.extension_point,
            operation_id: *operation_id,
            input_bytes: cached.input.len(),
            output_limit_bytes: cached.output_limit_bytes,
            deadline_unix_ms: cached.deadline_unix_ms,
            timeout_ms: cached.timeout_ms,
        }
    }

    fn evict_operations_if_needed(&mut self) {
        while self.operations.len() >= self.limits.max_idempotency_entries {
            let Some(oldest) = self.operation_order.pop_front() else {
                break;
            };
            self.operations.remove(&oldest);
        }
    }

    /// 校验宿主回报的输出：大小上限、截止时间与幂等重放。
    ///
    /// 失败不覆盖已记录结果：超时/超限返回错误且保持原状；同一 operation 的相同输出
    /// 幂等成功，不同输出 `PLUGIN_OPERATION_CONFLICT`。
    pub fn finish_dispatch(
        &mut self,
        runtime: &PluginRuntime,
        operation_id: OperationId,
        output: &[u8],
        now_unix_ms: u64,
    ) -> Result<()> {
        self.require_running(runtime)?;
        let plugin = runtime.plugin_id();
        let Some(cached) = self.operations.get(&operation_id) else {
            return Err(PluginError::OperationUnknown);
        };
        if cached.plugin_id != *plugin {
            // 不泄露其他插件的 operation 是否存在。
            return Err(PluginError::OperationUnknown);
        }
        if now_unix_ms > cached.deadline_unix_ms {
            return Err(PluginError::TaskTimeout);
        }
        if output.len() > cached.output_limit_bytes {
            return Err(PluginError::ContentTooLarge {
                size: output.len(),
                limit: cached.output_limit_bytes,
            });
        }
        match &cached.output {
            Some(existing) if existing.as_slice() == output => Ok(()),
            Some(_) => Err(PluginError::OperationConflict),
            None => {
                if let Some(cached) = self.operations.get_mut(&operation_id) {
                    cached.output = Some(output.to_vec());
                }
                Ok(())
            }
        }
    }

    /// 写入插件私有存储（需要 `PrivateStorage` 能力）。
    pub fn data_put(&mut self, runtime: &PluginRuntime, key: &str, value: &[u8]) -> Result<()> {
        self.require_storage(runtime)?;
        self.storage
            .put(runtime.plugin_id(), key, value, &self.limits)
    }

    /// 读取插件私有存储（只读自身命名空间）。
    pub fn data_get(&self, runtime: &PluginRuntime, key: &str) -> Result<Option<&[u8]>> {
        self.require_storage(runtime)?;
        Ok(self.storage.get(runtime.plugin_id(), key))
    }

    /// 列出自身命名空间的键。
    pub fn data_keys(&self, runtime: &PluginRuntime) -> Result<Vec<String>> {
        self.require_storage(runtime)?;
        Ok(self.storage.keys(runtime.plugin_id()))
    }

    /// 删除自身命名空间的键。
    pub fn data_remove(&mut self, runtime: &PluginRuntime, key: &str) -> Result<bool> {
        self.require_storage(runtime)?;
        Ok(self.storage.remove(runtime.plugin_id(), key))
    }

    /// 自身命名空间用量。
    pub fn data_usage(&self, runtime: &PluginRuntime) -> Result<StorageUsage> {
        self.require_storage(runtime)?;
        Ok(self.storage.usage(runtime.plugin_id()))
    }
}
