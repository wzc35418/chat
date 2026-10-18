
//! IDENTITY-008~012：双设备配对、设备授权、凭据完成、设备列表与撤销（持久化版）。
//!
//! 与纯密码学层的分工：
//!
//! - [crate::device] 负责凭据签发与校验的纯函数（签名、派生、期限、撤销集合检查）；
//! - 本模块负责**生命周期与持久化**：配对请求 → 账户授权并落库 → 新设备校验后落库 →
//!   列表 → 撤销，全部经 chat-storage 的正式加密存储（迁移 0006），不访问网络。
//!
//! 关键安全语义：
//!
//! - 账户有效设备上限 2 台（类型字典约束），超出显式 DEVICE_LIMIT_EXCEEDED；
//! - 撤销是终态：storage 拒绝为已撤销设备再写凭据，因此不能静默复活；
//! - 撤销推进账户撤销版本；其余设备必须重新签发凭据，否则按 INVALID_CREDENTIAL
//!   （detail identity.credential_stale）拒绝；
//! - 发送/解密前必须调用 [require_active_device]：过期、撤销、版本落后、能力不足
//!   一律显式失败，绝不降级为"尽力发送"；
//! - 最后一台可管理设备不允许在此直接撤销（需要恢复流程）。

use chat_protocol::{AccountId, DeviceId, Digest};
use chat_storage::formal::{DeviceCredentialInput, DeviceCredentialRecord, Store};

use crate::device::{
    CapabilitySet, DeviceCapability, DeviceCredential, RevocationSet, VerifiedDevice,
    create_device_credential, verify_credential,
};
use crate::error::{IdentityError, Result, StorageErrorKind};
use crate::keys::{
    AccountKeys, DeviceKeys, PUBLIC_KEY_BYTES, TrustAnchor, device_id_for,
};

/// 单账户有效设备上限（类型字典：每账户最多 2 台有效设备）。
pub const MAX_ACTIVE_DEVICES: usize = 2;
/// 配对请求最短有效期：30 秒。
pub const MIN_LINK_TTL_MS: u64 = 30_000;
/// 配对请求最长有效期：10 分钟。
pub const MAX_LINK_TTL_MS: u64 = 10 * 60_000;
/// 配对 nonce 字节数。
pub const LINK_NONCE_BYTES: usize = 16;
/// 配对短码派生域。
const LINK_CODE_DOMAIN: &[u8] = b"chat/p1/device-link-code/v1";

/// IDENTITY-008：第二设备的待配对请求（本机秘密；Debug 脱敏）。
pub struct PendingDeviceLink {
    keys: DeviceKeys,
    nonce: [u8; LINK_NONCE_BYTES],
    issued_at_ms: u64,
    expires_at_ms: u64,
}

impl PendingDeviceLink {
    /// 生成本机设备密钥与一次性配对请求。
    ///
    /// 有效期必须在 30 秒..=10 分钟内；配对请求只在本地保存，私钥永不进入二维码。
    pub fn begin(now_ms: u64, ttl_ms: u64) -> Result<Self> {
        if now_ms == 0 {
            return Err(IdentityError::InvalidTimestamp);
        }
        if ttl_ms == 0 || !(MIN_LINK_TTL_MS..=MAX_LINK_TTL_MS).contains(&ttl_ms) {
            return Err(IdentityError::InvalidLinkTtl {
                max_ms: MAX_LINK_TTL_MS,
            });
        }
        let expires_at_ms = now_ms
            .checked_add(ttl_ms)
            .ok_or(IdentityError::InvalidLinkTtl {
                max_ms: MAX_LINK_TTL_MS,
            })?;
        let mut nonce = [0u8; LINK_NONCE_BYTES];
        rand::fill(&mut nonce);
        Ok(Self {
            keys: DeviceKeys::generate(),
            nonce,
            issued_at_ms: now_ms,
            expires_at_ms,
        })
    }

    /// 本机设备公钥（可放进二维码）。
    pub fn device_public_key(&self) -> [u8; PUBLIC_KEY_BYTES] {
        self.keys.public_key()
    }

    /// 一次性 nonce。
    pub const fn nonce(&self) -> &[u8; LINK_NONCE_BYTES] {
        &self.nonce
    }

