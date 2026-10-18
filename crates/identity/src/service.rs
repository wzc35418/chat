//! 身份门面：账户创建、脱敏视图、设备签发、邀请创建与联系人确认。
//!
//! 与计划书的对应关系（详见 docs/implementation/identity.md）：
//!
//! - IDENTITY-001：本机账户密钥/ID/指纹与初始设备凭据（纯本地部分）；
//! - IDENTITY-002：脱敏账户视图 [AccountView]（锁状态与数据库读取待 storage 接入）；
//! - IDENTITY-003：设备凭据校验（[crate::device::verify_credential]）；
//! - IDENTITY-004/005/006/007：邀请创建、邀请校验、联系人确认与指纹展示。
//!
//! 所有函数都不写数据库、不发网络请求：事务、outbox 与锁状态由 core 的
//! storage 集成任务补齐，此处不假装已经完成持久化。

use core::fmt;

use chat_protocol::{AccountId, CoordinatorId};

use crate::contact::Contact;
use crate::device::{CapabilitySet, DeviceCredential, create_device_credential};
use crate::error::{IdentityError, Result};
use crate::invite::{
    InviteExpectations, InviteScope, InviteToken, VerifiedInvite, create_invite, verify_invite,
};
use crate::keys::{AccountKeys, DeviceKeys, Fingerprint, PUBLIC_KEY_BYTES, TrustAnchor};

/// 显示名最大字节数（UTF-8）。
pub const MAX_DISPLAY_NAME_BYTES: usize = 64;

fn validate_display_name(display_name: &str) -> Result<()> {
    let trimmed = display_name.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_DISPLAY_NAME_BYTES {
        return Err(IdentityError::InvalidDisplayName);
    }
    if trimmed.chars().any(|character| character.is_control()) {
        return Err(IdentityError::InvalidDisplayName);
    }
    Ok(())
}

/// 本机账户：长期账户密钥 + 显示名。Debug 不打印任何密钥材料。
pub struct LocalAccount {
    account: AccountKeys,
    display_name: String,
    created_at_ms: u64,
}

impl LocalAccount {
    /// IDENTITY-001（本地部分）：建立账户根密钥并派生 ID/指纹。
    ///
    /// 显示名必须是 1..=64 字节的非空白文本且不含控制字符。
    pub fn create(display_name: &str, created_at_ms: u64) -> Result<Self> {
        validate_display_name(display_name)?;
        if created_at_ms == 0 {
            return Err(IdentityError::InvalidTimestamp);
        }
        Ok(Self {
            account: AccountKeys::generate(),
            display_name: display_name.trim().to_owned(),
            created_at_ms,
        })
    }

    /// 由固定种子恢复账户（测试/恢复路径；生产必须由受保护密钥存储提供种子）。
    pub fn from_seed(
        display_name: &str,
        created_at_ms: u64,
        seed: &[u8; crate::keys::KEY_SEED_BYTES],
    ) -> Result<Self> {
        validate_display_name(display_name)?;
        if created_at_ms == 0 {
            return Err(IdentityError::InvalidTimestamp);
        }
        Ok(Self {
            account: AccountKeys::from_seed(seed),
            display_name: display_name.trim().to_owned(),
            created_at_ms,
        })
    }

    /// 账户 ID。
    pub fn account_id(&self) -> AccountId {
        self.account.account_id()
    }

    /// 显示名（可改，不影响 ID）。
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// 创建时间（UTC 毫秒）。
    pub const fn created_at_ms(&self) -> u64 {
        self.created_at_ms
    }

    /// 账户人工核验指纹。
    pub fn fingerprint(&self) -> Fingerprint {
        self.account.fingerprint()
    }

    /// 账户信任锚。
    pub fn trust_anchor(&self) -> TrustAnchor {
        self.account.trust_anchor()
    }

    /// IDENTITY-007：分组展示的核验指纹。
    pub fn verification_fingerprint(&self) -> String {
        self.account.fingerprint().grouped()
    }

    /// IDENTITY-002（脱敏部分）：只含显示名/ID/指纹/设备数的账户视图。
    pub fn view(&self, active_devices: usize) -> AccountView {
        AccountView {
            account_id: self.account_id(),
            display_name: self.display_name.clone(),
            fingerprint: self.account.fingerprint(),
            active_devices,
        }
    }

