//! PLUGINS：插件安装**事务**（P4 安装事务的最小闭环）。
//!
//! 流程（任何一步失败都不留半状态）：
//!
//! 1. `InstallTransaction::begin`：校验包结构（inspect_plugin_package），只读不落地；
//!    包结构/清单/SPI 不合法的包根本进不了事务；
//! 2. `InstallTransaction::stage`：把清单固定为"待提交"（幂等），宿主仍未改变；
//! 3. `InstallTransaction::commit`：按版本语义原子登记到 PluginHost：
//!    未安装 → 安装；更高版本 → 升级（返回 from/to）；同版本 → PLUGIN_STATE_CONFLICT；
//!    更低版本 → PLUGIN_VERSION_DOWNGRADE；commit 中任何失败都会恢复提交前状态；
//! 4. `InstallTransaction::rollback`：放弃事务（暂存清空，宿主不受影响）。

use crate::error::{PluginError, Result};
use crate::host::PluginHost;
use crate::limits::PluginLimits;
use crate::manifest::{PluginId, PluginManifest, PluginVersion, parse_manifest};
use crate::package::{PackageInspection, inspect_plugin_package, package_manifest_json};

/// 一次安装事务的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    /// 首次安装。
    Installed {
        /// 插件 ID。
        plugin_id: PluginId,
        /// 新装版本。
        version: PluginVersion,
    },
    /// 升级（更高版本覆盖旧版本）。
    Upgraded {
        /// 插件 ID。
        plugin_id: PluginId,
        /// 旧版本。
        from: PluginVersion,
        /// 新版本。
        to: PluginVersion,
    },
}

/// 安装事务（**不执行任何插件代码**）。
#[derive(Debug)]
pub struct InstallTransaction {
    manifest: PluginManifest,
    inspection: PackageInspection,
    limits: PluginLimits,
    staged: bool,
    finished: bool,
}

impl InstallTransaction {
    /// 开始事务：校验包结构并固定清单（失败 → 事务根本没建立，宿主不受影响）。
    pub fn begin(package_bytes: &[u8], limits: PluginLimits) -> Result<Self> {
        let inspection = inspect_plugin_package(package_bytes, &limits)?;
        let manifest = parse_manifest(&package_manifest_json(package_bytes)?, &limits)?;
        if manifest.id.as_str() != inspection.plugin_id {
            return Err(PluginError::PackageCorrupted {
                detail: "inspected manifest identity does not match the parsed manifest",
            });
        }
        Ok(Self {
            manifest,
            inspection,
            limits,
            staged: false,
            finished: false,
        })
    }

    /// 包结构校验摘要（诊断用）。
    pub fn inspection(&self) -> &PackageInspection {
        &self.inspection
    }

    /// 待安装清单。
    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    /// 事务预算。
    pub fn limits(&self) -> PluginLimits {
        self.limits
    }

    /// 是否已暂存。
    pub fn is_staged(&self) -> bool {
        self.staged
    }

    /// 是否已结束（提交成功或回滚过）。
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// 暂存（幂等）：清单固定为待提交，宿主仍未改变。
    pub fn stage(&mut self) -> Result<()> {
        self.require_open()?;
        self.staged = true;
        Ok(())
    }

    /// 放弃事务：暂存状态丢弃，宿主保持原样（幂等）。
    pub fn rollback(&mut self) {
        self.staged = false;
        self.finished = true;
    }

    fn require_open(&self) -> Result<()> {
        if self.finished {
            return Err(PluginError::PackageCorrupted {
                detail: "the install transaction is already finished",
            });
        }
        Ok(())
    }
}
impl InstallTransaction {
    /// 提交：按版本语义原子登记到宿主；失败自动回滚到提交前状态。
    pub fn commit(&mut self, host: &mut PluginHost) -> Result<InstallOutcome> {
        self.require_open()?;
        if !self.staged {
            return Err(PluginError::PackageCorrupted {
                detail: "the install transaction must be staged before commit",
            });
        }
        let plugin_id = self.manifest.id.clone();
        let new_version = self.manifest.version;
        let previous = host.manifest(&plugin_id).cloned();
        let was_running = host.is_running(&plugin_id);
        let outcome = match &previous {
            None => InstallOutcome::Installed {
                plugin_id: plugin_id.clone(),
                version: new_version,
            },
            Some(old) if old.version == new_version => {
                // 同版本重装：本切片不支持（显式冲突，不假装成功）。
                return Err(PluginError::RegistrationConflict);
            }
            Some(old) if old.version > new_version => {
                return Err(PluginError::VersionDowngrade {
                    from: old.version,
                    to: new_version,
                });
            }
            Some(old) => InstallOutcome::Upgraded {
                plugin_id: plugin_id.clone(),
                from: old.version,
                to: new_version,
            },
        };
        // 先停掉旧运行时（注销扩展点），再换清单；任何一步失败都恢复旧状态。
        if previous.is_some() {
            if let Err(error) = host.stop(&plugin_id) {
                restore(host, &plugin_id, previous.as_ref(), was_running);
                return Err(error);
            }
        }
        host.replace_manifest(self.manifest.clone());
        if was_running {
            if let Err(error) = host.start(&plugin_id) {
                restore(host, &plugin_id, previous.as_ref(), was_running);
                return Err(error);
            }
        }
        self.finished = true;
        Ok(outcome)
    }
}

/// 把宿主恢复到 commit 之前的状态（尽力而为，但绝不留下"新旧混合"）。
fn restore(
    host: &mut PluginHost,
    plugin_id: &PluginId,
    previous: Option<&PluginManifest>,
    was_running: bool,
) {
    match previous {
        Some(old) => {
            host.replace_manifest(old.clone());
            if was_running {
                let _ = host.start(plugin_id);
            }
        }
        None => {
            // 之前没装过：把可能写入的清单与注册项一起清掉。
            let _ = host.uninstall(plugin_id, false);
        }
    }
}

