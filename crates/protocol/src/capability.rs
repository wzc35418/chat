//! PROTOCOL-011：连接级能力协商。
//!
//! 冻结语义：
//!
//! - 双方各自声明 schema 版本、支持的协议版本、能力名与限额；
//! - 安全必需能力（`e2ee.v1`、`mls.v1`）不能由远端关闭，缺失即显式失败；
//! - 未知能力名被忽略（保留 + 计数，数量有上限），不参与会话；
//! - 限额一律取双方较小值，绝不放大；
//! - 协商结果是连接级不可变值，不修改身份信任。

use core::fmt;

use crate::canonical::{ProtocolVersion, WireField, WireValue, encode_canonical, field};
use crate::digest::Digest;
use crate::error::{ProtocolError, Result};

/// 能力声明 schema 版本。
pub const CAPABILITY_SCHEMA_VERSION: u16 = 1;
/// 单次声明允许的能力名数量上限。
pub const MAX_CAPABILITY_FEATURES: usize = 32;
/// 单个能力名字节上限。
pub const MAX_CAPABILITY_NAME_BYTES: usize = 64;
/// 单次声明允许的协议版本数量上限。
pub const MAX_DECLARED_VERSIONS: usize = 8;
/// 限额硬上限（与 64 KiB 消息天花板一致）。
pub const MAX_DECLARED_LIMIT: usize = 64 * 1024;
/// 会话指纹用途域。
pub const CAPABILITY_SESSION_DOMAIN: &[u8] = b"chat/p1/capability-session/v1";

/// 安全必需能力：远端缺失即拒绝，不能静默降级。
pub const REQUIRED_FEATURES: [&str; 2] = ["e2ee.v1", "mls.v1"];
/// 本实现认识的能力名集合。集外能力被忽略。
pub const KNOWN_FEATURES: [&str; 6] = [
    "attachments.v1",
    "e2ee.v1",
    "extensions.v1",
    "log.v1",
    "mls.v1",
    "receipts.v1",
];

/// 单条限额三元组。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapabilityLimits {
    /// 单帧字节上限。
    pub max_frame_bytes: usize,
    /// 单条消息完整规范字节上限。
    pub max_message_bytes: usize,
    /// 单条密文字节上限。
    pub max_ciphertext_bytes: usize,
}

impl CapabilityLimits {
    /// 冻结默认限额。
    pub const DEFAULT: CapabilityLimits = CapabilityLimits {
        max_frame_bytes: MAX_DECLARED_LIMIT,
        max_message_bytes: MAX_DECLARED_LIMIT,
        max_ciphertext_bytes: MAX_DECLARED_LIMIT,
    };

    /// 显式构造。
    pub const fn new(
        max_frame_bytes: usize,
        max_message_bytes: usize,
        max_ciphertext_bytes: usize,
    ) -> Self {
        Self {
            max_frame_bytes,
            max_message_bytes,
            max_ciphertext_bytes,
        }
    }

    /// 逐项取较小值。输入已通过声明校验，因此结果必然有界。
    pub fn merge(self, other: CapabilityLimits) -> CapabilityLimits {
        CapabilityLimits {
            max_frame_bytes: self.max_frame_bytes.min(other.max_frame_bytes),
            max_message_bytes: self.max_message_bytes.min(other.max_message_bytes),
            max_ciphertext_bytes: self.max_ciphertext_bytes.min(other.max_ciphertext_bytes),
        }
    }

    fn validate(&self) -> core::result::Result<(), CapabilityError> {
        for (name, value) in [
            ("maxFrameBytes", self.max_frame_bytes),
            ("maxMessageBytes", self.max_message_bytes),
            ("maxCiphertextBytes", self.max_ciphertext_bytes),
        ] {
            if value == 0 || value > MAX_DECLARED_LIMIT {
                return Err(CapabilityError::InvalidLimit { name });
            }
        }
        Ok(())
    }
}

