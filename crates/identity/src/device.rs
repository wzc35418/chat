//! 设备能力集合、账户签名的设备凭据与验证。
//!
//! 设备凭据由账户根密钥签发，绑定：账户 ID、账户公钥、设备 ID、设备公钥、
//! 可选端点标识、能力集合、生效/失效时间与撤销版本。验证顺序：
//!
//! 1. 信任锚绑定（账户 ID + 公钥）→ 跨账户设备/凭据显式拒绝；
//! 2. 自洽性（账户 ID 由公钥派生、设备 ID 由账户+设备公钥派生）；
//! 3. 撤销集合（账户一致、设备未撤销、凭据版本不落后）；
//! 4. 账户签名（先验签，后看时间，避免伪造凭据探测期限）；
//! 5. 期限（未生效 / 已过期）。
//!
//! 签名原文 = 目的域 || chat-protocol canonical 编码。canonical 的字段号表是
//! 协议层冻结的，identity 不能新增字段号，因此正文用**位置固定**的 canonical
//! 列表编码，并把 schema/协议版本放在正文最前面。

use core::fmt;

use chat_protocol::canonical::{ProtocolVersion, WireValue, encode_canonical};
use chat_protocol::{AccountId, DeviceId};

use crate::error::{IdentityError, Result};
use crate::keys::{
    AccountKeys, DeviceKeys, PUBLIC_KEY_BYTES, SIGNATURE_BYTES, TrustAnchor,
    account_id_from_public_key, device_id_for, verify_signature,
};

/// 单账户凭据允许的能力数量上限。
pub const MAX_DEVICE_CAPABILITIES: usize = 8;
/// 设备凭据最短有效期：1 分钟。
pub const MIN_DEVICE_TTL_MS: u64 = 60_000;
/// 设备凭据最长有效期：400 天。
pub const MAX_DEVICE_TTL_MS: u64 = 400 * 24 * 60 * 60 * 1_000;
/// 允许的时钟偏差：60 秒。
pub const DEVICE_CREDENTIAL_CLOCK_SKEW_MS: u64 = 60_000;
/// 设备凭据签名域。
pub const DEVICE_CREDENTIAL_SIGNING_DOMAIN: &[u8] = b"chat/p1/device-credential-signature/v1";
/// 凭据规范 schema 版本。
pub const CREDENTIAL_SCHEMA_VERSION: u64 = 1;
/// 撤销集合容量上限。
pub const MAX_REVOKED_DEVICES: usize = 64;

/// 设备能力（冻结数值，未知值必须拒绝）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceCapability {
    /// 一对一私聊收发。
    SendDirect = 1,
    /// 群聊收发。
    SendGroup = 2,
    /// 只接收（不主动发送）。
    Receive = 3,
    /// 附件上传。
    UploadAttachment = 4,
    /// 本人多设备同步。
    SelfSync = 5,
    /// 设备管理（授权/撤销）。
    ManageDevices = 6,
}

impl DeviceCapability {
    /// 全部冻结能力。
    pub const ALL: [DeviceCapability; 6] = [
        DeviceCapability::SendDirect,
        DeviceCapability::SendGroup,
        DeviceCapability::Receive,
        DeviceCapability::UploadAttachment,
        DeviceCapability::SelfSync,
        DeviceCapability::ManageDevices,
    ];

    /// 冻结数值。
    pub const fn as_u64(self) -> u64 {
        self as u64
    }

    /// 由冻结数值还原；未知值返回 None。
    pub const fn from_u64(value: u64) -> Option<Self> {
        match value {
            1 => Some(DeviceCapability::SendDirect),
            2 => Some(DeviceCapability::SendGroup),
            3 => Some(DeviceCapability::Receive),
            4 => Some(DeviceCapability::UploadAttachment),
            5 => Some(DeviceCapability::SelfSync),
            6 => Some(DeviceCapability::ManageDevices),
            _ => None,
        }
    }

    /// 稳定名字（日志/诊断用）。
    pub const fn as_str(self) -> &'static str {
        match self {
            DeviceCapability::SendDirect => "send-direct",
            DeviceCapability::SendGroup => "send-group",
            DeviceCapability::Receive => "receive",
            DeviceCapability::UploadAttachment => "upload-attachment",
            DeviceCapability::SelfSync => "self-sync",
            DeviceCapability::ManageDevices => "manage-devices",
        }
    }

    const fn bit(self) -> u32 {
        1 << (self as u32)
    }
}

