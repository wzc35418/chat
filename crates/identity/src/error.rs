//! 身份层的稳定错误与错误码映射。
//!
//! 码表由 protocol 的 ErrorCode 冻结（PROTOCOL-014），本模块**不新增**码。
//! 计划书里出现的模块级名字（INVITE_EXPIRED、IDENTITY_CHANGED 等）没有独立码，
//! 因此映射到最接近的冻结码，并保留独立 detail_key 供 UI 文案区分；
//! 映射表登记在 docs/implementation/identity.md 的「错误码映射」段。
//!
//! 错误结构只含类型化原因与静态诊断键，不含密钥、口令正文或其它敏感材料。

use core::fmt;

use chat_protocol::{ErrorCode, IdError, ProtocolError};

use crate::contact::ContactState;

/// identity 层返回类型。
pub type Result<T> = core::result::Result<T, IdentityError>;

/// 身份层失败原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityError {
    /// ID 文本解析失败（透传 PROTOCOL-001 原因）。
    InvalidId(IdError),
    /// 显示名为空、过长或含控制字符。
    InvalidDisplayName,
    /// 时间戳为 0 或无法换算。
    InvalidTimestamp,
    /// 账户公钥不是合法的 Ed25519 公钥。
    InvalidAccountPublicKey,
    /// 账户 ID 与账户公钥不自洽。
    AccountKeyMismatch,
    /// Ed25519 签名验证失败。
    InvalidSignature,
    /// 凭据结构、绑定或签名不合法。
    InvalidCredential,
    /// 凭据已过期。
    CredentialExpired,
    /// 凭据生效时间晚于本地时钟 + 允许偏差。
    CredentialNotYetValid,
    /// 凭据签发早于当前撤销版本，必须重新签发。
    CredentialStale,
    /// 设备已被撤销。
    DeviceRevoked,
    /// 设备/凭据属于另一个账户。
    CrossAccountDevice,
    /// 设备 ID 与（账户, 设备公钥）派生值不一致。
    DeviceIdMismatch,
    /// 能力集合为空。
    EmptyCapabilities,
    /// 能力位包含未知值。
    UnknownCapability(u32),
    /// 能力数量超过上限。
    TooManyCapabilities { limit: usize },
    /// 口令文本结构畸形。
    MalformedToken,
    /// 口令文本超过长度上限。
    TokenTooLong { limit: usize },
    /// 口令版本前缀不受支持。
    UnsupportedTokenVersion,
    /// 邀请已过期。
    InviteExpired,
    /// 邀请的签发时间晚于本地时钟 + 允许偏差。
    InviteNotYetValid,
    /// 邀请有效期超出允许范围（含 0 与溢出）。
    InvalidInviteLifetime { max_ms: u64 },
    /// 邀请绑定的节点与期望不符。
    WrongInviteNode,
    /// 邀请绑定的账户与期望或已固定信任锚不符。
    WrongInviteAccount,
    /// 邀请绑定的设备与期望不符。
    WrongInviteDevice,
    /// 邀请作用域不是 contact-request。
    WrongInviteScope,
    /// 联系人状态迁移非法。
    InvalidStateTransition { from: ContactState, to: ContactState },
    /// 对端身份指纹与本地固定信任锚不一致。
    IdentityChanged,
    /// 联系人尚未被用户确认，禁止发送。
    ContactNotAccepted,
    /// 缺少对端身份指纹，无法确认联系人。
    MissingIdentity,
    /// 撤销集合超过容量上限。
    TooManyRevocations { limit: usize },
}

