//! IDENTITY-006：联系人状态机与固定信任锚。
//!
//! 允许的迁移（其余一律拒绝，不静默归一）：
//!
//! ~~~text
//! Requested      -> Accepted | Declined | Blocked | IdentityChanged
//! Accepted       -> Accepted(幂等) | Declined | Blocked | IdentityChanged
//! Declined       -> Requested | Blocked | IdentityChanged
//! Blocked        -> Declined            （解除拉黑后必须重新请求）
//! IdentityChanged-> Accepted | Declined | Blocked | IdentityChanged
//! ~~~
//!
//! 关键安全约束：
//!
//! - **Accepted 之前必须有观测指纹**，接受动作同时把该指纹固定为信任锚；
//! - 信任锚一旦固定，观测指纹与锚不一致 → 立即转为 IdentityChanged 并禁止发送；
//! - IdentityChanged 不能靠普通 accept 恢复：必须显式 confirm_identity_change，
//!   由用户重新核对新指纹后重新固定信任锚；
//! - Blocked 不能直接接受（必须先解除拉黑并重新发起请求）。

use chat_protocol::AccountId;

use crate::error::{IdentityError, Result};
use crate::keys::Fingerprint;

/// 联系人状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContactState {
    /// 已发起请求，等待用户确认。
    Requested = 1,
    /// 用户已确认，可正常私聊。
    Accepted = 2,
    /// 用户已拒绝。
    Declined = 3,
    /// 用户已拉黑。
    Blocked = 4,
    /// 对端身份指纹与固定信任锚不一致，必须重新核对。
    IdentityChanged = 5,
}

impl ContactState {
    /// 稳定名字（日志/诊断用）。
    pub const fn as_str(self) -> &'static str {
        match self {
            ContactState::Requested => "requested",
            ContactState::Accepted => "accepted",
            ContactState::Declined => "declined",
            ContactState::Blocked => "blocked",
            ContactState::IdentityChanged => "identity_changed",
        }
    }

    /// 该状态本身是否允许发送。真正的判定还要比对信任锚。
    pub const fn allows_send(self) -> bool {
        matches!(self, ContactState::Accepted)
    }
}

/// 记录身份指纹的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityUpdate {
    /// 与已记录指纹一致。
    Unchanged,
    /// 与已固定信任锚（或先前观测）不一致。
    Changed,
}

/// 迁移是否合法。冻结表，任何新增都要在计划书登记。
pub const fn transition_allowed(from: ContactState, to: ContactState) -> bool {
    use ContactState::{Accepted, Blocked, Declined, IdentityChanged, Requested};
    matches!(
        (from, to),
        (Requested, Requested)
            | (Requested, Accepted)
            | (Requested, Declined)
            | (Requested, Blocked)
            | (Requested, IdentityChanged)
            | (Accepted, Accepted)
            | (Accepted, Declined)
            | (Accepted, Blocked)
            | (Accepted, IdentityChanged)
            | (Declined, Requested)
            | (Declined, Blocked)
            | (Declined, Declined)
            | (Declined, IdentityChanged)
            | (Blocked, Declined)
            | (Blocked, Blocked)
            | (IdentityChanged, Accepted)
            | (IdentityChanged, IdentityChanged)
            | (IdentityChanged, Declined)
            | (IdentityChanged, Blocked)
    )
}

/// 本地联系人记录：对端账户、状态、固定信任锚与最近观测指纹。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contact {
    peer: AccountId,
    state: ContactState,
    pinned: Option<Fingerprint>,
    observed: Option<Fingerprint>,
}

impl Contact {
    /// 由收到邀请/请求建立待确认联系人（尚未固定信任锚）。
    pub fn requested(peer: AccountId, observed: Fingerprint) -> Self {
        Self {
            peer,
            state: ContactState::Requested,
            pinned: None,
            observed: Some(observed),
        }
    }

    /// 由已持久化的信任锚恢复（存储集成用；不接受未核验指纹）。
    pub fn trusted(peer: AccountId, pinned: Fingerprint) -> Self {
        Self {
            peer,
            state: ContactState::Accepted,
            pinned: Some(pinned),
            observed: Some(pinned),
        }
    }

    /// 对端账户。
    pub const fn peer(&self) -> AccountId {
        self.peer
    }

    /// 当前状态。
    pub const fn state(&self) -> ContactState {
        self.state
    }

    /// 已固定信任锚（仅在用户确认后存在）。
    pub const fn pinned_fingerprint(&self) -> Option<Fingerprint> {
        self.pinned
    }

    /// 最近观测到的对端指纹。
    pub const fn observed_fingerprint(&self) -> Option<Fingerprint> {
        self.observed
    }

