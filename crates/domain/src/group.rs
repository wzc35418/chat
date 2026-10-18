//! DOMAIN-001/002/003/006/007：发言、群治理、容量、群主退出与转让。
//!
//! 本层是纯规则：不写本地正式成员、不查数据库、不推断授权。所有入口先校验
//! `GroupSnapshot` 不变量（群主唯一），任何违反都显式失败并停止该群分支。

use chat_protocol::{AccountId, Digest};

use crate::error::{DomainError, Result};
use crate::model::{
    Actor, AuthorizedGroupOperation, AuthorizedSend, Capacity, GroupLimits, GroupMember,
    GroupOperation, GroupSnapshot, MAX_MESSAGE_BYTES, MemberRole, RoleChange,
};

/// DOMAIN-003：校验账户数、设备数与单账户设备数。
///
/// 先做安全加法（溢出即拒绝），再逐维比较：
/// 账户超限 → `GROUP_LIMIT_EXCEEDED`；设备维超限 → `DEVICE_LIMIT_EXCEEDED`。
/// 19→20 接受，20→21 拒绝；重复设备由 [`Capacity::of_members`] 去重后再计数。
pub fn validate_capacity(
    accounts: usize,
    devices: usize,
    per_account: usize,
    limits: GroupLimits,
) -> Result<()> {
    accounts
        .checked_add(devices)
        .and_then(|total| total.checked_add(per_account))
        .ok_or(DomainError::GroupLimitExceeded {
            detail: "capacity arithmetic overflow",
        })?;
    if accounts > limits.max_accounts {
        return Err(DomainError::GroupLimitExceeded {
            detail: "account limit",
        });
    }
    if devices > limits.max_devices {
        return Err(DomainError::DeviceLimitExceeded {
            detail: "device limit",
        });
    }
    if per_account > limits.max_devices_per_account {
        return Err(DomainError::DeviceLimitExceeded {
            detail: "per-account device limit",
        });
    }
    Ok(())
}

/// 按成员集合统计容量后校验基线限额。
pub fn validate_snapshot_capacity(
    snapshot: &GroupSnapshot,
    limits: GroupLimits,
) -> Result<Capacity> {
    let capacity = Capacity::of_members(&snapshot.members);
    validate_capacity(
        capacity.accounts,
        capacity.devices,
        capacity.max_devices_per_account,
        limits,
    )?;
    Ok(capacity)
}

fn require_member(snapshot: &GroupSnapshot, actor: Actor) -> Result<&GroupMember> {
    let member = snapshot
        .member(actor.account_id)
        .ok_or(DomainError::PermissionDenied {
            detail: "actor is not a group member",
        })?;
    if !member.has_device(actor.device_id) {
        return Err(DomainError::DeviceRevoked);
    }
    if !member.account_active {
        return Err(DomainError::PermissionDenied {
            detail: "actor account is revoked",
        });
    }
    if !member.window.is_active() {
        return Err(DomainError::PermissionDenied {
            detail: "actor has left the group",
        });
    }
    Ok(member)
}

fn require_governor(member: &GroupMember) -> Result<()> {
    if !member.role.is_governor() {
        return Err(DomainError::PermissionDenied {
            detail: "operation requires owner or admin",
        });
    }
    Ok(())
}

fn capacity_after_invite(
    snapshot: &GroupSnapshot,
    devices: usize,
    limits: GroupLimits,
) -> Result<Capacity> {
    let current = Capacity::of_members(&snapshot.members);
    let accounts = current
        .accounts
        .checked_add(1)
        .ok_or(DomainError::GroupLimitExceeded {
            detail: "account limit",
        })?;
    let total_devices =
        current
            .devices
            .checked_add(devices)
            .ok_or(DomainError::GroupLimitExceeded {
                detail: "device limit",
            })?;
    let per_account = current.max_devices_per_account.max(devices);
    validate_capacity(accounts, total_devices, per_account, limits)?;
    Ok(Capacity {
        accounts,
        devices: total_devices,
        max_devices_per_account: per_account,
    })
}