const KNOWN_CAPABILITY_MASK: u32 = 0b0111_1110;

/// 能力集合（位集合，天然去重且编码唯一）。
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct CapabilitySet {
    mask: u32,
}

impl CapabilitySet {
    /// 空集合。
    pub const fn empty() -> Self {
        Self { mask: 0 }
    }

    /// 由能力列表构造；空列表拒绝，超过上限拒绝。
    pub fn try_new(items: &[DeviceCapability]) -> Result<Self> {
        if items.is_empty() {
            return Err(IdentityError::EmptyCapabilities);
        }
        if items.len() > MAX_DEVICE_CAPABILITIES {
            return Err(IdentityError::TooManyCapabilities {
                limit: MAX_DEVICE_CAPABILITIES,
            });
        }
        let mut mask = 0u32;
        for item in items {
            mask |= item.bit();
        }
        Self::from_mask(mask)
    }

    /// 由位图还原；未知位/超上限显式失败。
    pub fn from_mask(mask: u32) -> Result<Self> {
        if mask & !KNOWN_CAPABILITY_MASK != 0 {
            return Err(IdentityError::UnknownCapability(mask & !KNOWN_CAPABILITY_MASK));
        }
        let set = Self { mask };
        if set.count() > MAX_DEVICE_CAPABILITIES {
            return Err(IdentityError::TooManyCapabilities {
                limit: MAX_DEVICE_CAPABILITIES,
            });
        }
        Ok(set)
    }

    /// 位图。
    pub const fn mask(self) -> u32 {
        self.mask
    }

    /// 是否包含某能力。
    pub const fn contains(self, capability: DeviceCapability) -> bool {
        self.mask & capability.bit() != 0
    }

    /// 能力数量。
    pub const fn count(self) -> usize {
        self.mask.count_ones() as usize
    }

    /// 是否为空。
    pub const fn is_empty(self) -> bool {
        self.mask == 0
    }

    /// 按冻结数值升序遍历。
    pub fn iter(self) -> impl Iterator<Item = DeviceCapability> {
        DeviceCapability::ALL
            .into_iter()
            .filter(move |capability| self.contains(*capability))
    }
}

impl fmt::Debug for CapabilitySet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_set().entries(self.iter()).finish()
    }
}

/// 账户签发的设备凭据。只含公开材料。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCredential {
    /// 签发账户。
    pub account_id: AccountId,
    /// 签发账户长期公钥（供验签与信任锚比对）。
    pub account_public_key: [u8; PUBLIC_KEY_BYTES],
    /// 设备身份。
    pub device_id: DeviceId,
    /// 设备签名公钥。
    pub device_public_key: [u8; PUBLIC_KEY_BYTES],
    /// 传输端点标识（可选，不由账户授权推断）。
    pub endpoint: Option<[u8; PUBLIC_KEY_BYTES]>,
    /// 授权能力集合。
    pub capabilities: CapabilitySet,
    /// 签发时间（UTC 毫秒）。
    pub issued_at_ms: u64,
    /// 失效时间（UTC 毫秒，不含）。
    pub expires_at_ms: u64,
    /// 签发时已知的账户撤销版本。
    pub revocation_generation: u64,
    /// 账户根密钥签名（64 字节）。
    pub signature: [u8; SIGNATURE_BYTES],
}

fn signing_bytes_with_domain(domain: &[u8], canonical: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(domain.len() + canonical.len());
    out.extend_from_slice(domain);
    out.extend_from_slice(canonical);
    out
}

fn credential_body(credential: &DeviceCredential) -> WireValue {
    WireValue::List(vec![
        WireValue::U64(CREDENTIAL_SCHEMA_VERSION),
        WireValue::U64(u64::from(ProtocolVersion::V1.major)),
        WireValue::U64(u64::from(ProtocolVersion::V1.minor)),
        WireValue::id_bytes(credential.account_id.as_bytes()),
        WireValue::Bytes(credential.account_public_key.to_vec()),
        WireValue::id_bytes(credential.device_id.as_bytes()),
        WireValue::Bytes(credential.device_public_key.to_vec()),
        WireValue::optional(
            credential
                .endpoint
                .map(|endpoint| WireValue::Bytes(endpoint.to_vec())),
        ),
        WireValue::U64(u64::from(credential.capabilities.mask())),
        WireValue::U64(credential.issued_at_ms),
        WireValue::U64(credential.expires_at_ms),
        WireValue::U64(credential.revocation_generation),
    ])
}