    /// 签发时间。
    pub const fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }

    /// 过期时间。
    pub const fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    /// 本地时间是否已过期。
    pub const fn is_expired_at(&self, now_ms: u64) -> bool {
        now_ms >= self.expires_at_ms
    }

    /// 6 位核对短码（用户在两台设备之间人工比对；不参与任何密码学校验）。
    pub fn verification_code(&self) -> String {
        let digest = Digest::sha256(
            &[
                LINK_CODE_DOMAIN,
                self.device_public_key().as_slice(),
                self.nonce.as_slice(),
            ]
            .concat(),
        );
        let bytes = digest.as_bytes();
        let value = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) % 1_000_000;
        format!("{:03} {:03}", value / 1_000, value % 1_000)
    }

    /// 过期即显式失败（必须重新生成，不复用旧 nonce）。
    pub fn require_fresh(&self, now_ms: u64) -> Result<()> {
        if self.is_expired_at(now_ms) {
            return Err(IdentityError::LinkExpired);
        }
        Ok(())
    }
}

impl std::fmt::Debug for PendingDeviceLink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingDeviceLink")
            .field("device_public_key", &self.device_public_key())
            .field("issued_at_ms", &self.issued_at_ms)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("keys", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// IDENTITY-009 的结果：已签发并落库的设备授权。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedDevice {
    credential: DeviceCredential,
    record: DeviceCredentialRecord,
}

impl AuthorizedDevice {
    /// 可交给新设备的凭据（公开材料）。
    pub const fn credential(&self) -> &DeviceCredential {
        &self.credential
    }

    /// 存储记录（含 already_existed）。
    pub const fn record(&self) -> &DeviceCredentialRecord {
        &self.record
    }

    /// 设备 ID。
    pub const fn device_id(&self) -> DeviceId {
        self.credential.device_id
    }

    /// 失效时间。
    pub const fn expires_at_ms(&self) -> u64 {
        self.credential.expires_at_ms
    }
}

/// 设备在列表中的状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeviceState {
    /// 有效。
    Active,
    /// 已过期（必须重新签发）。
    Expired,
    /// 已撤销（终态）。
    Revoked,
}

impl DeviceState {
    /// 稳定名字。
    pub const fn as_str(self) -> &'static str {
        match self {
            DeviceState::Active => "active",
            DeviceState::Expired => "expired",
            DeviceState::Revoked => "revoked",
        }
    }
}

/// IDENTITY-011：设备摘要（只含公开材料，按稳定设备 ID 排序）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceSummary {
    /// 设备 ID。
    pub device_id: DeviceId,
    /// 设备公钥。
    pub device_public_key: [u8; PUBLIC_KEY_BYTES],
    /// 能力集合。
    pub capabilities: CapabilitySet,
    /// 签发时间。
    pub issued_at_ms: u64,
    /// 失效时间。
    pub expires_at_ms: u64,
    /// 状态。
    pub state: DeviceState,
}

/// IDENTITY-012 的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceRevocation {
    /// 被撤销设备。
    pub device_id: DeviceId,
    /// 撤销后的账户撤销版本。
    pub revocation_generation: u64,
    /// 撤销时间（Unix 毫秒）。
    pub revoked_at_ms: u64,
}

fn link_ttl_bounds(ttl_ms: u64) -> Result<()> {
    if ttl_ms == 0 || !(MIN_LINK_TTL_MS..=MAX_LINK_TTL_MS).contains(&ttl_ms) {
        return Err(IdentityError::InvalidLinkTtl {
            max_ms: MAX_LINK_TTL_MS,
        });
    }
    Ok(())
}

fn timestamp_to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| IdentityError::InvalidTimestamp)
}

fn timestamp_to_u64(value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| IdentityError::Storage(StorageErrorKind::Corrupted))
}

fn record_from_credential(credential: &DeviceCredential) -> DeviceCredentialInput {
    DeviceCredentialInput {
        account_id: credential.account_id.encode(),
        device_id: credential.device_id.encode(),
        device_public_key: credential.device_public_key,
        endpoint: credential.endpoint,
        capability_mask: credential.capabilities.mask(),
        // 有效期在上层已校验为正且非零；转换失败会由存储层 validate 拒绝。
        issued_at_ms: i64::try_from(credential.issued_at_ms).unwrap_or(i64::MAX),
        expires_at_ms: i64::try_from(credential.expires_at_ms).unwrap_or(i64::MAX),
        revocation_generation: credential.revocation_generation,
        signature: credential.signature,
    }
}