/// 能力声明失败原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityError {
    /// schema 版本不支持。
    UnsupportedSchemaVersion { found: u16 },
    /// 版本列表为空。
    EmptyVersionList,
    /// 版本数量超限。
    TooManyVersions { limit: usize },
    /// 版本号非法（主版本为 0）。
    InvalidVersion { major: u8, minor: u8 },
    /// 版本重复。
    DuplicateVersion { major: u8, minor: u8 },
    /// 能力名数量超限。
    TooManyFeatures { limit: usize },
    /// 能力名为空。
    EmptyFeatureName,
    /// 能力名超长。
    FeatureNameTooLong { limit: usize },
    /// 能力名不是规范形式。
    FeatureNameNotCanonical,
    /// 能力名重复。
    DuplicateFeature,
    /// 限额非法（0 或超过硬上限）。
    InvalidLimit { name: &'static str },
    /// 双方没有共同支持的协议版本。
    NoCommonVersion,
    /// 本机声明缺失安全必需能力。
    MissingLocalRequired { name: &'static str },
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CapabilityError::UnsupportedSchemaVersion { found } => {
                write!(formatter, "unsupported capability schema version {found}")
            }
            CapabilityError::EmptyVersionList => {
                formatter.write_str("no protocol versions declared")
            }
            CapabilityError::TooManyVersions { limit } => {
                write!(formatter, "too many protocol versions (limit {limit})")
            }
            CapabilityError::InvalidVersion { major, minor } => {
                write!(formatter, "invalid protocol version {major}.{minor}")
            }
            CapabilityError::DuplicateVersion { major, minor } => {
                write!(formatter, "duplicate protocol version {major}.{minor}")
            }
            CapabilityError::TooManyFeatures { limit } => {
                write!(formatter, "too many capability names (limit {limit})")
            }
            CapabilityError::EmptyFeatureName => formatter.write_str("empty capability name"),
            CapabilityError::FeatureNameTooLong { limit } => {
                write!(formatter, "capability name exceeds {limit} bytes")
            }
            CapabilityError::FeatureNameNotCanonical => {
                formatter.write_str("capability name is not canonical")
            }
            CapabilityError::DuplicateFeature => formatter.write_str("duplicate capability name"),
            CapabilityError::InvalidLimit { name } => write!(formatter, "invalid limit {name}"),
            CapabilityError::NoCommonVersion => formatter.write_str("no common protocol version"),
            CapabilityError::MissingLocalRequired { name } => {
                write!(
                    formatter,
                    "local declaration misses required capability {name}"
                )
            }
        }
    }
}

/// 单侧能力声明。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// 声明 schema 版本；当前只接受 1。
    pub schema_version: u16,
    /// 支持的协议版本（有序、无重复）。
    pub protocol_versions: Vec<ProtocolVersion>,
    /// 能力名（规范形式、无重复；未知名字允许存在）。
    pub features: Vec<String>,
    /// 单侧限额。
    pub limits: CapabilityLimits,
}

impl Capabilities {
    /// 由版本、能力与限额构造当前 schema 的声明。
    pub fn new(
        protocol_versions: Vec<ProtocolVersion>,
        features: Vec<String>,
        limits: CapabilityLimits,
    ) -> Self {
        Self {
            schema_version: CAPABILITY_SCHEMA_VERSION,
            protocol_versions,
            features,
            limits,
        }
    }

    /// 是否声明了某能力。
    pub fn declares(&self, feature: &str) -> bool {
        self.features.iter().any(|item| item == feature)
    }
}

/// 协商后的会话能力。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCapabilities {
    protocol_version: ProtocolVersion,
    features: Vec<String>,
    limits: CapabilityLimits,
    ignored_feature_count: usize,
}

impl SessionCapabilities {
    /// 协商出的协议版本。
    pub const fn protocol_version(&self) -> ProtocolVersion {
        self.protocol_version
    }

    /// 双方已知能力的交集（排序）。
    pub fn features(&self) -> &[String] {
        &self.features
    }

    /// 合并后的限额（逐项较小值）。
    pub const fn limits(&self) -> CapabilityLimits {
        self.limits
    }

    /// 被忽略的未知能力数量（有界）。
    pub const fn ignored_feature_count(&self) -> usize {
        self.ignored_feature_count
    }

    /// 会话是否具备某能力。
    pub fn supports(&self, feature: &str) -> bool {
        self.features.iter().any(|item| item == feature)
    }