impl IdentityError {
    /// 稳定错误码（全部来自 protocol 冻结码表）。
    pub const fn code(self) -> ErrorCode {
        match self {
            IdentityError::InvalidId(_) => ErrorCode::InvalidId,
            IdentityError::InvalidDisplayName => ErrorCode::InvalidTarget,
            IdentityError::InvalidTimestamp => ErrorCode::MalformedFrame,
            IdentityError::InvalidAccountPublicKey
            | IdentityError::AccountKeyMismatch
            | IdentityError::InvalidCredential
            | IdentityError::CredentialExpired
            | IdentityError::CredentialNotYetValid
            | IdentityError::CredentialStale
            | IdentityError::CrossAccountDevice
            | IdentityError::DeviceIdMismatch
            | IdentityError::IdentityChanged
            | IdentityError::MissingIdentity
            | IdentityError::InviteNotYetValid => ErrorCode::InvalidCredential,
            IdentityError::InvalidSignature => ErrorCode::InvalidSignature,
            IdentityError::DeviceRevoked => ErrorCode::DeviceRevoked,
            IdentityError::EmptyCapabilities
            | IdentityError::UnknownCapability(_)
            | IdentityError::WrongInviteScope => ErrorCode::RequiredCapabilityMissing,
            IdentityError::TooManyCapabilities { .. }
            | IdentityError::TooManyRevocations { .. } => ErrorCode::ContentTooLarge,
            IdentityError::MalformedToken => ErrorCode::MalformedFrame,
            IdentityError::TokenTooLong { .. } => ErrorCode::ContentTooLarge,
            IdentityError::UnsupportedTokenVersion => ErrorCode::ProtocolUnsupported,
            IdentityError::InviteExpired => ErrorCode::CursorExpired,
            IdentityError::InvalidInviteLifetime { .. }
            | IdentityError::WrongInviteNode
            | IdentityError::WrongInviteAccount
            | IdentityError::WrongInviteDevice => ErrorCode::InvalidTarget,
            IdentityError::InvalidStateTransition { .. } => ErrorCode::InvalidStateTransition,
            IdentityError::ContactNotAccepted => ErrorCode::ContactNotAccepted,
        }
    }