/// DOMAIN-001：校验发言权限并返回当前 head/epoch/policy 上下文。
///
/// 顺序：快照不变量 → 未解散 → 未暂停 → 设备在成员集合 → 账户有效且未禁言 →
/// 内容大小。禁言后旧草稿同样失败（每次都重新读取快照）。
pub fn validate_send_permission(
    snapshot: &GroupSnapshot,
    actor: Actor,
    size: usize,
) -> Result<AuthorizedSend> {
    snapshot.validate_invariants()?;
    if snapshot.dissolved {
        return Err(DomainError::PermissionDenied {
            detail: "group is dissolved",
        });
    }
    if snapshot.paused_reason.is_some() {
        return Err(DomainError::PermissionDenied {
            detail: "group is paused",
        });
    }
    let member = require_member(snapshot, actor)?;
    if member.muted {
        return Err(DomainError::PermissionDenied {
            detail: "actor is muted",
        });
    }
    if size > MAX_MESSAGE_BYTES {
        return Err(DomainError::ContentTooLarge {
            size,
            limit: MAX_MESSAGE_BYTES,
        });
    }
    Ok(AuthorizedSend {
        account_id: actor.account_id,
        device_id: actor.device_id,
        control_head: snapshot.control_head,
        policy_version: snapshot.policy_version,
        mls_epoch: snapshot.mls_epoch,
        max_content_bytes: MAX_MESSAGE_BYTES,
    })
}

