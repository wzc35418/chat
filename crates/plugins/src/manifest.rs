//! PLUGINS-001 的纯逻辑子集：插件清单解析与校验。
//!
//! 本模块只做声明式校验（不读 ZIP、不初始化 ClassLoader、不执行代码）：
//! 插件 ID、版本、SDK/平台、所需能力、可注册扩展点、权限声明与入口类名。
//! 重复 ID 由 [`crate::host::PluginHost`] 安装时拒绝（需要已安装集合）。
//!
//! JSON 清单字段（`deny_unknown_fields`，重复字段由 serde 拒绝）：
//!
//! ```json
//! {
//!   "schemaVersion": 1,
//!   "id": "notes.v1",
//!   "version": "1.2.3",
//!   "sdkVersion": 1,
//!   "platform": "desktop",
//!   "capabilities": ["PrivateStorage", "Command"],
//!   "permissions": ["UserSelectedContent"],
//!   "extensionPoints": ["Command"],
//!   "entryClass": "com.example.notes.NotesPlugin"
//! }
//! ```

use core::fmt;
use std::collections::BTreeSet;

use serde::Deserialize;

use crate::error::{PluginError, Result};
use crate::limits::PluginLimits;

/// 当前清单 schema 版本。
pub const CURRENT_MANIFEST_SCHEMA: u16 = 1;
/// 当前宿主 SDK 版本。
pub const CURRENT_SDK_VERSION: u16 = 1;
/// 插件 ID 字节上限。
pub const MAX_PLUGIN_ID_BYTES: usize = 64;
/// 入口类名字节上限。
pub const MAX_ENTRY_CLASS_BYTES: usize = 256;
/// 单清单权限声明数量上限。
pub const MAX_PERMISSIONS: usize = 8;

/// 规范插件 ID（小写点分，例如 `notes.v1`）。
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PluginId(String);

impl PluginId {
    /// 解析并校验插件 ID。
    ///
    /// 规则：1..=64 字节 ASCII；2..=4 段；每段 1..=32 字节，首字符小写字母，
    /// 其余为 `[a-z0-9-]` 且不以 `-` 结尾；首段不能是保留命名空间
    /// `core`/`sys`/`chat`。
    pub fn parse(input: &str) -> Result<Self> {
        let bytes = input.as_bytes();
        if bytes.is_empty() || bytes.len() > MAX_PLUGIN_ID_BYTES {
            return Err(PluginError::InvalidManifest {
                detail: "plugin id length",
            });
        }
        if !input.is_ascii() {
            return Err(PluginError::InvalidManifest {
                detail: "plugin id must be ASCII",
            });
        }
        let segments: Vec<&str> = input.split('.').collect();
        if !(2..=4).contains(&segments.len()) {
            return Err(PluginError::InvalidManifest {
                detail: "plugin id must have 2..=4 dot-separated segments",
            });
        }
        for segment in &segments {
            let segment_bytes = segment.as_bytes();
            if segment_bytes.is_empty() || segment_bytes.len() > 32 {
                return Err(PluginError::InvalidManifest {
                    detail: "plugin id segment length",
                });
            }
            if !segment_bytes[0].is_ascii_lowercase() {
                return Err(PluginError::InvalidManifest {
                    detail: "plugin id segment must start with a lowercase letter",
                });
            }
            if !segment_bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
            {
                return Err(PluginError::InvalidManifest {
                    detail: "plugin id segment characters",
                });
            }
            if segment_bytes[segment_bytes.len() - 1] == b'-' {
                return Err(PluginError::InvalidManifest {
                    detail: "plugin id segment must not end with '-'",
                });
            }
        }
        if matches!(segments[0], "core" | "sys" | "chat") {
            return Err(PluginError::InvalidManifest {
                detail: "plugin id uses a reserved namespace",
            });
        }
        Ok(Self(input.to_owned()))
    }

    /// 文本形式。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PluginId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PluginId({})", self.0)
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// 语义化版本（三段十进制）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PluginVersion {
    /// 主版本。
    pub major: u16,
    /// 次版本。
    pub minor: u16,
    /// 补丁版本。
    pub patch: u16,
}