    /// 是否允许向该联系人发送：状态为 Accepted 且观测指纹等于固定信任锚。
    pub fn can_send(&self) -> bool {
        match (self.pinned, self.observed) {
            (Some(pinned), Some(observed)) => self.state.allows_send() && pinned == observed,
            _ => false,
        }
    }

    /// 发送前的强制检查：不允许发送时返回稳定错误，调用方不得继续。
    pub fn require_sendable(&self) -> Result<()> {
        if self.state == ContactState::IdentityChanged {
            return Err(IdentityError::IdentityChanged);
        }
        if !self.can_send() {
            return Err(IdentityError::ContactNotAccepted);
        }
        Ok(())
    }

    fn transition(&mut self, to: ContactState) -> Result<()> {
        if !transition_allowed(self.state, to) {
            return Err(IdentityError::InvalidStateTransition {
                from: self.state,
                to,
            });
        }
        self.state = to;
        Ok(())
    }

    /// 记录一次对端身份观测（来自握手/凭据/邀请）。
    ///
    /// 与已固定信任锚（或先前观测）不一致时把状态转为 IdentityChanged 并
    /// 返回 [IdentityUpdate::Changed]；Blocked 保持 Blocked（本来就不能发送）。
    pub fn record_identity(&mut self, observed: Fingerprint) -> Result<IdentityUpdate> {
        let previous = self.pinned.or(self.observed);
        let changed = previous.is_some_and(|previous| previous != observed);
        self.observed = Some(observed);
        if changed && self.state != ContactState::Blocked {
            self.transition(ContactState::IdentityChanged)?;
        }
        Ok(if changed {
            IdentityUpdate::Changed
        } else {
            IdentityUpdate::Unchanged
        })
    }

    /// 用户确认联系人（IDENTITY-006）：固定当前观测指纹。
    ///
    /// - 没有观测指纹 → MissingIdentity；
    /// - 已固定信任锚与观测不一致 → 转 IdentityChanged 并返回 IdentityChanged；
    /// - 状态为 IdentityChanged → 必须走 confirm_identity_change，普通确认拒绝。
    pub fn accept(&mut self) -> Result<()> {
        let observed = self.observed.ok_or(IdentityError::MissingIdentity)?;
        if self.state == ContactState::IdentityChanged {
            return Err(IdentityError::InvalidStateTransition {
                from: self.state,
                to: ContactState::Accepted,
            });
        }
        if let Some(pinned) = self.pinned
            && pinned != observed
        {
            self.transition(ContactState::IdentityChanged)?;
            return Err(IdentityError::IdentityChanged);
        }
        self.transition(ContactState::Accepted)?;
        self.pinned = Some(observed);
        Ok(())
    }

    /// 用户重新核对身份变化后确认新指纹（唯一能从 IdentityChanged 回到 Accepted 的路径）。
    pub fn confirm_identity_change(&mut self) -> Result<()> {
        if self.state != ContactState::IdentityChanged {
            return Err(IdentityError::InvalidStateTransition {
                from: self.state,
                to: ContactState::Accepted,
            });
        }
        let observed = self.observed.ok_or(IdentityError::MissingIdentity)?;
        self.state = ContactState::Accepted;
        self.pinned = Some(observed);
        Ok(())
    }

    /// 重新发起请求（Declined → Requested；已在 Requested 时幂等）。
    pub fn request(&mut self, observed: Fingerprint) -> Result<()> {
        self.transition(ContactState::Requested)?;
        self.observed = Some(observed);
        Ok(())
    }

    /// 拒绝。
    pub fn decline(&mut self) -> Result<()> {
        self.transition(ContactState::Declined)
    }