/// DOMAIN-002：授权单一群操作。
///
/// 规则要点：
///
/// - 管理员只能治理普通成员：不能踢/降级/禁言管理员或群主，不能改角色、不能邀请管理员；
/// - 任何人都不能给自己升权（自改角色一律拒绝），群主降级必须走转让；
/// - 任命群主只能通过 `TransferOwnership`；
/// - 私聊/自同步会话没有群治理入口，只允许退出/解散；
/// - 邀请受基线容量约束（`GroupLimits::BASELINE`）；节点配置更低限额时，
///   节点必须另行调用 [`validate_capacity`]。
pub fn authorize_group_operation(
    snapshot: &GroupSnapshot,
    actor: Actor,
    operation: GroupOperation,
) -> Result<AuthorizedGroupOperation> {
    snapshot.validate_invariants()?;
    if snapshot.dissolved {
        return Err(DomainError::PermissionDenied {
            detail: "group is dissolved",
        });
    }
    let actor_member = require_member(snapshot, actor)?;
    if actor_member.muted {
        return Err(DomainError::PermissionDenied {
            detail: "muted actor cannot control the group",
        });
    }
    if !snapshot.kind.allows_governance()
        && !matches!(operation, GroupOperation::Leave | GroupOperation::Dissolve)
    {
        return Err(DomainError::PermissionDenied {
            detail: "conversation kind has no group governance",
        });
    }

    let mut role_changes: Vec<RoleChange> = Vec::new();
    let owner_after = snapshot.owner;
    let capacity_after;

    match &operation {
        GroupOperation::InviteMember {
            account,
            devices,
            role,
        } => {
            require_governor(actor_member)?;
            if role.is_owner() {
                return Err(DomainError::PermissionDenied {
                    detail: "only TransferOwnership appoints an owner",
                });
            }
            if actor_member.role == MemberRole::Admin && *role != MemberRole::Member {
                return Err(DomainError::PermissionDenied {
                    detail: "admin can only invite ordinary members",
                });
            }
            if snapshot.member(*account).is_some() {
                return Err(DomainError::InvalidTarget {
                    detail: "account is already a member",
                });
            }
            if devices.is_empty() {
                return Err(DomainError::InvalidTarget {
                    detail: "invited account has no devices",
                });
            }
            for (index, device) in devices.iter().enumerate() {
                if devices.iter().skip(index + 1).any(|other| other == device) {
                    return Err(DomainError::InvalidTarget {
                        detail: "duplicate device in invite",
                    });
                }
                if snapshot
                    .members
                    .iter()
                    .any(|member| member.has_device(*device))
                {
                    return Err(DomainError::InvalidTarget {
                        detail: "device already belongs to another member",
                    });
                }
            }
            capacity_after = capacity_after_invite(snapshot, devices.len(), GroupLimits::BASELINE)?;
            role_changes.push(RoleChange {
                account_id: *account,
                from: None,
                to: *role,
            });
        }
        GroupOperation::RemoveMember { account } => {
            require_governor(actor_member)?;
            if *account == actor.account_id {
                return Err(DomainError::PermissionDenied {
                    detail: "use Leave for self removal",
                });
            }
            let target = snapshot
                .member(*account)
                .ok_or(DomainError::InvalidTarget {
                    detail: "target is not a member",
                })?;
            if target.role.is_owner() {
                return Err(DomainError::PermissionDenied {
                    detail: "owner cannot be removed; transfer or dissolve",
                });
            }
            if actor_member.role == MemberRole::Admin && target.role != MemberRole::Member {
                return Err(DomainError::PermissionDenied {
                    detail: "admin can only govern ordinary members",
                });
            }
            let remaining: Vec<GroupMember> = snapshot
                .members
                .iter()
                .filter(|member| member.account_id != *account)
                .cloned()
                .collect();
            capacity_after = Capacity::of_members(&remaining);
        }
        GroupOperation::SetRole { account, role } => {
            if !actor_member.role.is_owner() {
                return Err(DomainError::PermissionDenied {
                    detail: "role changes require the owner",
                });
            }
            if *role == MemberRole::Owner {
                return Err(DomainError::PermissionDenied {
                    detail: "only TransferOwnership appoints an owner",
                });
            }
            if *account == actor.account_id {
                return Err(DomainError::PermissionDenied {
                    detail: "cannot change own role",
                });
            }
            let target = snapshot
                .member(*account)
                .ok_or(DomainError::InvalidTarget {
                    detail: "target is not a member",
                })?;
            if target.role.is_owner() {
                return Err(DomainError::PermissionDenied {
                    detail: "cannot change the owner role",
                });
            }
            capacity_after = Capacity::of_members(&snapshot.members);
            if target.role != *role {
                role_changes.push(RoleChange {
                    account_id: *account,
                    from: Some(target.role),
                    to: *role,
                });
            }
        }
        GroupOperation::SetMuted { account, muted } => {
            require_governor(actor_member)?;
            if *account == actor.account_id {
                return Err(DomainError::PermissionDenied {
                    detail: "cannot mute self",
                });
            }
            let target = snapshot
                .member(*account)
                .ok_or(DomainError::InvalidTarget {
                    detail: "target is not a member",
                })?;
            if actor_member.role == MemberRole::Admin && target.role != MemberRole::Member {
                return Err(DomainError::PermissionDenied {
                    detail: "admin can only govern ordinary members",
                });
            }
            let _ = muted;
            capacity_after = Capacity::of_members(&snapshot.members);
        }
        GroupOperation::Rename { name_digest } => {
            require_governor(actor_member)?;
            if *name_digest == Digest::ZERO {
                return Err(DomainError::InvalidTarget {
                    detail: "new name digest is empty",
                });
            }
            capacity_after = Capacity::of_members(&snapshot.members);
        }
        GroupOperation::TransferOwnership { target } => {
            let authorized = validate_ownership_transfer(snapshot, actor, *target)?;
            return Ok(authorized);
        }
        GroupOperation::Leave => {
            if actor_member.role.is_owner() {
                return Err(DomainError::OwnerTransferRequired);
            }
            capacity_after = Capacity::of_members(&snapshot.members);
        }
        GroupOperation::Dissolve => {
            if !actor_member.role.is_owner() {
                return Err(DomainError::PermissionDenied {
                    detail: "dissolve requires the owner",
                });
            }
            capacity_after = Capacity::of_members(&snapshot.members);
        }
    }

    Ok(AuthorizedGroupOperation {
        snapshot_head: snapshot.control_head,
        policy_version: snapshot.policy_version,
        actor,
        operation,
        role_changes,
        owner_after,
        capacity_after,
    })
}