fn credential_from_record(
    record: &DeviceCredentialRecord,
    anchor: &TrustAnchor,
) -> Result<DeviceCredential> {
    let device_id = DeviceId::parse(&record.device_id)
        .map_err(|_| IdentityError::Storage(StorageErrorKind::Corrupted))?;
    let capabilities = CapabilitySet::from_mask(record.capability_mask)
        .map_err(|_| IdentityError::Storage(StorageErrorKind::Corrupted))?;
    Ok(DeviceCredential {
        account_id: anchor.account_id(),
        account_public_key: *anchor.account_public_key(),
        device_id,
        device_public_key: record.device_public_key,
        endpoint: record.endpoint,
        capabilities,
        issued_at_ms: timestamp_to_u64(record.issued_at_ms)?,
        expires_at_ms: timestamp_to_u64(record.expires_at_ms)?,
        revocation_generation: record.revocation_generation,
        signature: record.signature,
    })
}

fn revocation_set_from_store(store: &Store, account: AccountId) -> Result<RevocationSet> {
    let stored = store.revoked_devices(&account.encode())?;
    let mut devices = Vec::with_capacity(stored.revoked.len());
    for record in &stored.revoked {
        devices.push(
            DeviceId::parse(&record.device_id)
                .map_err(|_| IdentityError::Storage(StorageErrorKind::Corrupted))?,
        );
    }
    RevocationSet::new(account, stored.generation, devices)
}

fn is_record_active(record: &DeviceCredentialRecord, now_ms: u64) -> Result<bool> {
    if record.revoked {
        return Ok(false);
    }
    Ok(now_ms < timestamp_to_u64(record.expires_at_ms)?)
}

fn count_active_devices(store: &Store, account: AccountId, now_ms: u64) -> Result<usize> {
    let records = store.list_device_credentials(&account.encode())?;
    let mut active = 0usize;
    for record in &records {
        if is_record_active(record, now_ms)? {
            active += 1;
        }
    }
    Ok(active)
}

/// IDENTITY-008：开始本机设备配对（生成设备密钥与短期一次性请求）。
pub fn begin_device_link(now_ms: u64, ttl_ms: u64) -> Result<PendingDeviceLink> {
    link_ttl_bounds(ttl_ms)?;
    PendingDeviceLink::begin(now_ms, ttl_ms)
}

/// IDENTITY-009：主管设备为用户核对的配对请求签发设备凭据并落库。
///
/// 拒绝：配对请求过期、设备已被撤销、账户有效设备已达上限、能力集合为空。
pub fn authorize_device(
    store: &mut Store,
    account: &AccountKeys,
    pending: &PendingDeviceLink,
    capabilities: CapabilitySet,
    endpoint: Option<[u8; PUBLIC_KEY_BYTES]>,
    now_ms: u64,
    ttl_ms: u64,
) -> Result<AuthorizedDevice> {
    pending.require_fresh(now_ms)?;
    if capabilities.is_empty() {
        return Err(IdentityError::EmptyCapabilities);
    }
    let account_id = account.account_id();
    let device_id = device_id_for(account_id, &pending.device_public_key());
    let revocations = revocation_set_from_store(store, account_id)?;
    if revocations.contains(device_id) {
        return Err(IdentityError::DeviceRevoked);
    }
    if count_active_devices(store, account_id, now_ms)? >= MAX_ACTIVE_DEVICES {
        return Err(IdentityError::DeviceLimitExceeded);
    }
    let revocation_generation = store.device_revocation_generation(&account_id.encode())?;
    let credential = create_device_credential(
        account,
        &pending.keys,
        capabilities,
        endpoint,
        now_ms,
        ttl_ms,
        revocation_generation,
    )?;
    let record = store.save_device_credential(record_from_credential(&credential))?;
    if record.device_id != credential.device_id.encode()
        || record.device_public_key != credential.device_public_key
        || record.signature != credential.signature
    {
        return Err(IdentityError::Storage(StorageErrorKind::Corrupted));
    }
    Ok(AuthorizedDevice { credential, record })
}