    /// 会话能力指纹：连接级不可变值的确定性摘要，可用于日志关联或对比。
    pub fn fingerprint(&self) -> Digest {
        let body = WireValue::structure([
            WireField::new(
                field::SCHEMA_VERSION,
                WireValue::U64(u64::from(CAPABILITY_SCHEMA_VERSION)),
            ),
            WireField::new(
                field::PROTOCOL_MAJOR,
                WireValue::U64(u64::from(self.protocol_version.major)),
            ),
            WireField::new(
                field::PROTOCOL_MINOR,
                WireValue::U64(u64::from(self.protocol_version.minor)),
            ),
            WireField::new(
                field::SESSION_FEATURES,
                WireValue::List(
                    self.features
                        .iter()
                        .map(|feature| WireValue::Text(feature.clone()))
                        .collect(),
                ),
            ),
            WireField::new(
                field::SESSION_MAX_FRAME_BYTES,
                WireValue::U64(self.limits.max_frame_bytes as u64),
            ),
            WireField::new(
                field::SESSION_MAX_MESSAGE_BYTES,
                WireValue::U64(self.limits.max_message_bytes as u64),
            ),
            WireField::new(
                field::SESSION_MAX_CIPHERTEXT_BYTES,
                WireValue::U64(self.limits.max_ciphertext_bytes as u64),
            ),
        ]);
        let encoded = encode_canonical(&body, ProtocolVersion::V1)
            .expect("capability session body is structurally valid");
        let mut preimage = Vec::with_capacity(CAPABILITY_SESSION_DOMAIN.len() + encoded.len());
        preimage.extend_from_slice(CAPABILITY_SESSION_DOMAIN);
        preimage.extend_from_slice(encoded.as_bytes());
        Digest::sha256(&preimage)
    }
}

fn canonical_feature_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_CAPABILITY_NAME_BYTES {
        return false;
    }
    if !bytes
        .iter()
        .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_'))
    {
        return false;
    }
    // 首字符必须是字母或数字，避免 ".hidden" 之类非规范名。
    matches!(bytes[0], b'a'..=b'z' | b'0'..=b'9')
}

/// 校验单侧声明，返回规范化所需的错误。
fn validate_declaration(declaration: &Capabilities) -> core::result::Result<(), CapabilityError> {
    if declaration.schema_version != CAPABILITY_SCHEMA_VERSION {
        return Err(CapabilityError::UnsupportedSchemaVersion {
            found: declaration.schema_version,
        });
    }
    if declaration.protocol_versions.is_empty() {
        return Err(CapabilityError::EmptyVersionList);
    }
    if declaration.protocol_versions.len() > MAX_DECLARED_VERSIONS {
        return Err(CapabilityError::TooManyVersions {
            limit: MAX_DECLARED_VERSIONS,
        });
    }
    let mut versions: Vec<(u8, u8)> = Vec::with_capacity(declaration.protocol_versions.len());
    for version in &declaration.protocol_versions {
        if version.major == 0 {
            return Err(CapabilityError::InvalidVersion {
                major: version.major,
                minor: version.minor,
            });
        }
        let pair = (version.major, version.minor);
        if versions.contains(&pair) {
            return Err(CapabilityError::DuplicateVersion {
                major: version.major,
                minor: version.minor,
            });
        }
        versions.push(pair);
    }
    if declaration.features.len() > MAX_CAPABILITY_FEATURES {
        return Err(CapabilityError::TooManyFeatures {
            limit: MAX_CAPABILITY_FEATURES,
        });
    }
    let mut names: Vec<&str> = Vec::with_capacity(declaration.features.len());
    for feature in &declaration.features {
        if feature.is_empty() {
            return Err(CapabilityError::EmptyFeatureName);
        }
        if feature.len() > MAX_CAPABILITY_NAME_BYTES {
            return Err(CapabilityError::FeatureNameTooLong {
                limit: MAX_CAPABILITY_NAME_BYTES,
            });
        }
        if !canonical_feature_name(feature) {
            return Err(CapabilityError::FeatureNameNotCanonical);
        }
        if names.contains(&feature.as_str()) {
            return Err(CapabilityError::DuplicateFeature);
        }
        names.push(feature.as_str());
    }
    declaration.limits.validate().map_err(|error| match error {
        CapabilityError::InvalidLimit { name } => CapabilityError::InvalidLimit { name },
        other => other,
    })
}

fn invalid(error: CapabilityError) -> ProtocolError {
    ProtocolError::InvalidCapabilities(error)
}

