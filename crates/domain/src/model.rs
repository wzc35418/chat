//! 领域模型：正式群快照、成员、操作与授权结果。
//!
//! 所有快照必须来自已验证正式日志；作者时间不参与权限排序。类型用不同名字表达
//! 「已授权」与「仅为意图」（`AuthorizedGroupOperation` 不是新快照）。

use chat_protocol::{AccountId, ControlHead, CoordinatorId, DeviceId, Digest, GroupId};

use crate::error::{DomainError, Result};

/// 单条消息完整规范序列化的上限（与协议层 64 KiB 一致）。
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// 会话类别。私聊/自同步共用协调顺序，但没有群治理入口。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversationKind {
    /// 一一私聊。
    Direct,
    /// 小群。
    Group,
    /// 本人多设备同步会话。
    SelfSync,
}

impl ConversationKind {
    /// 是否允许群治理操作。
    pub const fn allows_governance(self) -> bool {
        matches!(self, ConversationKind::Group)
    }
}

/// 群内角色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemberRole {
    /// 唯一群主。
    Owner,
    /// 管理员。
    Admin,
    /// 普通成员。
    Member,
}

impl MemberRole {
    /// 是否群主。
    pub const fn is_owner(self) -> bool {
        matches!(self, MemberRole::Owner)
    }

    /// 是否具备治理资格（群主或管理员）。
    pub const fn is_governor(self) -> bool {
        matches!(self, MemberRole::Owner | MemberRole::Admin)
    }
}

/// 正式状态暂停原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseReason {
    /// 验证失败。
    VerificationFailed,
    /// 日志/控制冲突。
    Conflict,
    /// 用户或节点主动暂停。
    Manual,
}

/// 成员在群内的序号窗口。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MembershipWindow {
    /// 加入后的第一个可见序号（从 1 开始）。
    pub joined_sequence: u64,
    /// 离开序号（不含）；仍在群内为 `None`。
    pub left_sequence: Option<u64>,
}

impl MembershipWindow {
    /// 仍在群内的窗口。
    pub const fn active(joined_sequence: u64) -> Self {
        Self {
            joined_sequence,
            left_sequence: None,
        }
    }

    /// 是否仍在群内。
    pub const fn is_active(&self) -> bool {
        self.left_sequence.is_none()
    }

    /// 序号是否落在窗口内：`joined <= seq < left`。
    pub fn contains(&self, sequence: u64) -> bool {
        if sequence < self.joined_sequence {
            return false;
        }
        match self.left_sequence {
            Some(left) => sequence < left,
            None => true,
        }
    }

    /// 校验窗口自身合法（用于快照不变量）。
    pub fn validate(&self) -> Result<()> {
        if self.joined_sequence == 0 {
            return Err(DomainError::InvalidSnapshot {
                detail: "membership window starts at 0",
            });
        }
        if let Some(left) = self.left_sequence
            && left <= self.joined_sequence
        {
            return Err(DomainError::InvalidSnapshot {
                detail: "membership window leaves before joining",
            });
        }
        Ok(())
    }
}

/// 正式群成员。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupMember {
    /// 账户。
    pub account_id: AccountId,
    /// 角色。
    pub role: MemberRole,
    /// 有效设备集合。
    pub devices: Vec<DeviceId>,
    /// 是否被禁言。
    pub muted: bool,
    /// 账户本身是否有效（撤销账户后为 false）。
    pub account_active: bool,
    /// 加入/退出窗口。
    pub window: MembershipWindow,
}

impl GroupMember {
    /// 是否持有该设备。
    pub fn has_device(&self, device: DeviceId) -> bool {
        self.devices.contains(&device)
    }

    /// 账户有效且仍在群内。
    pub fn is_active(&self) -> bool {
        self.account_active && self.window.is_active()
    }
}