impl PluginVersion {
    /// 构造版本。
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// 解析 `MAJOR.MINOR.PATCH`：恰好三段、无前导零、每段 ≤ 65535。
    pub fn parse(input: &str) -> Result<Self> {
        if input.is_empty() || input.len() > 24 || !input.is_ascii() {
            return Err(PluginError::InvalidVersion {
                detail: "version length",
            });
        }
        let parts: Vec<&str> = input.split('.').collect();
        if parts.len() != 3 {
            return Err(PluginError::InvalidVersion {
                detail: "version must have three components",
            });
        }
        let mut numbers = [0u16; 3];
        for (index, part) in parts.iter().enumerate() {
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(PluginError::InvalidVersion {
                    detail: "version component is not numeric",
                });
            }
            if part.len() > 1 && part.starts_with('0') {
                return Err(PluginError::InvalidVersion {
                    detail: "version component has a leading zero",
                });
            }
            numbers[index] = part
                .parse::<u16>()
                .map_err(|_| PluginError::InvalidVersion {
                    detail: "version component exceeds 65535",
                })?;
        }
        Ok(Self::new(numbers[0], numbers[1], numbers[2]))
    }
}

impl fmt::Display for PluginVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// 宿主平台。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginPlatform {
    /// 桌面宿主（MVP 唯一支持）。
    Desktop,
}

impl PluginPlatform {
    /// 从清单字符串解析；未知平台返回 `UNSUPPORTED_PLATFORM`。
    pub fn parse(input: &str) -> Result<Self> {
        match input {
            "desktop" => Ok(PluginPlatform::Desktop),
            _ => Err(PluginError::UnsupportedPlatform),
        }
    }

    /// 清单字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            PluginPlatform::Desktop => "desktop",
        }
    }
}

/// 插件所需能力。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PluginCapability {
    /// 插件私有键值存储。
    PrivateStorage,
    /// 注册命令。
    Command,
    /// 注册消息操作。
    MessageAction,
    /// 注册设置面板。
    SettingsPanel,
}

impl PluginCapability {
    /// 全量能力。
    pub const ALL: [PluginCapability; 4] = [
        PluginCapability::PrivateStorage,
        PluginCapability::Command,
        PluginCapability::MessageAction,
        PluginCapability::SettingsPanel,
    ];

    /// 从清单字符串解析（大小写敏感）。
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "PrivateStorage" => Some(PluginCapability::PrivateStorage),
            "Command" => Some(PluginCapability::Command),
            "MessageAction" => Some(PluginCapability::MessageAction),
            "SettingsPanel" => Some(PluginCapability::SettingsPanel),
            _ => None,
        }
    }

    /// 清单字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            PluginCapability::PrivateStorage => "PrivateStorage",
            PluginCapability::Command => "Command",
            PluginCapability::MessageAction => "MessageAction",
            PluginCapability::SettingsPanel => "SettingsPanel",
        }
    }
}

/// 插件权限声明（清单 `permissions` 字段）。
///
/// 两类值共存：
/// - **沙箱权限**（JVM 侧按这些限制类加载/系统调用）：`network` / `filesystem` / `process` /
///   `host-ui`；
/// - **既有用户可见权限**（P4 早期就有，保持向后兼容）：`UserSelectedContent` / `ComposeDraft`。
///
/// 解析**大小写敏感**、按清单顺序去重、最多 [`MAX_PERMISSIONS`] 项；未知值 → 显式错误（绝不忽略）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PluginPermission {
    /// 读取用户显式选择的内容。
    UserSelectedContent,
    /// 向编辑区提出草稿建议。
    ComposeDraft,
    /// 沙箱：允许发起网络访问。
    Network,
    /// 沙箱：允许读写插件私有目录之外的文件系统。
    Filesystem,
    /// 沙箱：允许启动子进程。
    Process,
    /// 沙箱：允许扩展宿主 UI（面板/命令之外的界面元素）。
    HostUi,
}

impl PluginPermission {
    /// 全量权限。
    pub const ALL: [PluginPermission; 6] = [
        PluginPermission::UserSelectedContent,
        PluginPermission::ComposeDraft,
        PluginPermission::Network,
        PluginPermission::Filesystem,
        PluginPermission::Process,
        PluginPermission::HostUi,
    ];

    /// 四个**沙箱**权限（JVM 侧沙箱最小可行版只认这四个）。
    pub const SANDBOX: [PluginPermission; 4] = [
        PluginPermission::Network,
        PluginPermission::Filesystem,
        PluginPermission::Process,
        PluginPermission::HostUi,
    ];

