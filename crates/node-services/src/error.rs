//! 节点稳定错误码。
//!
//! 所有安全校验失败都返回显式错误，不返回“看起来成功”的结果；
//! 错误码是失败契约的一部分，HTTP 层只做映射（见 crates/node-api/src/error.rs）。
use std::fmt;

/// 节点服务统一错误类型。
#[derive(Debug)]
pub enum NodeError {
    /// 请求形状/字段不合法（含超长、非法字符、非法十六进制）。
    InvalidRequest(&'static str),
    /// 请求正文超过上限。
    PayloadTooLarge,
    /// 未知、已用尽保留期或与节点不匹配的挑战。
    InvalidChallenge,
    /// 挑战绑定的节点不是本节点。
    NodeIdMismatch,
    /// 挑战已过期。
    AuthExpired,
    /// 同一挑战被第二次消费。
    AuthReplay,
    /// 设备签名验证失败（含跨方法/路径/查询/正文/节点重放）。
    InvalidSignature,
    /// 设备凭据无效：签名、账户绑定、期限、supervisor 公钥或已登记数据不一致。
    InvalidCredential,
    /// 设备已撤销。
    DeviceRevoked,
    /// 账户有效设备达到上限。
    DeviceLimitExceeded,
    /// 注册能力无效：范围、期限、签名或撤销版本。
    InvalidCapability,
    /// 请求证明与请求目标不匹配（跨设备/跨账户）。
    PermissionDenied,
    /// 未消费挑战数量达到全局容量上限。
    ChallengeCapacityExceeded,
    /// 限流拒绝（本阶段仅保留错误码，逐 IP 限流属 API-005）。
    RateLimited,
    /// 群不存在或不属于本节点协调。
    GroupNotFound,
    /// 同一 GroupId 已用相异创建内容登记。
    GroupIdConflict,
    /// 单群成员数超过节点执行的上限。
    GroupCapacityExceeded,
    /// 同一 SubmissionId 已用相异候选登记。
    SubmissionIdConflict,
    /// 候选声明的 control head 或 policy 版本不是当前值。
    StaleControlHead,
    /// 候选声明的 MLS epoch 不是当前值。
    StaleEpoch,
    /// 设备不在该群允许的历史/成员窗口内。
    HistoryNotAuthorized,
    /// 分页游标非法、过期或不属于当前设备。
    InvalidCursor,
    /// 目标设备/账户的持久配额已满。
    QuotaExceeded,
    /// 同一信箱 item 已用相异密文投递。
    ItemIdConflict,
    /// ACK 批量非法（空、超上限或含重复之外的畸形标识）。
    InvalidAckBatch,
    /// 迁移执行或校验失败。
    MigrationFailed(&'static str),
    /// 持久化节点身份与配置不一致。
    NodeIdentityMismatch,
    /// 节点配置不合法。
    InvalidConfig(&'static str),
    /// 存储暂时不可用（锁中毒、SQLite IO）。
    StorageUnavailable,
    /// SQLite 错误。
    Storage(rusqlite::Error),
    /// JSON 编解码错误。
    Encoding(serde_json::Error),
}

impl NodeError {
    /// 稳定错误码，供 HTTP 层与客户端按码分支。
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "INVALID_REQUEST",
            Self::PayloadTooLarge => "PAYLOAD_TOO_LARGE",
            Self::InvalidChallenge => "INVALID_CHALLENGE",
            Self::NodeIdMismatch => "NODE_ID_MISMATCH",
            Self::AuthExpired => "AUTH_EXPIRED",
            Self::AuthReplay => "AUTH_REPLAY",
            Self::InvalidSignature => "INVALID_SIGNATURE",
            Self::InvalidCredential => "INVALID_CREDENTIAL",
            Self::DeviceRevoked => "DEVICE_REVOKED",
            Self::DeviceLimitExceeded => "DEVICE_LIMIT_EXCEEDED",
            Self::InvalidCapability => "INVALID_CAPABILITY",
            Self::PermissionDenied => "PERMISSION_DENIED",
            Self::ChallengeCapacityExceeded => "CHALLENGE_CAPACITY_EXCEEDED",
            Self::RateLimited => "RATE_LIMITED",
            Self::GroupNotFound => "GROUP_NOT_FOUND",
            Self::GroupIdConflict => "GROUP_ID_CONFLICT",
            Self::GroupCapacityExceeded => "GROUP_CAPACITY_EXCEEDED",
            Self::SubmissionIdConflict => "SUBMISSION_ID_CONFLICT",
            Self::StaleControlHead => "STALE_CONTROL_HEAD",
            Self::StaleEpoch => "STALE_EPOCH",
            Self::HistoryNotAuthorized => "HISTORY_NOT_AUTHORIZED",
            Self::InvalidCursor => "INVALID_CURSOR",
            Self::QuotaExceeded => "QUOTA_EXCEEDED",
            Self::ItemIdConflict => "ITEM_ID_CONFLICT",
            Self::InvalidAckBatch => "INVALID_ACK_BATCH",
            Self::MigrationFailed(_) => "MIGRATION_FAILED",
            Self::NodeIdentityMismatch => "NODE_IDENTITY_MISMATCH",
            Self::InvalidConfig(_) => "INVALID_CONFIG",
            Self::StorageUnavailable => "STORAGE_FAILURE",
            Self::Storage(_) => "STORAGE_FAILURE",
            Self::Encoding(_) => "ENCODING_FAILURE",
        }
    }

    /// 面向客户端的固定短说明；不包含内部细节、密钥或正文。
    pub fn safe_message(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "request is malformed or out of bounds",
            Self::PayloadTooLarge => "request body exceeds the node limit",
            Self::InvalidChallenge => "challenge is unknown or not usable",
            Self::NodeIdMismatch => "challenge belongs to another node",
            Self::AuthExpired => "challenge has expired",
            Self::AuthReplay => "challenge was already used",
            Self::InvalidSignature => "device signature does not match this request",
            Self::InvalidCredential => "device credential is not valid",
            Self::DeviceRevoked => "device is revoked",
            Self::DeviceLimitExceeded => "account device limit reached",
            Self::InvalidCapability => "registration capability is not valid",
            Self::PermissionDenied => "request is not authorized for this target",
            Self::ChallengeCapacityExceeded => "node challenge capacity is exhausted",
            Self::RateLimited => "rate limited",
            Self::GroupNotFound => "group is unknown to this node",
            Self::GroupIdConflict => "group id was already registered with other content",
            Self::GroupCapacityExceeded => "group member capacity exceeded",
            Self::SubmissionIdConflict => "submission id was already used with other bytes",
            Self::StaleControlHead => "candidate control head or policy version is stale",
            Self::StaleEpoch => "candidate mls epoch is stale",
            Self::HistoryNotAuthorized => "device is not authorized for this group history",
            Self::InvalidCursor => "page cursor is invalid or not issued to this device",
            Self::QuotaExceeded => "target quota is exhausted",
            Self::ItemIdConflict => "mailbox item id was already used with other bytes",
            Self::InvalidAckBatch => "acknowledgement batch is empty or out of bounds",
            Self::MigrationFailed(_) => "node storage migration failed",
            Self::NodeIdentityMismatch => "stored node identity differs from configuration",
            Self::InvalidConfig(_) => "node configuration is not valid",
            Self::StorageUnavailable => "node storage is unavailable",
            Self::Storage(_) => "node storage failure",
            Self::Encoding(_) => "payload encoding failure",
        }
    }
}

impl fmt::Display for NodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.safe_message())?;
        match self {
            Self::InvalidRequest(detail) => write!(formatter, " ({detail})"),
            Self::MigrationFailed(detail) => write!(formatter, " ({detail})"),
            Self::InvalidConfig(detail) => write!(formatter, " ({detail})"),
            Self::Storage(error) => write!(formatter, " ({error})"),
            Self::Encoding(error) => write!(formatter, " ({error})"),
            _ => Ok(()),
        }
    }
}

impl std::error::Error for NodeError {}

impl From<rusqlite::Error> for NodeError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error)
    }
}

impl From<serde_json::Error> for NodeError {
    fn from(error: serde_json::Error) -> Self {
        Self::Encoding(error)
    }
}

/// 节点服务结果别名。
pub type NodeResult<T> = Result<T, NodeError>;