/// 已验证正式日志产生的群快照。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupSnapshot {
    /// 协议群。
    pub id: GroupId,
    /// 会话类别。
    pub kind: ConversationKind,
    /// 唯一群主账户。
    pub owner: AccountId,
    /// 成员集合。
    pub members: Vec<GroupMember>,
    /// 正式控制头。
    pub control_head: ControlHead,
    /// 策略版本。
    pub policy_version: u64,
    /// MLS epoch。
    pub mls_epoch: u64,
    /// 已验证日志序号。
    pub verified_sequence: u64,
    /// 已验证日志头摘要。
    pub verified_log_hash: Digest,
    /// 固定协调节点。
    pub coordinator: CoordinatorId,
    /// 是否已解散。
    pub dissolved: bool,
    /// 暂停原因。
    pub paused_reason: Option<PauseReason>,
}

impl GroupSnapshot {
    /// 按账户查成员。
    pub fn member(&self, account_id: AccountId) -> Option<&GroupMember> {
        self.members
            .iter()
            .find(|member| member.account_id == account_id)
    }

    /// 当前群主成员。
    pub fn owner_member(&self) -> Option<&GroupMember> {
        self.members
            .iter()
            .find(|member| member.account_id == self.owner)
    }

    /// 角色为群主的成员数量。必须恒为 1。
    pub fn owner_count(&self) -> usize {
        self.members
            .iter()
            .filter(|member| member.role.is_owner())
            .count()
    }

    /// 校验快照不变量；任何违反都必须显式失败并停止该群分支。
    ///
    /// - 群主唯一，且与 `owner` 字段一致；
    /// - 群主是有效成员；
    /// - 账户唯一、单账户设备唯一、设备不跨账户共享；
    /// - 加入窗口合法。
    pub fn validate_invariants(&self) -> Result<()> {
        if self.members.is_empty() {
            return Err(DomainError::InvalidSnapshot {
                detail: "group has no members",
            });
        }
        if self.owner_count() != 1 {
            return Err(DomainError::InvalidSnapshot {
                detail: "group must have exactly one owner",
            });
        }
        let Some(owner_member) = self.owner_member() else {
            return Err(DomainError::InvalidSnapshot {
                detail: "owner field does not match a member",
            });
        };
        if !owner_member.role.is_owner() {
            return Err(DomainError::InvalidSnapshot {
                detail: "owner field points to a non-owner member",
            });
        }
        if !owner_member.is_active() {
            return Err(DomainError::InvalidSnapshot {
                detail: "owner is not an active member",
            });
        }
        let mut accounts: Vec<AccountId> = Vec::with_capacity(self.members.len());
        let mut all_devices: Vec<DeviceId> = Vec::new();
        for member in &self.members {
            member.window.validate()?;
            if accounts.contains(&member.account_id) {
                return Err(DomainError::InvalidSnapshot {
                    detail: "duplicate account in group snapshot",
                });
            }
            accounts.push(member.account_id);
            for device in &member.devices {
                if member.devices.iter().filter(|item| *item == device).count() > 1 {
                    return Err(DomainError::InvalidSnapshot {
                        detail: "duplicate device within one member",
                    });
                }
                if all_devices.contains(device) {
                    return Err(DomainError::InvalidSnapshot {
                        detail: "device shared by multiple members",
                    });
                }
                all_devices.push(*device);
            }
        }
        Ok(())
    }
}

/// 从可信凭据建立的操作者（账户 + 设备）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Actor {
    /// 账户。
    pub account_id: AccountId,
    /// 本次操作使用的设备。
    pub device_id: DeviceId,
}

/// 单一群操作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupOperation {
    /// 邀请新成员（携带其设备集合）。
    InviteMember {
        /// 新账户。
        account: AccountId,
        /// 新设备集合（至少 1 台）。
        devices: Vec<DeviceId>,
        /// 授予的角色（不能是群主）。
        role: MemberRole,
    },
    /// 移除成员。
    RemoveMember {
        /// 目标账户。
        account: AccountId,
    },
    /// 变更角色。
    SetRole {
        /// 目标账户。
        account: AccountId,
        /// 新角色（不能是群主）。
        role: MemberRole,
    },
    /// 设置禁言。
    SetMuted {
        /// 目标账户。
        account: AccountId,
        /// 是否禁言。
        muted: bool,
    },
    /// 群名变更（公开层只放摘要）。
    Rename {
        /// 新群名的规范摘要。
        name_digest: Digest,
    },
    /// 转让群主。
    TransferOwnership {
        /// 目标账户。
        target: AccountId,
    },
    /// 自己退出。
    Leave,
    /// 解散群。
    Dissolve,
}

