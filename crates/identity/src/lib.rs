//! identity 模块：账户根密钥、设备凭据生命周期、联系方式邀请口令与联系人状态机。
//!
//! 实现范围（与 docs/implementation/identity.md 的 IDENTITY-001~012 对应）：
//!
//! - [`keys`]：账户根密钥/设备密钥、账户与设备 ID 派生、人工核验指纹；
//! - [`device`]：设备能力集合、账户签名的设备凭据与验证（含撤销/期限）；
//! - [`invite`]：一次性邀请口令 `inv1.`（canonical 正文 + Ed25519 签名 + 期限）；
//! - [`contact`]：联系人状态机与固定信任锚（指纹变化即阻断发送）；
//! - [`service`]：脱敏账户视图与上述能力的门面（不含数据库/网络副作用）；
//! - [`registry`]：IDENTITY-008~012 的设备配对/授权/列表/撤销（经正式加密存储持久化）；
//! - [`error`]：稳定错误码映射。
//!
//! 本 crate 不访问网络、不模拟成功：纯密码学与状态层不做 I/O，持久化只经
//! chat-storage 的正式加密存储；签名原文 = 目的域字符串 || chat-protocol canonical
//! 编码，验签复用 protocol 的 Ed25519 原语，绝不接受未验证输入。
#![forbid(unsafe_code)]

pub mod contact;
pub mod device;
pub mod devices;
pub mod error;
pub mod invite;
pub mod keys;
pub mod recovery;
pub mod registry;
pub mod service;

pub use contact::{Contact, ContactState, IdentityUpdate};
pub use device::{
    CapabilitySet, DeviceCapability, DeviceCredential, RevocationSet, VerifiedDevice,
    create_device_credential, verify_credential,
};
pub use devices::{
    DeviceEntry, DeviceError, DeviceResult, device_is_revoked, local_identity_present,
    load_device, register_device, revoke_registered_device, registered_devices,
};
pub use error::{IdentityError, Result, StorageErrorKind};
pub use invite::{
    DEFAULT_INVITE_TTL_MS, InviteExpectations, InviteScope, InviteToken, VerifiedInvite,
    classify_invite_text, create_invite, verify_invite,
};
pub use keys::{
    ACCOUNT_ID_DOMAIN, AccountKeys, DEVICE_ID_DOMAIN, DeviceKeys, FINGERPRINT_DOMAIN, Fingerprint,
    KEY_SEED_BYTES, PUBLIC_KEY_BYTES, SIGNATURE_BYTES, TrustAnchor, account_id_from_public_key,
    device_id_for,
};
pub use registry::{
    AuthorizedDevice, DeviceRevocation, DeviceState, DeviceSummary, MAX_ACTIVE_DEVICES,
    MAX_LINK_TTL_MS, MIN_LINK_TTL_MS, PendingDeviceLink, authorize_device, begin_device_link,
    complete_device_link, list_devices, require_active_device, revoke_device,
};
pub use recovery::{
    ALLOWED_ARGON2_PARAMS, Argon2Params, MAX_PASSPHRASE_BYTES, MAX_RECOVERY_BUNDLE_BYTES,
    MIN_PASSPHRASE_BYTES, RECOVERY_FORMAT_VERSION, RecoveryError, RecoveryIdentityView,
    RecoveryResult, export_recovery_bundle, export_recovery_bundle_with_params,
    import_recovery_bundle,
};
pub use service::{
    AccountView, ContactRequest, LocalAccount, MAX_DISPLAY_NAME_BYTES, accept_contact_invite,
    confirm_contact, current_account, verification_fingerprint,
};