    /// 静态诊断键（UI 文案映射用，不含用户内容）。
    pub const fn detail_key(self) -> &'static str {
        match self {
            IdentityError::InvalidId(_) => "identity.invalid_id",
            IdentityError::InvalidDisplayName => "identity.invalid_display_name",
            IdentityError::InvalidTimestamp => "identity.invalid_timestamp",
            IdentityError::InvalidAccountPublicKey => "identity.invalid_account_public_key",
            IdentityError::AccountKeyMismatch => "identity.account_key_mismatch",
            IdentityError::InvalidSignature => "identity.invalid_signature",
            IdentityError::InvalidCredential => "identity.invalid_credential",
            IdentityError::CredentialExpired => "identity.credential_expired",
            IdentityError::CredentialNotYetValid => "identity.credential_not_yet_valid",
            IdentityError::CredentialStale => "identity.credential_stale",
            IdentityError::DeviceRevoked => "identity.device_revoked",
            IdentityError::CrossAccountDevice => "identity.cross_account_device",
            IdentityError::DeviceIdMismatch => "identity.device_id_mismatch",
            IdentityError::EmptyCapabilities => "identity.empty_capabilities",
            IdentityError::UnknownCapability(_) => "identity.unknown_capability",
            IdentityError::TooManyCapabilities { .. } => "identity.too_many_capabilities",
            IdentityError::MalformedToken => "identity.malformed_token",
            IdentityError::TokenTooLong { .. } => "identity.token_too_long",
            IdentityError::UnsupportedTokenVersion => "identity.unsupported_token_version",
            IdentityError::InviteExpired => "identity.invite_expired",
            IdentityError::InviteNotYetValid => "identity.invite_not_yet_valid",
            IdentityError::InvalidInviteLifetime { .. } => "identity.invalid_invite_lifetime",
            IdentityError::WrongInviteNode => "identity.wrong_invite_node",
            IdentityError::WrongInviteAccount => "identity.wrong_invite_account",
            IdentityError::WrongInviteDevice => "identity.wrong_invite_device",
            IdentityError::WrongInviteScope => "identity.wrong_invite_scope",
            IdentityError::InvalidStateTransition { .. } => "identity.invalid_state_transition",
            IdentityError::IdentityChanged => "identity.changed",
            IdentityError::ContactNotAccepted => "identity.contact_not_accepted",
            IdentityError::MissingIdentity => "identity.missing_identity",
            IdentityError::TooManyRevocations { .. } => "identity.too_many_revocations",
        }
    }

    /// 该错误默认是否可重试。身份/验签/状态类失败一律不可自动重试。
    pub const fn retryable(self) -> bool {
        self.code().retryable()
    }
}

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdentityError::InvalidId(reason) => write!(formatter, "invalid id: {reason}"),
            IdentityError::InvalidDisplayName => formatter.write_str("invalid display name"),
            IdentityError::InvalidTimestamp => formatter.write_str("invalid timestamp"),
            IdentityError::InvalidAccountPublicKey => {
                formatter.write_str("invalid account public key")
            }
            IdentityError::AccountKeyMismatch => {
                formatter.write_str("account id does not match the account public key")
            }
            IdentityError::InvalidSignature => formatter.write_str("invalid signature"),
            IdentityError::InvalidCredential => formatter.write_str("invalid credential"),
            IdentityError::CredentialExpired => formatter.write_str("credential expired"),
            IdentityError::CredentialNotYetValid => formatter.write_str("credential not yet valid"),
            IdentityError::CredentialStale => formatter.write_str("credential is stale"),
            IdentityError::DeviceRevoked => formatter.write_str("device revoked"),
            IdentityError::CrossAccountDevice => {
                formatter.write_str("device belongs to another account")
            }
            IdentityError::DeviceIdMismatch => {
                formatter.write_str("device id does not match its binding")
            }
            IdentityError::EmptyCapabilities => formatter.write_str("capability set is empty"),
            IdentityError::UnknownCapability(value) => {
                write!(formatter, "unknown capability {value}")
            }
            IdentityError::TooManyCapabilities { limit } => {
                write!(formatter, "capability count exceeds {limit}")
            }
            IdentityError::MalformedToken => formatter.write_str("malformed token"),
            IdentityError::TokenTooLong { limit } => {
                write!(formatter, "token exceeds {limit} bytes")
            }
            IdentityError::UnsupportedTokenVersion => {
                formatter.write_str("unsupported token version")
            }
            IdentityError::InviteExpired => formatter.write_str("invite expired"),
            IdentityError::InviteNotYetValid => formatter.write_str("invite not yet valid"),
            IdentityError::InvalidInviteLifetime { max_ms } => {
                write!(formatter, "invite lifetime is outside 1..={max_ms} ms")
            }
            IdentityError::WrongInviteNode => formatter.write_str("invite binds another node"),
            IdentityError::WrongInviteAccount => formatter.write_str("invite binds another account"),
            IdentityError::WrongInviteDevice => formatter.write_str("invite binds another device"),
            IdentityError::WrongInviteScope => {
                formatter.write_str("invite scope is not contact-request")
            }
            IdentityError::InvalidStateTransition { from, to } => write!(
                formatter,
                "illegal contact transition {} -> {}",
                from.as_str(),
                to.as_str()
            ),
            IdentityError::IdentityChanged => {
                formatter.write_str("peer identity differs from the pinned trust anchor")
            }
            IdentityError::ContactNotAccepted => formatter.write_str("contact is not accepted"),
            IdentityError::MissingIdentity => {
                formatter.write_str("contact has no observed identity fingerprint")
            }
            IdentityError::TooManyRevocations { limit } => {
                write!(formatter, "revocation set exceeds {limit} devices")
            }
        }
    }
}

impl std::error::Error for IdentityError {}

impl From<ProtocolError> for IdentityError {
    fn from(error: ProtocolError) -> Self {
        match error {
            ProtocolError::InvalidId(reason) => IdentityError::InvalidId(reason),
            ProtocolError::InvalidSignature => IdentityError::InvalidSignature,
            ProtocolError::ContentTooLarge { limit } => IdentityError::TokenTooLong { limit },
            _ => IdentityError::InvalidCredential,
        }
    }
}
