//! 插件层稳定错误码。
//!
//! 与 PROTOCOL-014 的关系：
//!
//! - 尺寸类失败复用冻结码 `CONTENT_TOO_LARGE`（[\`PluginError::frozen_code\`] 返回
//!   `Some(ErrorCode::ContentTooLarge)`），可直接进入 `chat_protocol::map_error`；
//! - 其余 `PLUGIN_* / SDK_* / INVALID_PLUGIN_*` 是本 crate 新增的稳定码，尚未登记到
//!   `chat_protocol::ErrorCode` 冻结码表（新增协议码需要协议模块版本化变更）。
//!   在协议层登记之前，跨 FFI 暴露时应映射为 `INTERNAL_FAILURE` 并保留本层码做诊断，
//!   不得伪装成 `NETWORK_UNAVAILABLE` 等可重试错误。
//! - 插件错误一律不可自动重试：需要用户干预（重装/重启/修正清单）。
//!
//! 新增稳定码必须同时登记到 `docs/implementation/plugins.md` 的码表。

use core::fmt;

use chat_protocol::ErrorCode;

/// 插件层错误。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginError {
    /// 清单结构/取值非法。
    InvalidManifest { detail: &'static str },
    /// 插件 ID 与已安装包冲突（本切片不做升级）。
    PluginIdConflict,
    /// SDK 或平台不兼容。
    SdkIncompatible { found: u16, supported: u16 },
    /// 目标平台不是宿主支持的平台。
    UnsupportedPlatform,
    /// 版本字符串非法。
    InvalidVersion { detail: &'static str },
    /// 未知扩展点。
    UnknownExtensionPoint,
    /// 未知能力声明。
    UnknownCapability,
    /// 未知权限声明。
    UnknownPermission,
    /// 注册扩展点所需能力未声明。
    CapabilityNotDeclared,
    /// 插件未安装。
    NotInstalled,
    /// 插件未运行（已停止或从未启动）。
    NotRunning,
    /// 插件未注册该扩展点（含注销后调用）。
    NotRegistered,
    /// 同一插件的注册集合冲突（不同扩展点集合重复注册）。
    RegistrationConflict,
    /// 同一个 operationId 对应不同请求。
    OperationConflict,
    /// operationId 不存在（未开始或已被清理）。
    OperationUnknown,
    /// 超时预算已耗尽。
    TaskTimeout,
    /// 输入/输出/manifest 超过上限。
    ContentTooLarge { size: usize, limit: usize },
    /// 存储键非法。
    InvalidStorageKey { detail: &'static str },
    /// 插件私有存储超出配额。
    StorageQuotaExceeded { detail: &'static str },
    /// 宿主限额配置非法（0 或超过硬上限）。
    InvalidLimits { detail: &'static str },
}

impl PluginError {
    /// 本层稳定码字符串。
    pub const fn code(&self) -> &'static str {
        match self {
            PluginError::InvalidManifest { .. } => "INVALID_PLUGIN_PACKAGE",
            PluginError::PluginIdConflict => "PLUGIN_ID_CONFLICT",
            PluginError::SdkIncompatible { .. } => "SDK_INCOMPATIBLE",
            PluginError::UnsupportedPlatform => "UNSUPPORTED_PLATFORM",
            PluginError::InvalidVersion { .. } => "INVALID_PLUGIN_VERSION",
            PluginError::UnknownExtensionPoint => "UNKNOWN_EXTENSION_POINT",
            PluginError::UnknownCapability => "UNKNOWN_CAPABILITY",
            PluginError::UnknownPermission => "UNKNOWN_PERMISSION",
            PluginError::CapabilityNotDeclared => "CAPABILITY_NOT_DECLARED",
            PluginError::NotInstalled => "PLUGIN_NOT_INSTALLED",
            PluginError::NotRunning => "PLUGIN_NOT_RUNNING",
            PluginError::NotRegistered => "PLUGIN_NOT_REGISTERED",
            PluginError::RegistrationConflict => "PLUGIN_STATE_CONFLICT",
            PluginError::OperationConflict => "PLUGIN_OPERATION_CONFLICT",
            PluginError::OperationUnknown => "PLUGIN_OPERATION_UNKNOWN",
            PluginError::TaskTimeout => "PLUGIN_TASK_TIMEOUT",
            PluginError::ContentTooLarge { .. } => "CONTENT_TOO_LARGE",
            PluginError::InvalidStorageKey { .. } => "INVALID_STORAGE_KEY",
            PluginError::StorageQuotaExceeded { .. } => "PLUGIN_STORAGE_QUOTA_EXCEEDED",
            PluginError::InvalidLimits { .. } => "INVALID_PLUGIN_LIMITS",
        }
    }

    /// 本层不允许自动重试（需要用户干预或修正清单）。
    pub const fn retryable(&self) -> bool {
        false
    }

    /// 已在 PROTOCOL-014 冻结码表中登记的同义码；没有则为 `None`。
    pub const fn frozen_code(&self) -> Option<ErrorCode> {
        match self {
            PluginError::ContentTooLarge { .. } => Some(ErrorCode::ContentTooLarge),
            _ => None,
        }
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PluginError::InvalidManifest { detail } => {
                write!(formatter, "invalid plugin manifest: {detail}")
            }
            PluginError::PluginIdConflict => formatter.write_str("plugin id is already installed"),
            PluginError::SdkIncompatible { found, supported } => write!(
                formatter,
                "plugin sdk {found} is incompatible with host sdk {supported}"
            ),
            PluginError::UnsupportedPlatform => formatter.write_str("unsupported plugin platform"),
            PluginError::InvalidVersion { detail } => {
                write!(formatter, "invalid plugin version: {detail}")
            }
            PluginError::UnknownExtensionPoint => formatter.write_str("unknown extension point"),
            PluginError::UnknownCapability => formatter.write_str("unknown capability"),
            PluginError::UnknownPermission => formatter.write_str("unknown permission"),
            PluginError::CapabilityNotDeclared => {
                formatter.write_str("required capability is not declared")
            }
            PluginError::NotInstalled => formatter.write_str("plugin is not installed"),
            PluginError::NotRunning => formatter.write_str("plugin is not running"),
            PluginError::NotRegistered => {
                formatter.write_str("plugin is not registered for this extension point")
            }
            PluginError::RegistrationConflict => {
                formatter.write_str("plugin registration set conflicts with the existing one")
            }
            PluginError::OperationConflict => {
                formatter.write_str("operation id is already bound to a different request")
            }
            PluginError::OperationUnknown => formatter.write_str("operation id is unknown"),
            PluginError::TaskTimeout => formatter.write_str("plugin task exceeded its deadline"),
            PluginError::ContentTooLarge { size, limit } => {
                write!(formatter, "content {size} bytes exceeds {limit}")
            }
            PluginError::InvalidStorageKey { detail } => {
                write!(formatter, "invalid storage key: {detail}")
            }
            PluginError::StorageQuotaExceeded { detail } => {
                write!(formatter, "plugin storage quota exceeded: {detail}")
            }
            PluginError::InvalidLimits { detail } => {
                write!(formatter, "invalid plugin host limits: {detail}")
            }
        }
    }
}

impl std::error::Error for PluginError {}

/// 本 crate 的 `Result`。
pub type Result<T> = core::result::Result<T, PluginError>;

/// 插件层稳定码全量，供唯一性测试与文档码表使用。
pub const CODE_TABLE: [&str; 20] = [
    "INVALID_PLUGIN_PACKAGE",
    "PLUGIN_ID_CONFLICT",
    "SDK_INCOMPATIBLE",
    "UNSUPPORTED_PLATFORM",
    "INVALID_PLUGIN_VERSION",
    "UNKNOWN_EXTENSION_POINT",
    "UNKNOWN_CAPABILITY",
    "UNKNOWN_PERMISSION",
    "CAPABILITY_NOT_DECLARED",
    "PLUGIN_NOT_INSTALLED",
    "PLUGIN_NOT_RUNNING",
    "PLUGIN_NOT_REGISTERED",
    "PLUGIN_STATE_CONFLICT",
    "PLUGIN_OPERATION_CONFLICT",
    "PLUGIN_OPERATION_UNKNOWN",
    "PLUGIN_TASK_TIMEOUT",
    "CONTENT_TOO_LARGE",
    "INVALID_STORAGE_KEY",
    "PLUGIN_STORAGE_QUOTA_EXCEEDED",
    "INVALID_PLUGIN_LIMITS",
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn codes_are_unique_and_stable() {
        let mut seen = BTreeSet::new();
        for code in CODE_TABLE {
            assert!(code.is_ascii());
            assert!(!code.is_empty());
            assert!(seen.insert(code), "duplicate plugin code {code}");
        }
        assert_eq!(seen.len(), CODE_TABLE.len());
    }

    #[test]
    fn only_content_too_large_has_a_frozen_protocol_code() {
        let errors = [
            PluginError::InvalidManifest { detail: "x" },
            PluginError::PluginIdConflict,
            PluginError::SdkIncompatible {
                found: 2,
                supported: 1,
            },
            PluginError::UnsupportedPlatform,
            PluginError::InvalidVersion { detail: "x" },
            PluginError::UnknownExtensionPoint,
            PluginError::UnknownCapability,
            PluginError::UnknownPermission,
            PluginError::CapabilityNotDeclared,
            PluginError::NotInstalled,
            PluginError::NotRunning,
            PluginError::NotRegistered,
            PluginError::RegistrationConflict,
            PluginError::OperationConflict,
            PluginError::OperationUnknown,
            PluginError::TaskTimeout,
            PluginError::ContentTooLarge { size: 9, limit: 8 },
            PluginError::InvalidStorageKey { detail: "x" },
            PluginError::StorageQuotaExceeded { detail: "x" },
            PluginError::InvalidLimits { detail: "x" },
        ];
        for error in errors {
            assert!(!error.retryable(), "{error} must not be retryable");
            assert!(!error.code().is_empty());
            if matches!(error, PluginError::ContentTooLarge { .. }) {
                assert_eq!(error.frozen_code(), Some(ErrorCode::ContentTooLarge));
                assert_eq!(error.code(), ErrorCode::ContentTooLarge.as_str());
            } else {
                assert_eq!(error.frozen_code(), None);
            }
        }
    }
}