/// 群主退出模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitMode {
    /// 成员可直接退出。
    Leave,
    /// 群主必须先把群主转让给其他活跃成员。
    OwnerMustTransfer,
    /// 群主是最后一名成员时，必须显式解散，不能隐式退出。
    OwnerMustDissolve,
}

/// DOMAIN-006：判断成员退出模式。
///
/// 群主的退出永远不会被推断为「最后一人自动离开」：仍有人时要求转让，
/// 只剩自己时要求显式解散确认。
pub fn validate_owner_exit(snapshot: &GroupSnapshot, actor: AccountId) -> Result<ExitMode> {
    snapshot.validate_invariants()?;
    if snapshot.dissolved {
        return Err(DomainError::PermissionDenied {
            detail: "group is dissolved",
        });
    }
    let member = snapshot.member(actor).ok_or(DomainError::NotMember)?;
    if !member.account_active || !member.window.is_active() {
        return Err(DomainError::NotMember);
    }
    if member.role.is_owner() {
        if snapshot.members.len() == 1 {
            Ok(ExitMode::OwnerMustDissolve)
        } else {
            Ok(ExitMode::OwnerMustTransfer)
        }
    } else {
        Ok(ExitMode::Leave)
    }
}

/// DOMAIN-007：授权群主转让。
///
/// 结果是「目标成为唯一群主、原群主降为普通成员」的单一操作；转让不会让原群主
/// 自动退出。旧群主重试时按当前快照重新校验，若已不是群主则 `PERMISSION_DENIED`。
pub fn validate_ownership_transfer(
    snapshot: &GroupSnapshot,
    actor: Actor,
    target: AccountId,
) -> Result<AuthorizedGroupOperation> {
    snapshot.validate_invariants()?;
    if snapshot.dissolved {
        return Err(DomainError::PermissionDenied {
            detail: "group is dissolved",
        });
    }
    let actor_member = require_member(snapshot, actor)?;
    if actor.account_id != snapshot.owner || !actor_member.role.is_owner() {
        return Err(DomainError::PermissionDenied {
            detail: "only the owner can transfer ownership",
        });
    }
    if target == actor.account_id {
        return Err(DomainError::InvalidTarget {
            detail: "cannot transfer ownership to self",
        });
    }
    let target_member = snapshot.member(target).ok_or(DomainError::InvalidTarget {
        detail: "target is not a member",
    })?;
    if !target_member.account_active || !target_member.window.is_active() {
        return Err(DomainError::InvalidTarget {
            detail: "target is not an active member",
        });
    }
    if target_member.devices.is_empty() {
        return Err(DomainError::InvalidTarget {
            detail: "target has no active device",
        });
    }
    if target_member.role.is_owner() {
        return Err(DomainError::InvalidTarget {
            detail: "target is already the owner",
        });
    }
    let role_changes = vec![
        RoleChange {
            account_id: target,
            from: Some(target_member.role),
            to: MemberRole::Owner,
        },
        RoleChange {
            account_id: actor.account_id,
            from: Some(MemberRole::Owner),
            to: MemberRole::Member,
        },
    ];
    Ok(AuthorizedGroupOperation {
        snapshot_head: snapshot.control_head,
        policy_version: snapshot.policy_version,
        actor,
        operation: GroupOperation::TransferOwnership { target },
        role_changes,
        owner_after: target,
        capacity_after: Capacity::of_members(&snapshot.members),
    })
}