impl DeviceCredential {
    /// 设备凭据的签名原文（目的域 + canonical 正文，排除 signature 自身）。
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        let canonical = encode_canonical(&credential_body(self), ProtocolVersion::V1)?;
        Ok(signing_bytes_with_domain(
            DEVICE_CREDENTIAL_SIGNING_DOMAIN,
            canonical.as_bytes(),
        ))
    }
}

/// 由账户根密钥为设备签发凭据。
///
/// 期限必须在 [MIN_DEVICE_TTL_MS, MAX_DEVICE_TTL_MS] 内；能力集合不能为空。
pub fn create_device_credential(
    account: &AccountKeys,
    device: &DeviceKeys,
    capabilities: CapabilitySet,
    endpoint: Option<[u8; PUBLIC_KEY_BYTES]>,
    issued_at_ms: u64,
    ttl_ms: u64,
    revocation_generation: u64,
) -> Result<DeviceCredential> {
    if issued_at_ms == 0 {
        return Err(IdentityError::InvalidTimestamp);
    }
    if ttl_ms == 0 || !(MIN_DEVICE_TTL_MS..=MAX_DEVICE_TTL_MS).contains(&ttl_ms) {
        return Err(IdentityError::InvalidCredential);
    }
    if capabilities.is_empty() {
        return Err(IdentityError::EmptyCapabilities);
    }
    let expires_at_ms = issued_at_ms
        .checked_add(ttl_ms)
        .ok_or(IdentityError::InvalidCredential)?;
    let account_id = account.account_id();
    let device_public_key = device.public_key();
    let mut credential = DeviceCredential {
        account_id,
        account_public_key: account.public_key(),
        device_id: device_id_for(account_id, &device_public_key),
        device_public_key,
        endpoint,
        capabilities,
        issued_at_ms,
        expires_at_ms,
        revocation_generation,
        signature: [0u8; SIGNATURE_BYTES],
    };
    let message = credential.signing_bytes()?;
    credential.signature = account.sign(&message);
    Ok(credential)
}

/// 撤销集合：账户一致、容量有界、按设备 ID 排序去重。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevocationSet {
    account: AccountId,
    generation: u64,
    revoked: Vec<DeviceId>,
}

impl RevocationSet {
    /// 构造撤销集合。
    pub fn new(
        account: AccountId,
        generation: u64,
        revoked: impl IntoIterator<Item = DeviceId>,
    ) -> Result<Self> {
        let mut ids: Vec<DeviceId> = revoked.into_iter().take(MAX_REVOKED_DEVICES + 1).collect();
        if ids.len() > MAX_REVOKED_DEVICES {
            return Err(IdentityError::TooManyRevocations {
                limit: MAX_REVOKED_DEVICES,
            });
        }
        ids.sort_unstable();
        ids.dedup();
        Ok(Self {
            account,
            generation,
            revoked: ids,
        })
    }

    /// 无撤销记录。
    pub const fn empty(account: AccountId, generation: u64) -> Self {
        Self {
            account,
            generation,
            revoked: Vec::new(),
        }
    }

    /// 所属账户。
    pub const fn account(&self) -> AccountId {
        self.account
    }

    /// 撤销版本。
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// 已撤销设备（排序）。
    pub fn revoked(&self) -> &[DeviceId] {
        &self.revoked
    }

    /// 是否已撤销某设备（按序号二分）。
    pub fn contains(&self, device: DeviceId) -> bool {
        self.revoked.binary_search(&device).is_ok()
    }
}

/// 已通过信任锚、签名、撤销与期限检查的设备凭据视图。只含公开材料。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedDevice {
    account_id: AccountId,
    device_id: DeviceId,
    device_public_key: [u8; PUBLIC_KEY_BYTES],
    endpoint: Option<[u8; PUBLIC_KEY_BYTES]>,
    capabilities: CapabilitySet,
    issued_at_ms: u64,
    expires_at_ms: u64,
    fingerprint: crate::keys::Fingerprint,
}

impl VerifiedDevice {
    /// 账户。
    pub const fn account_id(&self) -> AccountId {
        self.account_id
    }