    /// IDENTITY-001 步骤 4：签发初始/追加设备凭据。
    pub fn issue_device(
        &self,
        device: &DeviceKeys,
        capabilities: CapabilitySet,
        endpoint: Option<[u8; PUBLIC_KEY_BYTES]>,
        issued_at_ms: u64,
        ttl_ms: u64,
        revocation_generation: u64,
    ) -> Result<DeviceCredential> {
        create_device_credential(
            &self.account,
            device,
            capabilities,
            endpoint,
            issued_at_ms,
            ttl_ms,
            revocation_generation,
        )
    }

    /// IDENTITY-004：为本账户 + 指定设备 + 指定节点生成邀请口令。
    pub fn create_contact_invite(
        &self,
        device: &DeviceKeys,
        node: CoordinatorId,
        scope: InviteScope,
        issued_at_ms: u64,
        ttl_ms: u64,
    ) -> Result<InviteToken> {
        create_invite(&self.account, device, node, scope, issued_at_ms, ttl_ms)
    }
}

impl fmt::Debug for LocalAccount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalAccount")
            .field("account_id", &self.account.account_id())
            .field("display_name", &self.display_name)
            .field("created_at_ms", &self.created_at_ms)
            .field("keys", &"<redacted>")
            .finish()
    }
}

/// 脱敏账户视图：不含任何密钥字段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountView {
    account_id: AccountId,
    display_name: String,
    fingerprint: Fingerprint,
    active_devices: usize,
}

impl AccountView {
    /// 账户 ID。
    pub const fn account_id(&self) -> AccountId {
        self.account_id
    }

    /// 显示名。
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// 核验指纹。
    pub const fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    /// 分组指纹文本。
    pub fn fingerprint_text(&self) -> String {
        self.fingerprint.grouped()
    }

    /// 当前有效设备数。
    pub const fn active_devices(&self) -> usize {
        self.active_devices
    }
}

/// IDENTITY-002（脱敏部分）：把本机账户投影成无密钥视图。
///
/// 锁状态与数据库读取由 core 的 storage 集成补齐；本函数不做任何 I/O，
/// 也不会把「没有账户」和「存储被锁定」混为一谈（调用方需自己区分）。
pub fn current_account(
    account: Option<&LocalAccount>,
    active_devices: usize,
) -> Result<Option<AccountView>> {
    Ok(account.map(|account| account.view(active_devices)))
}

/// IDENTITY-007：为任意已固定信任锚返回分组核验指纹。
pub fn verification_fingerprint(anchor: &TrustAnchor) -> Result<String> {
    Ok(anchor.fingerprint().grouped())
}

/// IDENTITY-005 的结果：已验证邀请 + 待用户确认的联系人记录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactRequest {
    invite: VerifiedInvite,
    contact: Contact,
}

impl ContactRequest {
    /// 已验证邀请摘要。
    pub const fn invite(&self) -> &VerifiedInvite {
        &self.invite
    }

    /// 待确认联系人（状态恒为 Requested，未固定信任锚）。
    pub const fn contact(&self) -> &Contact {
        &self.contact
    }

    /// 可变联系人句柄：供用户确认/拒绝/记录身份变化。
    pub fn contact_mut(&mut self) -> &mut Contact {
        &mut self.contact
    }

    /// 取出联系人记录。
    pub fn into_contact(self) -> Contact {
        self.contact
    }

    /// 用本地可信时间检查该请求是否已过期。
    pub const fn is_expired_at(&self, now_ms: u64) -> bool {
        self.invite.is_expired_at(now_ms)
    }
}

/// IDENTITY-005：解析并校验邀请口令，建立**待确认**联系人请求。
///
/// 不会自动信任身份：联系人状态为 Requested，用户确认前不能私聊。
pub fn accept_contact_invite(
    token: &str,
    expectations: &InviteExpectations<'_>,
) -> Result<ContactRequest> {
    let invite = verify_invite(token, expectations)?;
    let contact = Contact::requested(invite.account_id(), invite.account_fingerprint());
    Ok(ContactRequest { invite, contact })
}

/// IDENTITY-006：用户核对指纹后确认联系人（固定信任锚）。
pub fn confirm_contact(contact: &mut Contact) -> Result<()> {
    contact.accept()
}