    /// 拉黑。
    pub fn block(&mut self) -> Result<()> {
        self.transition(ContactState::Blocked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer() -> AccountId {
        AccountId::from_bytes([0x77; 32])
    }

    fn fingerprint(value: u8) -> Fingerprint {
        Fingerprint::from_bytes([value; 32])
    }

    #[test]
    fn requested_accept_pins_anchor_and_enables_send() {
        let mut contact = Contact::requested(peer(), fingerprint(0x10));
        assert_eq!(contact.state(), ContactState::Requested);
        assert!(!contact.can_send());
        assert_eq!(
            contact.require_sendable().unwrap_err(),
            IdentityError::ContactNotAccepted
        );
        contact.accept().expect("accept");
        assert_eq!(contact.state(), ContactState::Accepted);
        assert_eq!(contact.pinned_fingerprint(), Some(fingerprint(0x10)));
        assert!(contact.can_send());
        assert!(contact.require_sendable().is_ok());
        // 重复确认幂等。
        contact.accept().expect("idempotent accept");
        assert!(contact.can_send());
    }

    #[test]
    fn declined_can_be_requested_again_but_not_accepted_directly() {
        let mut contact = Contact::requested(peer(), fingerprint(0x10));
        contact.decline().expect("decline");
        assert_eq!(contact.state(), ContactState::Declined);
        assert_eq!(
            contact.accept().unwrap_err(),
            IdentityError::InvalidStateTransition {
                from: ContactState::Declined,
                to: ContactState::Accepted,
            }
        );
        contact.request(fingerprint(0x10)).expect("re-request");
        contact.accept().expect("accept after re-request");
        assert!(contact.can_send());
    }

    #[test]
    fn blocked_cannot_be_accepted_and_requires_unblock_then_request() {
        let mut contact = Contact::requested(peer(), fingerprint(0x10));
        contact.block().expect("block");
        assert_eq!(contact.state(), ContactState::Blocked);
        assert_eq!(
            contact.accept().unwrap_err(),
            IdentityError::InvalidStateTransition {
                from: ContactState::Blocked,
                to: ContactState::Accepted,
            }
        );
        assert_eq!(
            contact
                .request(fingerprint(0x10))
                .unwrap_err(),
            IdentityError::InvalidStateTransition {
                from: ContactState::Blocked,
                to: ContactState::Requested,
            }
        );
        contact.decline().expect("unblock to declined");
        contact.request(fingerprint(0x10)).expect("request");
        contact.accept().expect("accept");
        assert!(contact.can_send());
    }

    #[test]
    fn identity_change_blocks_sending_until_explicit_reconfirmation() {
        let mut contact = Contact::requested(peer(), fingerprint(0x10));
        contact.accept().expect("accept");
        assert!(contact.can_send());

        assert_eq!(
            contact.record_identity(fingerprint(0x99)).expect("record"),
            IdentityUpdate::Changed
        );
        assert_eq!(contact.state(), ContactState::IdentityChanged);
        assert!(!contact.can_send());
        assert_eq!(
            contact.require_sendable().unwrap_err(),
            IdentityError::IdentityChanged
        );
        // 普通确认不能绕过身份变化。
        assert_eq!(
            contact.accept().unwrap_err(),
            IdentityError::InvalidStateTransition {
                from: ContactState::IdentityChanged,
                to: ContactState::Accepted,
            }
        );
        // 显式重新核对方可恢复。
        contact.confirm_identity_change().expect("reconfirm");
        assert_eq!(contact.pinned_fingerprint(), Some(fingerprint(0x99)));
        assert!(contact.can_send());
    }

    #[test]
    fn same_fingerprint_is_unchanged_and_third_identity_is_tracked() {
        let mut contact = Contact::requested(peer(), fingerprint(0x10));
        assert_eq!(
            contact.record_identity(fingerprint(0x10)).expect("record"),
            IdentityUpdate::Unchanged
        );
        contact.accept().expect("accept");
        assert_eq!(
            contact.record_identity(fingerprint(0x10)).expect("record"),
            IdentityUpdate::Unchanged
        );
        assert!(contact.can_send());
        assert_eq!(
            contact.record_identity(fingerprint(0x11)).expect("record"),
            IdentityUpdate::Changed
        );
        assert_eq!(
            contact.record_identity(fingerprint(0x12)).expect("record"),
            IdentityUpdate::Changed
        );
        assert_eq!(contact.state(), ContactState::IdentityChanged);
    }

    #[test]
    fn accept_without_observed_identity_is_rejected() {
        // 仅 trusted() 会同时写入 pinned/observed；这里构造一个没有观测的退化记录。
        let mut contact = Contact {
            peer: peer(),
            state: ContactState::Requested,
            pinned: None,
            observed: None,
        };
        assert_eq!(
            contact.accept().unwrap_err(),
            IdentityError::MissingIdentity
        );
    }

    #[test]
    fn blocked_identity_change_is_recorded_but_stays_blocked() {
        let mut contact = Contact::requested(peer(), fingerprint(0x10));
        contact.block().expect("block");
        assert_eq!(
            contact.record_identity(fingerprint(0x99)).expect("record"),
            IdentityUpdate::Changed
        );
        assert_eq!(contact.state(), ContactState::Blocked);
        assert!(!contact.can_send());
    }

    #[test]
    fn trusted_restore_matches_fingerprint() {
        let mut contact = Contact::trusted(peer(), fingerprint(0x10));
        assert!(contact.can_send());
        assert_eq!(
            contact.record_identity(fingerprint(0x10)).expect("record"),
            IdentityUpdate::Unchanged
        );
        assert!(contact.can_send());
        assert_eq!(
            contact.record_identity(fingerprint(0x20)).expect("record"),
            IdentityUpdate::Changed
        );
        assert!(!contact.can_send());
    }
}