    /// 设备。
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// 设备公钥。
    pub const fn device_public_key(&self) -> &[u8; PUBLIC_KEY_BYTES] {
        &self.device_public_key
    }

    /// 端点标识。
    pub const fn endpoint(&self) -> Option<&[u8; PUBLIC_KEY_BYTES]> {
        self.endpoint.as_ref()
    }

    /// 能力集合。
    pub const fn capabilities(&self) -> CapabilitySet {
        self.capabilities
    }

    /// 签发时间。
    pub const fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }

    /// 失效时间。
    pub const fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    /// 账户人工核验指纹。
    pub const fn fingerprint(&self) -> crate::keys::Fingerprint {
        self.fingerprint
    }

    /// 是否具备某能力。
    pub const fn has(&self, capability: DeviceCapability) -> bool {
        self.capabilities.contains(capability)
    }
}

/// IDENTITY-003：校验设备凭据。
///
/// 信任锚必须来自本机创建或用户核验后的固定记录；服务端新公钥不能覆盖它。
pub fn verify_credential(
    credential: &DeviceCredential,
    trust: &TrustAnchor,
    revoked: &RevocationSet,
    now_ms: u64,
) -> Result<VerifiedDevice> {
    // 1. 信任锚绑定：跨账户凭据一律拒绝。
    if credential.account_id != trust.account_id()
        || credential.account_public_key != *trust.account_public_key()
    {
        return Err(IdentityError::CrossAccountDevice);
    }
    // 2. 派生自洽性。
    if account_id_from_public_key(&credential.account_public_key) != credential.account_id {
        return Err(IdentityError::AccountKeyMismatch);
    }
    if device_id_for(credential.account_id, &credential.device_public_key)
        != credential.device_id
    {
        return Err(IdentityError::DeviceIdMismatch);
    }
    // 3. 撤销集合。
    if revoked.account() != credential.account_id {
        return Err(IdentityError::CrossAccountDevice);
    }
    if revoked.contains(credential.device_id) {
        return Err(IdentityError::DeviceRevoked);
    }
    if credential.revocation_generation < revoked.generation() {
        return Err(IdentityError::CredentialStale);
    }
    if credential.capabilities.is_empty() {
        return Err(IdentityError::EmptyCapabilities);
    }
    // 4. 账户签名（在时间检查之前，避免伪造凭据探测期限）。
    let message = credential.signing_bytes()?;
    verify_signature(
        &credential.account_public_key,
        &message,
        &credential.signature,
    )?;
    // 5. 期限。
    if credential.issued_at_ms == 0 || credential.expires_at_ms <= credential.issued_at_ms {
        return Err(IdentityError::InvalidCredential);
    }
    if now_ms.saturating_add(DEVICE_CREDENTIAL_CLOCK_SKEW_MS) < credential.issued_at_ms {
        return Err(IdentityError::CredentialNotYetValid);
    }
    if now_ms >= credential.expires_at_ms {
        return Err(IdentityError::CredentialExpired);
    }
    Ok(VerifiedDevice {
        account_id: credential.account_id,
        device_id: credential.device_id,
        device_public_key: credential.device_public_key,
        endpoint: credential.endpoint,
        capabilities: credential.capabilities,
        issued_at_ms: credential.issued_at_ms,
        expires_at_ms: credential.expires_at_ms,
        fingerprint: trust.fingerprint(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::KEY_SEED_BYTES;

    fn account(seed: u8) -> AccountKeys {
        AccountKeys::from_seed(&[seed; KEY_SEED_BYTES])
    }

    fn device(seed: u8) -> DeviceKeys {
        DeviceKeys::from_seed(&[seed; KEY_SEED_BYTES])
    }

    fn capabilities() -> CapabilitySet {
        CapabilitySet::try_new(&[
            DeviceCapability::SendDirect,
            DeviceCapability::SendGroup,
            DeviceCapability::Receive,
        ])
        .expect("capability set")
    }

    #[test]
    fn capability_mask_rejects_unknown_bits_and_overflow() {
        assert_eq!(
            CapabilitySet::from_mask(0b1000_0000).unwrap_err(),
            IdentityError::UnknownCapability(0b1000_0000)
        );
        assert_eq!(CapabilitySet::from_mask(0).unwrap(), CapabilitySet::empty());
        assert_eq!(
            CapabilitySet::try_new(&[]).unwrap_err(),
            IdentityError::EmptyCapabilities
        );

        let mut items: Vec<DeviceCapability> = DeviceCapability::ALL.to_vec();
        assert_eq!(items.len(), MAX_DEVICE_CAPABILITIES - 2);
        assert!(CapabilitySet::try_new(&items).is_ok());
        items.extend([DeviceCapability::SendDirect; 3]);
        assert!(matches!(
            CapabilitySet::try_new(&items).unwrap_err(),
            IdentityError::TooManyCapabilities { .. }
        ));

        let set = capabilities();
        assert_eq!(set.count(), 3);
        assert_eq!(set.mask(), 0b1110);
        assert!(set.contains(DeviceCapability::Receive));
        assert_eq!(
            set.iter().collect::<Vec<_>>(),
            vec![
                DeviceCapability::SendDirect,
                DeviceCapability::SendGroup,
                DeviceCapability::Receive
            ]
        );
    }

    #[test]
    fn credential_round_trip_and_binding() {
        let alice = account(0x11);
        let laptop = device(0x22);
        let credential =
            create_device_credential(&alice, &laptop, capabilities(), None, 1_000, 60_000, 3)
                .expect("credential");
        let anchor = alice.trust_anchor();
        let revocations = RevocationSet::empty(alice.account_id(), 3);
        let verified =
            verify_credential(&credential, &anchor, &revocations, 1_001).expect("verified");
        assert_eq!(verified.account_id(), alice.account_id());
        assert_eq!(verified.device_id(), laptop.device_id(alice.account_id()));
        assert_eq!(verified.capabilities(), capabilities());
        assert!(verified.has(DeviceCapability::SendDirect));
        assert!(!verified.has(DeviceCapability::UploadAttachment));
        assert_eq!(verified.fingerprint(), anchor.fingerprint());
        assert!(verified.endpoint().is_none());
    }

    #[test]
    fn tampered_signature_and_device_key_are_rejected() {
        let alice = account(0x11);
        let laptop = device(0x22);
        let credential =
            create_device_credential(&alice, &laptop, capabilities(), None, 1_000, 60_000, 0)
                .expect("credential");
        let anchor = alice.trust_anchor();
        let revocations = RevocationSet::empty(alice.account_id(), 0);

        let mut tampered = credential.clone();
        tampered.signature[0] ^= 0xff;
        assert_eq!(
            verify_credential(&tampered, &anchor, &revocations, 1_001).unwrap_err(),
            IdentityError::InvalidSignature
        );

        let mut swapped = credential.clone();
        swapped.device_public_key = device(0x33).public_key();
        assert_eq!(
            verify_credential(&swapped, &anchor, &revocations, 1_001).unwrap_err(),
            IdentityError::DeviceIdMismatch
        );

        let mut widened = credential.clone();
        widened.capabilities =
            CapabilitySet::from_mask(0b0111_1000).expect("capabilities without direct/group");
        assert_eq!(
            verify_credential(&widened, &anchor, &revocations, 1_001).unwrap_err(),
            IdentityError::InvalidSignature
        );

        let mut repointed = credential.clone();
        repointed.endpoint = Some([0x5a; PUBLIC_KEY_BYTES]);
        assert_eq!(
            verify_credential(&repointed, &anchor, &revocations, 1_001).unwrap_err(),
            IdentityError::InvalidSignature
        );
    }

    #[test]
    fn cross_account_is_rejected() {
        let alice = account(0x11);
        let mallory = account(0x44);
        let laptop = device(0x22);
        let credential =
            create_device_credential(&alice, &laptop, capabilities(), None, 1_000, 60_000, 1)
                .expect("credential");
        let revocations = RevocationSet::empty(alice.account_id(), 1);
        assert_eq!(
            verify_credential(&credential, &mallory.trust_anchor(), &revocations, 1_001)
                .unwrap_err(),
            IdentityError::CrossAccountDevice
        );
        let other_account_revocations = RevocationSet::empty(mallory.account_id(), 1);
        assert_eq!(
            verify_credential(
                &credential,
                &alice.trust_anchor(),
                &other_account_revocations,
                1_001
            )
            .unwrap_err(),
            IdentityError::CrossAccountDevice
        );
    }

    #[test]
    fn expiry_and_clock_skew_are_enforced() {
        let alice = account(0x11);
        let laptop = device(0x22);
        let credential =
            create_device_credential(&alice, &laptop, capabilities(), None, 1_000, 60_000, 1)
                .expect("credential");
        let revocations = RevocationSet::empty(alice.account_id(), 1);
        let anchor = alice.trust_anchor();

        // 恰好在失效时间点即过期。
        assert_eq!(
            verify_credential(&credential, &anchor, &revocations, 61_000).unwrap_err(),
            IdentityError::CredentialExpired
        );
        assert!(verify_credential(&credential, &anchor, &revocations, 60_999).is_ok());

        // 允许 60 秒时钟偏差；再多就必须失败。
        let future =
            create_device_credential(&alice, &laptop, capabilities(), None, 1_000_000, 60_000, 1)
                .expect("credential");
        assert!(verify_credential(&future, &anchor, &revocations, 1_000_000 - 60_000).is_ok());
        assert_eq!(
            verify_credential(&future, &anchor, &revocations, 1_000_000 - 60_001).unwrap_err(),
            IdentityError::CredentialNotYetValid
        );
    }

    #[test]
    fn revocation_and_stale_generation_are_enforced() {
        let alice = account(0x11);
        let laptop = device(0x22);
        let credential =
            create_device_credential(&alice, &laptop, capabilities(), None, 1_000, 60_000, 1)
                .expect("credential");
        let device_id = laptop.device_id(alice.account_id());
        let revoked_devices = RevocationSet::new(alice.account_id(), 1, [device_id])
            .expect("revocations");
        assert_eq!(
            verify_credential(&credential, &alice.trust_anchor(), &revoked_devices, 1_001)
                .unwrap_err(),
            IdentityError::DeviceRevoked
        );
        let newer_generation = RevocationSet::empty(alice.account_id(), 2);
        assert_eq!(
            verify_credential(&credential, &alice.trust_anchor(), &newer_generation, 1_001)
                .unwrap_err(),
            IdentityError::CredentialStale
        );
    }

    #[test]
    fn credential_creation_bounds_are_enforced() {
        let alice = account(0x11);
        let laptop = device(0x22);
        assert_eq!(
            create_device_credential(&alice, &laptop, capabilities(), None, 0, 60_000, 0)
                .unwrap_err(),
            IdentityError::InvalidTimestamp
        );
        assert_eq!(
            create_device_credential(&alice, &laptop, capabilities(), None, 1_000, 0, 0)
                .unwrap_err(),
            IdentityError::InvalidCredential
        );
        assert_eq!(
            create_device_credential(
                &alice,
                &laptop,
                capabilities(),
                None,
                1_000,
                MAX_DEVICE_TTL_MS + 1,
                0
            )
            .unwrap_err(),
            IdentityError::InvalidCredential
        );
        assert_eq!(
            create_device_credential(&alice, &laptop, capabilities(), None, u64::MAX - 1, 60_000, 0)
                .unwrap_err(),
            IdentityError::InvalidCredential
        );
        assert_eq!(
            create_device_credential(&alice, &laptop, capabilities(), None, 1_000, 60_000, 0)
                .expect("credential")
                .expires_at_ms,
            61_000
        );
        assert_eq!(
            create_device_credential(&alice, &laptop, CapabilitySet::empty(), None, 1_000, 60_000, 0)
                .unwrap_err(),
            IdentityError::EmptyCapabilities
        );
    }

    #[test]
    fn revocation_set_is_sorted_deduplicated_and_bounded() {
        let alice = account(0x11);
        let first = DeviceId::from_bytes([0x01; 32]);
        let second = DeviceId::from_bytes([0x02; 32]);
        let set = RevocationSet::new(alice.account_id(), 1, [second, first, second])
            .expect("bounded revocations");
        assert_eq!(set.revoked(), &[first, second]);
        assert!(set.contains(first));

        let ids: Vec<DeviceId> = (0..MAX_REVOKED_DEVICES as u8)
            .map(|index| DeviceId::from_bytes([index; 32]))
            .collect();
        assert!(RevocationSet::new(alice.account_id(), 1, ids).is_ok());
        let over: Vec<DeviceId> = (0..=MAX_REVOKED_DEVICES as u8)
            .map(|index| DeviceId::from_bytes([index; 32]))
            .collect();
        assert!(matches!(
            RevocationSet::new(alice.account_id(), 1, over).unwrap_err(),
            IdentityError::TooManyRevocations { .. }
        ));
    }
}