    /// 是否属于沙箱权限。
    pub const fn is_sandbox(self) -> bool {
        matches!(
            self,
            PluginPermission::Network
                | PluginPermission::Filesystem
                | PluginPermission::Process
                | PluginPermission::HostUi
        )
    }

    /// 从清单字符串解析（**大小写敏感**）。
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "UserSelectedContent" => Some(PluginPermission::UserSelectedContent),
            "ComposeDraft" => Some(PluginPermission::ComposeDraft),
            "network" => Some(PluginPermission::Network),
            "filesystem" => Some(PluginPermission::Filesystem),
            "process" => Some(PluginPermission::Process),
            "host-ui" => Some(PluginPermission::HostUi),
            _ => None,
        }
    }

    /// 清单字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            PluginPermission::UserSelectedContent => "UserSelectedContent",
            PluginPermission::ComposeDraft => "ComposeDraft",
            PluginPermission::Network => "network",
            PluginPermission::Filesystem => "filesystem",
            PluginPermission::Process => "process",
            PluginPermission::HostUi => "host-ui",
        }
    }
}

/// 可注册扩展点。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExtensionPoint {
    /// 命令入口。
    Command,
    /// 消息操作。
    MessageAction,
    /// 设置面板。
    SettingsPanel,
}

impl ExtensionPoint {
    /// 全量扩展点。
    pub const ALL: [ExtensionPoint; 3] = [
        ExtensionPoint::Command,
        ExtensionPoint::MessageAction,
        ExtensionPoint::SettingsPanel,
    ];

    /// 从清单字符串解析。
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "Command" => Some(ExtensionPoint::Command),
            "MessageAction" => Some(ExtensionPoint::MessageAction),
            "SettingsPanel" => Some(ExtensionPoint::SettingsPanel),
            _ => None,
        }
    }

    /// 清单字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            ExtensionPoint::Command => "Command",
            ExtensionPoint::MessageAction => "MessageAction",
            ExtensionPoint::SettingsPanel => "SettingsPanel",
        }
    }

    /// 注册该扩展点所需的能力。
    pub const fn required_capability(self) -> PluginCapability {
        match self {
            ExtensionPoint::Command => PluginCapability::Command,
            ExtensionPoint::MessageAction => PluginCapability::MessageAction,
            ExtensionPoint::SettingsPanel => PluginCapability::SettingsPanel,
        }
    }
}

/// 校验通过、不可变的插件清单。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginManifest {
    /// 清单 schema 版本。
    pub schema_version: u16,
    /// 规范插件 ID。
    pub id: PluginId,
    /// 插件版本。
    pub version: PluginVersion,
    /// SDK 版本。
    pub sdk_version: u16,
    /// 目标平台。
    pub platform: PluginPlatform,
    /// 入口类名（不含路径）。
    pub entry_class: String,
    /// 所需能力。
    pub capabilities: BTreeSet<PluginCapability>,
    /// 权限声明。
    pub permissions: BTreeSet<PluginPermission>,
    /// 可注册扩展点。
    pub extension_points: BTreeSet<ExtensionPoint>,
}

impl PluginManifest {
    /// 是否声明了某能力。
    pub fn has_capability(&self, capability: PluginCapability) -> bool {
        self.capabilities.contains(&capability)
    }

    /// 是否声明了某权限。
    pub fn has_permission(&self, permission: PluginPermission) -> bool {
        self.permissions.contains(&permission)
    }

    /// 是否允许注册某扩展点（清单声明 + 所需能力）。
    pub fn permits_extension_point(&self, point: ExtensionPoint) -> bool {
        self.extension_points.contains(&point) && self.has_capability(point.required_capability())
    }
}