impl GroupOperation {
    /// 冻结的操作名，用于日志与错误诊断。
    pub const fn op_name(&self) -> &'static str {
        match self {
            GroupOperation::InviteMember { .. } => "InviteMember",
            GroupOperation::RemoveMember { .. } => "RemoveMember",
            GroupOperation::SetRole { .. } => "SetRole",
            GroupOperation::SetMuted { .. } => "SetMuted",
            GroupOperation::Rename { .. } => "Rename",
            GroupOperation::TransferOwnership { .. } => "TransferOwnership",
            GroupOperation::Leave => "Leave",
            GroupOperation::Dissolve => "Dissolve",
        }
    }
}

/// 群规模上限。实际节点可能配置更低值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupLimits {
    /// 最大账户数。
    pub max_accounts: usize,
    /// 最大设备数。
    pub max_devices: usize,
    /// 单账户最大设备数。
    pub max_devices_per_account: usize,
}

impl GroupLimits {
    /// 基线：20 账户 / 40 设备 / 每账户 2 设备。
    pub const BASELINE: GroupLimits = GroupLimits {
        max_accounts: 20,
        max_devices: 40,
        max_devices_per_account: 2,
    };
}

/// 去重后的规模计数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capacity {
    /// 账户数。
    pub accounts: usize,
    /// 去重设备数。
    pub devices: usize,
    /// 单账户最大去重设备数。
    pub max_devices_per_account: usize,
}

impl Capacity {
    /// 按成员集合统计；重复设备只计一次，单账户设备重复也只计一次。
    pub fn of_members(members: &[GroupMember]) -> Capacity {
        let mut all_devices: Vec<DeviceId> = Vec::new();
        let mut max_per_account = 0usize;
        for member in members {
            let mut unique_devices: Vec<DeviceId> = Vec::new();
            for device in &member.devices {
                if !unique_devices.contains(device) {
                    unique_devices.push(*device);
                }
                if !all_devices.contains(device) {
                    all_devices.push(*device);
                }
            }
            max_per_account = max_per_account.max(unique_devices.len());
        }
        Capacity {
            accounts: members.len(),
            devices: all_devices.len(),
            max_devices_per_account: max_per_account,
        }
    }
}

/// 明确的角色迁移（before/after）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoleChange {
    /// 目标账户。
    pub account_id: AccountId,
    /// 变更前角色；新加入为 `None`。
    pub from: Option<MemberRole>,
    /// 变更后角色。
    pub to: MemberRole,
}

/// 已授权的发言上下文。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthorizedSend {
    /// 授权账户。
    pub account_id: AccountId,
    /// 授权设备。
    pub device_id: DeviceId,
    /// 当前控制头。
    pub control_head: ControlHead,
    /// 当前策略版本。
    pub policy_version: u64,
    /// 当前 MLS epoch。
    pub mls_epoch: u64,
    /// 本条消息允许的最大规范字节。
    pub max_content_bytes: usize,
}

/// 已授权的单一群操作。它不是新快照。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedGroupOperation {
    /// 授权时的控制头。
    pub snapshot_head: ControlHead,
    /// 授权时的策略版本。
    pub policy_version: u64,
    /// 操作者。
    pub actor: Actor,
    /// 被授权的单一操作。
    pub operation: GroupOperation,
    /// 该操作导致的明确角色迁移（可能为空）。
    pub role_changes: Vec<RoleChange>,
    /// 操作生效后的唯一群主。
    pub owner_after: AccountId,
    /// 操作生效后的规模。
    pub capacity_after: Capacity,
}