/// PROTOCOL-011：协商连接级能力。
///
/// 顺序：校验双方声明 → 选择双方共同支持且本实现支持的版本（取最高）→
/// 逐项检查安全必需能力 → 逐项取较小限额 → 固定会话结果。
/// 未知能力被忽略但仍受数量与长度上限约束。
pub fn negotiate_capabilities(
    local: Capabilities,
    remote: Capabilities,
) -> Result<SessionCapabilities> {
    validate_declaration(&local).map_err(invalid)?;
    validate_declaration(&remote).map_err(invalid)?;

    // 版本：双方共同声明且本实现支持，取最高版本。
    let mut common: Vec<ProtocolVersion> = local
        .protocol_versions
        .iter()
        .copied()
        .filter(|version| version.is_supported())
        .filter(|version| remote.protocol_versions.contains(version))
        .collect();
    common.sort_unstable();
    let Some(protocol_version) = common.pop() else {
        return Err(invalid(CapabilityError::NoCommonVersion));
    };

    // 必需能力：本机不能缺，远端不能关闭。
    for required in REQUIRED_FEATURES {
        if !local.declares(required) {
            return Err(invalid(CapabilityError::MissingLocalRequired {
                name: required,
            }));
        }
        if !remote.declares(required) {
            return Err(ProtocolError::RequiredCapabilityMissing);
        }
    }

    // 会话能力：双方都认识且双方都声明的能力，排序去重；未知能力忽略。
    let mut session_features: Vec<String> = Vec::new();
    for feature in &local.features {
        if KNOWN_FEATURES.contains(&feature.as_str()) && remote.declares(feature) {
            session_features.push(feature.clone());
        }
    }
    session_features.sort();
    session_features.dedup();
    let ignored_feature_count = remote
        .features
        .iter()
        .filter(|feature| !KNOWN_FEATURES.contains(&feature.as_str()))
        .count();

    Ok(SessionCapabilities {
        protocol_version,
        features: session_features,
        limits: local.limits.merge(remote.limits),
        ignored_feature_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    fn local() -> Capabilities {
        Capabilities::new(
            vec![ProtocolVersion::V1],
            vec![
                "e2ee.v1".to_owned(),
                "mls.v1".to_owned(),
                "log.v1".to_owned(),
                "attachments.v1".to_owned(),
                "receipts.v1".to_owned(),
            ],
            CapabilityLimits::new(64 * 1024, 64 * 1024, 64 * 1024),
        )
    }

    fn remote() -> Capabilities {
        Capabilities::new(
            vec![ProtocolVersion::V1],
            vec![
                "mls.v1".to_owned(),
                "e2ee.v1".to_owned(),
                "extensions.v1".to_owned(),
            ],
            CapabilityLimits::new(32 * 1024, 16 * 1024, 8 * 1024),
        )
    }

    #[test]
    fn negotiates_required_features_and_smaller_limits() {
        let session = negotiate_capabilities(local(), remote()).unwrap();
        assert_eq!(session.protocol_version(), ProtocolVersion::V1);
        assert!(session.supports("e2ee.v1"));
        assert!(session.supports("mls.v1"));
        assert!(!session.supports("log.v1"), "远端未声明");
        assert_eq!(session.limits().max_frame_bytes, 32 * 1024);
        assert_eq!(session.limits().max_message_bytes, 16 * 1024);
        assert_eq!(session.limits().max_ciphertext_bytes, 8 * 1024);
        assert_eq!(session.ignored_feature_count(), 0);
    }

    #[test]
    fn remote_missing_e2ee_is_rejected() {
        let mut remote = remote();
        remote.features.retain(|feature| feature != "e2ee.v1");
        let error = negotiate_capabilities(local(), remote).unwrap_err();
        assert_eq!(error.code(), ErrorCode::RequiredCapabilityMissing);
        let mut local = local();
        local.features.retain(|feature| feature != "mls.v1");
        assert_eq!(
            negotiate_capabilities(local, remote_ok())
                .unwrap_err()
                .code(),
            ErrorCode::ProtocolUnsupported
        );
    }

    fn remote_ok() -> Capabilities {
        Capabilities::new(
            vec![ProtocolVersion::V1],
            vec!["mls.v1".to_owned(), "e2ee.v1".to_owned()],
            CapabilityLimits::DEFAULT,
        )
    }

    #[test]
    fn no_common_version_is_rejected() {
        let mut remote = remote_ok();
        remote.protocol_versions = vec![ProtocolVersion { major: 2, minor: 0 }];
        assert_eq!(
            negotiate_capabilities(local(), remote).unwrap_err().code(),
            ErrorCode::ProtocolUnsupported
        );
        // 远端同时声明 2.0 与 1.0 时选 1.0，不因未知版本拒绝。
        let mut mixed = remote_ok();
        mixed.protocol_versions = vec![ProtocolVersion { major: 2, minor: 7 }, ProtocolVersion::V1];
        assert_eq!(
            negotiate_capabilities(local(), mixed)
                .unwrap()
                .protocol_version(),
            ProtocolVersion::V1
        );
    }

    #[test]
    fn unknown_features_are_ignored_but_bounded() {
        let mut remote = remote_ok();
        remote.features.push("vendor.experimental".to_owned());
        let session = negotiate_capabilities(local(), remote).unwrap();
        assert_eq!(session.ignored_feature_count(), 1);
        assert!(
            session
                .features()
                .iter()
                .all(|f| f != "vendor.experimental")
        );

        // 数量上限。
        let mut too_many = remote_ok();
        too_many.features = (0..MAX_CAPABILITY_FEATURES + 1)
            .map(|index| format!("x{index}"))
            .collect();
        assert_eq!(
            negotiate_capabilities(local(), too_many)
                .unwrap_err()
                .code(),
            ErrorCode::ProtocolUnsupported
        );
        // 超长能力名。
        let mut too_long = remote_ok();
        too_long
            .features
            .push("a".repeat(MAX_CAPABILITY_NAME_BYTES + 1));
        assert!(negotiate_capabilities(local(), too_long).is_err());
        // 非规范（大写）。
        let mut non_canonical = remote_ok();
        non_canonical.features.push("E2EE.v1".to_owned());
        assert!(negotiate_capabilities(local(), non_canonical).is_err());
        // 重复能力名。
        let mut duplicate = remote_ok();
        duplicate.features.push("e2ee.v1".to_owned());
        assert!(negotiate_capabilities(local(), duplicate).is_err());
    }

    #[test]
    fn malformed_declarations_are_rejected() {
        let mut wrong_schema = remote_ok();
        wrong_schema.schema_version = 2;
        assert_eq!(
            negotiate_capabilities(local(), wrong_schema)
                .unwrap_err()
                .code(),
            ErrorCode::ProtocolUnsupported
        );
        let mut empty_versions = remote_ok();
        empty_versions.protocol_versions.clear();
        assert!(negotiate_capabilities(local(), empty_versions).is_err());
        let mut duplicate_version = remote_ok();
        duplicate_version
            .protocol_versions
            .push(ProtocolVersion::V1);
        assert!(negotiate_capabilities(local(), duplicate_version).is_err());
        let mut too_many_versions = remote_ok();
        too_many_versions.protocol_versions = (0..MAX_DECLARED_VERSIONS + 1)
            .map(|minor| ProtocolVersion {
                major: 1,
                minor: minor as u8,
            })
            .collect();
        assert!(negotiate_capabilities(local(), too_many_versions).is_err());
        let mut zero_version = remote_ok();
        zero_version.protocol_versions = vec![ProtocolVersion { major: 0, minor: 1 }];
        assert!(negotiate_capabilities(local(), zero_version).is_err());
        // 0 与超过硬上限的限额都拒绝（不放大、不静默取默认值）。
        for limits in [
            CapabilityLimits::new(0, 1024, 1024),
            CapabilityLimits::new(1024, MAX_DECLARED_LIMIT + 1, 1024),
            CapabilityLimits::new(1024, 1024, usize::MAX),
        ] {
            let mut bad = remote_ok();
            bad.limits = limits;
            assert!(negotiate_capabilities(local(), bad).is_err());
        }
        // 本地声明同样校验。
        let mut bad_local = local();
        bad_local.limits = CapabilityLimits::new(0, 1, 1);
        assert!(negotiate_capabilities(bad_local, remote_ok()).is_err());
    }

    #[test]
    fn session_fingerprint_is_deterministic_and_limit_sensitive() {
        let first = negotiate_capabilities(local(), remote())
            .unwrap()
            .fingerprint();
        let second = negotiate_capabilities(local(), remote())
            .unwrap()
            .fingerprint();
        assert_eq!(first, second);
        let mut smaller = remote();
        smaller.limits = CapabilityLimits::new(1, 1, 1);
        let changed = negotiate_capabilities(local(), smaller)
            .unwrap()
            .fingerprint();
        assert_ne!(first, changed);
    }
}