/// IDENTITY-010：新设备校验主管签名并保存自己的凭据。
///
/// 凭据必须绑定本机待配对公钥；过期/撤销/信任根不一致一律显式失败。
pub fn complete_device_link(
    store: &mut Store,
    pending: &PendingDeviceLink,
    credential: &DeviceCredential,
    now_ms: u64,
) -> Result<VerifiedDevice> {
    pending.require_fresh(now_ms)?;
    if credential.device_public_key != pending.device_public_key() {
        return Err(IdentityError::LinkMismatch);
    }
    let anchor = TrustAnchor::from_public_key(credential.account_public_key)?;
    let revocations = revocation_set_from_store(store, anchor.account_id())?;
    let verified = verify_credential(credential, &anchor, &revocations, now_ms)?;
    let record = store.save_device_credential(record_from_credential(credential))?;
    if record.device_id != verified.device_id().encode() {
        return Err(IdentityError::Storage(StorageErrorKind::Corrupted));
    }
    Ok(verified)
}

/// IDENTITY-011：列出本账户设备（按稳定设备 ID 排序，含过期/撤销状态）。
pub fn list_devices(store: &Store, account: AccountId, now_ms: u64) -> Result<Vec<DeviceSummary>> {
    let records = store.list_device_credentials(&account.encode())?;
    let mut summaries = Vec::with_capacity(records.len());
    for record in &records {
        let device_id = DeviceId::parse(&record.device_id)
            .map_err(|_| IdentityError::Storage(StorageErrorKind::Corrupted))?;
        let capabilities = CapabilitySet::from_mask(record.capability_mask)
            .map_err(|_| IdentityError::Storage(StorageErrorKind::Corrupted))?;
        let expires_at_ms = timestamp_to_u64(record.expires_at_ms)?;
        let state = if record.revoked {
            DeviceState::Revoked
        } else if now_ms >= expires_at_ms {
            DeviceState::Expired
        } else {
            DeviceState::Active
        };
        summaries.push(DeviceSummary {
            device_id,
            device_public_key: record.device_public_key,
            capabilities,
            issued_at_ms: timestamp_to_u64(record.issued_at_ms)?,
            expires_at_ms,
            state,
        });
    }
    summaries.sort_by_key(|summary| summary.device_id);
    Ok(summaries)
}

/// IDENTITY-012：撤销设备（终态、幂等、撤销版本单调递增）。
///
/// 拒绝撤销本账户最后一台仍有效的可管理设备（需要恢复流程）。
pub fn revoke_device(
    store: &mut Store,
    account: AccountId,
    device: DeviceId,
    now_ms: u64,
) -> Result<DeviceRevocation> {
    if now_ms == 0 {
        return Err(IdentityError::InvalidTimestamp);
    }
    let account_text = account.encode();
    let device_text = device.encode();
    let active = count_active_devices(store, account, now_ms)?;
    let target_active = store
        .load_device_credential(&account_text, &device_text)?
        .map(|record| is_record_active(&record, now_ms))
        .transpose()?
        .unwrap_or(false);
    if target_active && active <= 1 {
        return Err(IdentityError::LastDeviceRequiresRecovery);
    }
    let record = store.revoke_device(&account_text, &device_text, timestamp_to_i64(now_ms)?)?;
    Ok(DeviceRevocation {
        device_id: device,
        revocation_generation: record.revocation_generation,
        revoked_at_ms: timestamp_to_u64(record.revoked_at_ms)?,
    })
}

/// 发送/解密前的强制检查（IDENTITY-010 的授权上下文）。
///
/// 委托 [verify_credential]：过期、撤销、撤销版本落后、信任根不一致都显式失败；
/// 需要的能力不在凭据里则 REQUIRED_CAPABILITY_MISSING。
pub fn require_active_device(
    store: &Store,
    anchor: &TrustAnchor,
    device: DeviceId,
    now_ms: u64,
    required: Option<DeviceCapability>,
) -> Result<VerifiedDevice> {
    let record = store
        .load_device_credential(&anchor.account_id().encode(), &device.encode())?
        .ok_or(IdentityError::DeviceNotFound)?;
    let credential = credential_from_record(&record, anchor)?;
    let revocations = revocation_set_from_store(store, anchor.account_id())?;
    let verified = verify_credential(&credential, anchor, &revocations, now_ms)?;
    if let Some(required) = required
        && !verified.has(required)
    {
        return Err(IdentityError::MissingCapability);
    }
    Ok(verified)
}