fn validate_entry_class(input: &str) -> Result<()> {
    let bytes = input.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_ENTRY_CLASS_BYTES {
        return Err(PluginError::InvalidManifest {
            detail: "entry class length",
        });
    }
    if !input.is_ascii() {
        return Err(PluginError::InvalidManifest {
            detail: "entry class must be ASCII",
        });
    }
    if input.starts_with('.')
        || input.ends_with('.')
        || input.contains("..")
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'$'))
    {
        return Err(PluginError::InvalidManifest {
            detail: "entry class must be a dotted Java class name",
        });
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawManifest {
    schema_version: u16,
    id: String,
    version: String,
    sdk_version: u16,
    platform: String,
    capabilities: Vec<String>,
    /// 可选：不写 = 无敏感权限（旧清单照常通过）。
    #[serde(default)]
    permissions: Vec<String>,
    extension_points: Vec<String>,
    entry_class: String,
}

/// 解析并校验清单 JSON。
///
/// 顺序：输入长度 → JSON 结构（未知/重复字段拒绝）→ schema 版本 → ID/版本 →
/// SDK/平台 → 能力/权限/扩展点（数量、重复、未知、缺能力）→ 入口类名。
/// 本函数不读文件、不解压、不执行任何插件代码。
pub fn parse_manifest(json: &str, limits: &PluginLimits) -> Result<PluginManifest> {
    limits.validate()?;
    if json.len() > limits.max_manifest_bytes {
        return Err(PluginError::ContentTooLarge {
            size: json.len(),
            limit: limits.max_manifest_bytes,
        });
    }
    let raw: RawManifest =
        serde_json::from_str(json).map_err(|_| PluginError::InvalidManifest {
            detail: "manifest is not valid JSON for schema 1",
        })?;
    if raw.schema_version != CURRENT_MANIFEST_SCHEMA {
        return Err(PluginError::InvalidManifest {
            detail: "unsupported manifest schema version",
        });
    }
    let id = PluginId::parse(&raw.id)?;
    let version = PluginVersion::parse(&raw.version)?;
    if raw.sdk_version != CURRENT_SDK_VERSION {
        return Err(PluginError::SdkIncompatible {
            found: raw.sdk_version,
            supported: CURRENT_SDK_VERSION,
        });
    }
    let platform = PluginPlatform::parse(&raw.platform)?;

    if raw.capabilities.len() > limits.max_capabilities {
        return Err(PluginError::InvalidManifest {
            detail: "too many capability declarations",
        });
    }
    let mut capabilities = BTreeSet::new();
    for capability in &raw.capabilities {
        let parsed = PluginCapability::parse(capability).ok_or(PluginError::UnknownCapability)?;
        if !capabilities.insert(parsed) {
            return Err(PluginError::InvalidManifest {
                detail: "duplicate capability declaration",
            });
        }
    }

    if raw.permissions.len() > MAX_PERMISSIONS {
        return Err(PluginError::InvalidManifest {
            detail: "too many permission declarations",
        });
    }
    let mut permissions = BTreeSet::new();
    for permission in &raw.permissions {
        // 未知/大小写不符的值一律按**清单非法**拒绝（绝不忽略）。
        let Some(parsed) = PluginPermission::parse(permission) else {
            return Err(PluginError::InvalidManifest {
                detail: "unknown permission declaration",
            });
        };
        if !permissions.insert(parsed) {
            return Err(PluginError::InvalidManifest {
                detail: "duplicate permission declaration",
            });
        }
    }

    if raw.extension_points.len() > limits.max_extension_points {
        return Err(PluginError::InvalidManifest {
            detail: "too many extension point declarations",
        });
    }
    let mut extension_points = BTreeSet::new();
    for point in &raw.extension_points {
        let parsed = ExtensionPoint::parse(point).ok_or(PluginError::UnknownExtensionPoint)?;
        if !extension_points.insert(parsed) {
            return Err(PluginError::InvalidManifest {
                detail: "duplicate extension point declaration",
            });
        }
    }
    // 缺能力显式拒绝：扩展点必须同时声明所需能力。
    for point in &extension_points {
        if !capabilities.contains(&point.required_capability()) {
            return Err(PluginError::CapabilityNotDeclared);
        }
    }

    validate_entry_class(&raw.entry_class)?;

    Ok(PluginManifest {
        schema_version: raw.schema_version,
        id,
        version,
        sdk_version: raw.sdk_version,
        platform,
        entry_class: raw.entry_class,
        capabilities,
        permissions,
        extension_points,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_json(capabilities: &str, points: &str) -> String {
        format!(
            r#"{{"schemaVersion":1,"id":"notes.v1","version":"1.2.3","sdkVersion":1,"platform":"desktop","capabilities":{capabilities},"permissions":["UserSelectedContent"],"extensionPoints":{points},"entryClass":"com.example.notes.NotesPlugin"}}"#
        )
    }

    #[test]
    fn parses_valid_manifest() {
        let manifest = parse_manifest(
            &manifest_json(r#"["PrivateStorage","Command"]"#, r#"["Command"]"#),
            &PluginLimits::DEFAULT,
        )
        .unwrap();
        assert_eq!(manifest.id.as_str(), "notes.v1");
        assert_eq!(manifest.version, PluginVersion::new(1, 2, 3));
        assert_eq!(manifest.sdk_version, CURRENT_SDK_VERSION);
        assert_eq!(manifest.platform, PluginPlatform::Desktop);
        assert_eq!(manifest.entry_class, "com.example.notes.NotesPlugin");
        assert!(manifest.has_capability(PluginCapability::PrivateStorage));
        assert!(manifest.has_capability(PluginCapability::Command));
        assert!(manifest.has_permission(PluginPermission::UserSelectedContent));
        assert!(manifest.permits_extension_point(ExtensionPoint::Command));
        assert!(!manifest.permits_extension_point(ExtensionPoint::MessageAction));
    }

    #[test]
    fn rejects_unknown_and_duplicate_fields_and_bad_json() {
        let unknown_field = manifest_json(r#"["Command"]"#, r#"["Command"]"#).replace(
            r#""entryClass""#,
            r#""entryClass":"com.example.notes.NotesPlugin","unexpected":true,"entryClass""#,
        );
        assert_eq!(
            parse_manifest(&unknown_field, &PluginLimits::DEFAULT)
                .unwrap_err()
                .code(),
            "INVALID_PLUGIN_PACKAGE"
        );
        let duplicate_field = manifest_json(r#"["Command"]"#, r#"["Command"]"#)
            .replace(r#""id":"notes.v1""#, r#""id":"notes.v1","id":"notes.v2""#);
        assert!(parse_manifest(&duplicate_field, &PluginLimits::DEFAULT).is_err());
        assert!(parse_manifest("{", &PluginLimits::DEFAULT).is_err());
        // 超长清单在解析前拒绝。
        let mut limits = PluginLimits::DEFAULT;
        limits.max_manifest_bytes = 8;
        assert_eq!(
            parse_manifest(&manifest_json("[]", "[]"), &limits)
                .unwrap_err()
                .code(),
            "CONTENT_TOO_LARGE"
        );
    }

    #[test]
    fn rejects_invalid_ids() {
        for id in [
            "",
            "notes",
            "notes.v1.extra.parts.here",
            "Notes.V1",
            "notes..v1",
            "notes.v1-",
            "notes.-v1",
            "core.plugin",
            "sys.plugin",
            "chat.plugin",
            &"a".repeat(MAX_PLUGIN_ID_BYTES + 1),
        ] {
            let json = manifest_json(r#"["Command"]"#, r#"["Command"]"#)
                .replace(r#""id":"notes.v1""#, &format!(r#""id":"{id}""#));
            assert!(
                parse_manifest(&json, &PluginLimits::DEFAULT).is_err(),
                "id {id:?} must be rejected"
            );
        }
        assert!(PluginId::parse("notes.v1").is_ok());
        assert!(PluginId::parse("a.b").is_ok());
    }

    #[test]
    fn rejects_invalid_versions() {
        for version in [
            "1.2", "1.2.3.4", "1.02.3", "01.2.3", "1.2.x", "1.2.-3", "", "a.b.c",
        ] {
            let json = manifest_json(r#"["Command"]"#, r#"["Command"]"#)
                .replace(r#""version":"1.2.3""#, &format!(r#""version":"{version}""#));
            assert!(
                parse_manifest(&json, &PluginLimits::DEFAULT).is_err(),
                "version {version:?} must be rejected"
            );
        }
        let json = manifest_json(r#"["Command"]"#, r#"["Command"]"#)
            .replace(r#""version":"1.2.3""#, r#""version":"65536.0.0""#);
        assert_eq!(
            parse_manifest(&json, &PluginLimits::DEFAULT)
                .unwrap_err()
                .code(),
            "INVALID_PLUGIN_VERSION"
        );
        assert_eq!(
            PluginVersion::parse("1.2.3").unwrap(),
            PluginVersion::new(1, 2, 3)
        );
        assert_eq!(
            PluginVersion::parse("0.0.0").unwrap(),
            PluginVersion::new(0, 0, 0)
        );
    }

    #[test]
    fn rejects_incompatible_sdk_and_platform() {
        let json = manifest_json(r#"["Command"]"#, r#"["Command"]"#)
            .replace(r#""sdkVersion":1"#, r#""sdkVersion":2"#);
        assert_eq!(
            parse_manifest(&json, &PluginLimits::DEFAULT)
                .unwrap_err()
                .code(),
            "SDK_INCOMPATIBLE"
        );
        let json = manifest_json(r#"["Command"]"#, r#"["Command"]"#)
            .replace(r#""platform":"desktop""#, r#""platform":"android""#);
        assert_eq!(
            parse_manifest(&json, &PluginLimits::DEFAULT)
                .unwrap_err()
                .code(),
            "UNSUPPORTED_PLATFORM"
        );
    }

    #[test]
    fn rejects_unknown_and_duplicate_declarations_and_missing_capability() {
        let unknown_capability = manifest_json(r#"["Command","Teleport"]"#, r#"["Command"]"#);
        assert_eq!(
            parse_manifest(&unknown_capability, &PluginLimits::DEFAULT)
                .unwrap_err()
                .code(),
            "UNKNOWN_CAPABILITY"
        );
        let unknown_permission = manifest_json(r#"["Command"]"#, r#"["Command"]"#).replace(
            r#"["UserSelectedContent"]"#,
            r#"["UserSelectedContent","Filesystem"]"#,
        );
        // TASK-129：未知/大小写不符的权限值按**清单非法**拒绝（本模块冻结码 INVALID_PLUGIN_PACKAGE）。
        assert_eq!(
            parse_manifest(&unknown_permission, &PluginLimits::DEFAULT)
                .unwrap_err()
                .code(),
            "INVALID_PLUGIN_PACKAGE"
        );
        let unknown_point = manifest_json(r#"["Command"]"#, r#"["Command","NetworkHook"]"#);
        assert_eq!(
            parse_manifest(&unknown_point, &PluginLimits::DEFAULT)
                .unwrap_err()
                .code(),
            "UNKNOWN_EXTENSION_POINT"
        );
        let duplicate_point = manifest_json(r#"["Command"]"#, r#"["Command","Command"]"#);
        assert_eq!(
            parse_manifest(&duplicate_point, &PluginLimits::DEFAULT)
                .unwrap_err()
                .code(),
            "INVALID_PLUGIN_PACKAGE"
        );
        // 声明扩展点但没有所需能力。
        let missing_capability = manifest_json(r#"["PrivateStorage"]"#, r#"["Command"]"#);
        assert_eq!(
            parse_manifest(&missing_capability, &PluginLimits::DEFAULT)
                .unwrap_err()
                .code(),
            "CAPABILITY_NOT_DECLARED"
        );
        // 能力数量上限。
        let mut limits = PluginLimits::DEFAULT;
        limits.max_capabilities = 1;
        let too_many = manifest_json(r#"["Command","MessageAction"]"#, r#"["Command"]"#);
        assert!(parse_manifest(&too_many, &limits).is_err());
    }

    #[test]
    fn rejects_unsafe_entry_class() {
        for entry in [
            "",
            "/abs/Path",
            "com/example/Plugin",
            "com..Plugin",
            ".Plugin",
            "Plugin.",
            "com.例子",
        ] {
            let json = manifest_json(r#"["Command"]"#, r#"["Command"]"#)
                .replace(r#"com.example.notes.NotesPlugin"#, entry);
            assert!(
                parse_manifest(&json, &PluginLimits::DEFAULT).is_err(),
                "entry class {entry:?} must be rejected"
            );
        }
        assert!(validate_entry_class("a.B$C_d.1").is_ok());
    }

    #[test]
    fn extension_point_capability_mapping_is_frozen() {
        assert_eq!(
            ExtensionPoint::Command.required_capability(),
            PluginCapability::Command
        );
        assert_eq!(
            ExtensionPoint::MessageAction.required_capability(),
            PluginCapability::MessageAction
        );
        assert_eq!(
            ExtensionPoint::SettingsPanel.required_capability(),
            PluginCapability::SettingsPanel
        );
        assert_eq!(PluginCapability::ALL.len(), 4);
        // 权限白名单：2 个既有用户可见权限 + 4 个沙箱权限（TASK-129）。
        assert_eq!(PluginPermission::ALL.len(), 6);
        assert_eq!(PluginPermission::SANDBOX.len(), 4);
        assert_eq!(ExtensionPoint::ALL.len(), 3);
    }
}
