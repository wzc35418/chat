//! 宿主资源预算。
//!
//! 所有预算都有非零下界与硬上限：调用方可以收紧，不能放大到无界执行。

use crate::error::{PluginError, Result};

/// 单字节类预算的硬上限（1 MiB）。
pub const HARD_MAX_LIMIT_BYTES: usize = 1024 * 1024;
/// 超时预算的硬上限（60 秒）。
pub const HARD_MAX_TIMEOUT_MS: u64 = 60_000;
/// 计数类预算的硬上限。
pub const HARD_MAX_COUNT: usize = 1024;

/// 插件宿主资源预算。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PluginLimits {
    /// 清单 JSON 字节上限。
    pub max_manifest_bytes: usize,
    /// 单次调用输入字节上限。
    pub max_input_bytes: usize,
    /// 单次调用输出字节上限。
    pub max_output_bytes: usize,
    /// 单次调用允许的最大超时预算（毫秒）。
    pub max_timeout_ms: u64,
    /// 私有存储单键字节上限。
    pub max_storage_key_bytes: usize,
    /// 私有存储单值字节上限。
    pub max_storage_value_bytes: usize,
    /// 私有存储单插件条目数上限。
    pub max_storage_entries: usize,
    /// 私有存储单插件总字节上限。
    pub max_storage_bytes: usize,
    /// 清单能力数上限。
    pub max_capabilities: usize,
    /// 清单扩展点数上限。
    pub max_extension_points: usize,
    /// 幂等 operation 缓存条目上限。
    pub max_idempotency_entries: usize,
}

impl PluginLimits {
    /// 冻结默认预算。
    pub const DEFAULT: PluginLimits = PluginLimits {
        max_manifest_bytes: 64 * 1024,
        max_input_bytes: 64 * 1024,
        max_output_bytes: 64 * 1024,
        max_timeout_ms: 10_000,
        max_storage_key_bytes: 128,
        max_storage_value_bytes: 8 * 1024,
        max_storage_entries: 256,
        max_storage_bytes: 64 * 1024,
        max_capabilities: 16,
        max_extension_points: 8,
        max_idempotency_entries: 64,
    };

    /// 校验预算可用：每项非零且不超过硬上限。
    pub fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("maxManifestBytes", self.max_manifest_bytes),
            ("maxInputBytes", self.max_input_bytes),
            ("maxOutputBytes", self.max_output_bytes),
            ("maxStorageKeyBytes", self.max_storage_key_bytes),
            ("maxStorageValueBytes", self.max_storage_value_bytes),
            ("maxStorageBytes", self.max_storage_bytes),
        ] {
            if value == 0 || value > HARD_MAX_LIMIT_BYTES {
                return Err(PluginError::InvalidLimits { detail: name });
            }
        }
        if self.max_timeout_ms == 0 || self.max_timeout_ms > HARD_MAX_TIMEOUT_MS {
            return Err(PluginError::InvalidLimits {
                detail: "maxTimeoutMs",
            });
        }
        for (name, value) in [
            ("maxStorageEntries", self.max_storage_entries),
            ("maxCapabilities", self.max_capabilities),
            ("maxExtensionPoints", self.max_extension_points),
            ("maxIdempotencyEntries", self.max_idempotency_entries),
        ] {
            if value == 0 || value > HARD_MAX_COUNT {
                return Err(PluginError::InvalidLimits { detail: name });
            }
        }
        // 单值预算不能大于总预算，否则配额检查自相矛盾。
        if self.max_storage_value_bytes > self.max_storage_bytes {
            return Err(PluginError::InvalidLimits {
                detail: "maxStorageValueBytes exceeds maxStorageBytes",
            });
        }
        Ok(())
    }
}

impl Default for PluginLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_limits_are_valid() {
        PluginLimits::DEFAULT.validate().unwrap();
        PluginLimits::default().validate().unwrap();
    }

    #[test]
    fn zero_and_huge_budgets_are_rejected() {
        let mut limits = PluginLimits::DEFAULT;
        limits.max_input_bytes = 0;
        assert_eq!(
            limits.validate().unwrap_err().code(),
            "INVALID_PLUGIN_LIMITS"
        );
        let mut limits = PluginLimits::DEFAULT;
        limits.max_output_bytes = HARD_MAX_LIMIT_BYTES + 1;
        assert!(limits.validate().is_err());
        let mut limits = PluginLimits::DEFAULT;
        limits.max_timeout_ms = 0;
        assert!(limits.validate().is_err());
        let mut limits = PluginLimits::DEFAULT;
        limits.max_timeout_ms = HARD_MAX_TIMEOUT_MS + 1;
        assert!(limits.validate().is_err());
        let mut limits = PluginLimits::DEFAULT;
        limits.max_storage_entries = 0;
        assert!(limits.validate().is_err());
        let mut limits = PluginLimits::DEFAULT;
        limits.max_storage_value_bytes = limits.max_storage_bytes + 1;
        assert!(limits.validate().is_err());
    }
}
