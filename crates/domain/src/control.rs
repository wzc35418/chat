//! DOMAIN-004/005：策略操作应用与控制结果核对。
//!
//! 两个函数都是纯函数，且**不产生部分状态**：
//!
//! - [`apply_policy_operation`] 先按当前快照重新授权（`authorize_group_operation`），
//!   再只应用该单一操作，输出候选快照与设备增删差集；它不是正式快照。
//! - [`verify_control_result`] 先用公开控制元数据与已授权操作对上，再把 MLS Commit
//!   的实际叶子增删、epoch 与预期业务结果逐项比对；任一不一致都不返回结论。
//!
//! 冻结的派生公式（P1-01 补充）：
//!
//! - `policy_operation_digest` = SHA-256(域 || 规范编码的操作细节)；
//! - `control_head_after` = SHA-256(域 || 规范编码{前一控制头, 新策略版本, 操作种类, 操作摘要})。

use chat_protocol::{
    AccountId, ControlHead, ControlMetadata, ControlOperation, Digest, ProtocolVersion, WireField,
    WireValue, encode_canonical, field,
};

use crate::error::{DomainError, Result};
use crate::group::authorize_group_operation;
use crate::model::{
    AuthorizedGroupOperation, CredentialLeaf, DeviceSetDiff, GroupMember, GroupOperation,
    GroupSnapshot, MemberRole, MembershipWindow, ProposedPolicy, StagedCommit,
    VerifiedControlResult,
};

/// 策略操作摘要用途域。
pub const POLICY_OPERATION_DOMAIN: &[u8] = b"chat/p1/policy-operation/v1";
/// 控制头派生用途域。
pub const CONTROL_HEAD_DOMAIN: &[u8] = b"chat/p1/control-head/v1";
const SCHEMA_VERSION: u64 = 1;

/// 把领域操作映射为公开控制操作种类。
pub const fn control_operation_of(operation: &GroupOperation) -> ControlOperation {
    match operation {
        GroupOperation::InviteMember { .. } => ControlOperation::Invite,
        GroupOperation::RemoveMember { .. } => ControlOperation::Remove,
        GroupOperation::SetRole { .. } => ControlOperation::SetRole,
        GroupOperation::SetMuted { .. } => ControlOperation::SetMuted,
        GroupOperation::Rename { .. } => ControlOperation::Rename,
        GroupOperation::TransferOwnership { .. } => ControlOperation::TransferOwnership,
        GroupOperation::Leave => ControlOperation::Leave,
        GroupOperation::Dissolve => ControlOperation::Dissolve,
    }
}

fn optional_id(value: Option<&[u8; 32]>) -> WireValue {
    WireValue::optional(value.map(WireValue::id_bytes))
}

fn operation_body(operation: &GroupOperation) -> WireValue {
    let mut fields = vec![
        WireField::new(field::SCHEMA_VERSION, WireValue::U64(SCHEMA_VERSION)),
        WireField::new(
            field::CONTROL_OPERATION,
            WireValue::U64(control_operation_of(operation).as_u64()),
        ),
    ];
    match operation {
        GroupOperation::InviteMember {
            account,
            devices,
            role,
        } => {
            fields.push(WireField::new(
                field::POLICY_OPERATION_ACCOUNT,
                optional_id(Some(account.as_bytes())),
            ));
            fields.push(WireField::new(
                field::POLICY_OPERATION_ROLE,
                WireValue::optional(Some(WireValue::U64(role.as_u64()))),
            ));
            fields.push(WireField::new(
                field::POLICY_OPERATION_DEVICES,
                WireValue::optional(Some(WireValue::List(
                    devices
                        .iter()
                        .map(|device| WireValue::id_bytes(device.as_bytes()))
                        .collect(),
                ))),
            ));
        }
        GroupOperation::RemoveMember { account } => {
            fields.push(WireField::new(
                field::POLICY_OPERATION_ACCOUNT,
                optional_id(Some(account.as_bytes())),
            ));
        }
        GroupOperation::SetRole { account, role } => {
            fields.push(WireField::new(
                field::POLICY_OPERATION_ACCOUNT,
                optional_id(Some(account.as_bytes())),
            ));
            fields.push(WireField::new(
                field::POLICY_OPERATION_ROLE,
                WireValue::optional(Some(WireValue::U64(role.as_u64()))),
            ));
        }
        GroupOperation::SetMuted { account, muted } => {
            fields.push(WireField::new(
                field::POLICY_OPERATION_ACCOUNT,
                optional_id(Some(account.as_bytes())),
            ));
            fields.push(WireField::new(
                field::POLICY_OPERATION_MUTED,
                WireValue::optional(Some(WireValue::U64(u64::from(*muted)))),
            ));
        }
        GroupOperation::Rename { name_digest } => {
            fields.push(WireField::new(
                field::POLICY_OPERATION_NAME_DIGEST,
                WireValue::optional(Some(WireValue::digest(*name_digest))),
            ));
        }
        GroupOperation::TransferOwnership { target } => {
            fields.push(WireField::new(
                field::POLICY_OPERATION_ACCOUNT,
                optional_id(Some(target.as_bytes())),
            ));
        }
        GroupOperation::Leave | GroupOperation::Dissolve => {}
    }
    WireValue::Struct(fields)
}

/// 冻结的操作摘要：绑定操作种类与全部目标细节。
pub fn policy_operation_digest(operation: &GroupOperation) -> Result<Digest> {
    let encoded = encode_canonical(&operation_body(operation), ProtocolVersion::V1)?;
    let mut preimage = Vec::with_capacity(POLICY_OPERATION_DOMAIN.len() + encoded.len());
    preimage.extend_from_slice(POLICY_OPERATION_DOMAIN);
    preimage.extend_from_slice(encoded.as_bytes());
    Ok(Digest::sha256(&preimage))
}

/// 冻结的控制头派生：前一控制头 + 新策略版本 + 操作种类 + 操作摘要。
pub fn control_head_after(
    previous: ControlHead,
    policy_version_after: u64,
    operation: &GroupOperation,
) -> Result<ControlHead> {
    let target_digest = policy_operation_digest(operation)?;
    let body = WireValue::structure([
        WireField::new(field::SCHEMA_VERSION, WireValue::U64(SCHEMA_VERSION)),
        WireField::new(
            field::CONTROL_HEAD,
            WireValue::id_bytes(previous.as_bytes()),
        ),
        WireField::new(field::POLICY_VERSION, WireValue::U64(policy_version_after)),
        WireField::new(
            field::CONTROL_OPERATION,
            WireValue::U64(control_operation_of(operation).as_u64()),
        ),
        WireField::new(
            field::CONTROL_TARGET_DIGEST,
            WireValue::digest(target_digest),
        ),
    ]);
    let encoded = encode_canonical(&body, ProtocolVersion::V1)?;
    let mut preimage = Vec::with_capacity(CONTROL_HEAD_DOMAIN.len() + encoded.len());
    preimage.extend_from_slice(CONTROL_HEAD_DOMAIN);
    preimage.extend_from_slice(encoded.as_bytes());
    let digest = Digest::sha256(&preimage);
    Ok(ControlHead::from_bytes(*digest.as_bytes()))
}

fn member_mut(policy: &mut GroupSnapshot, account_id: AccountId) -> Option<&mut GroupMember> {
    policy
        .members
        .iter_mut()
        .find(|member| member.account_id == account_id)
}

fn remove_member(policy: &mut GroupSnapshot, account_id: AccountId) -> Result<GroupMember> {
    let position = policy
        .members
        .iter()
        .position(|member| member.account_id == account_id)
        .ok_or(DomainError::InvalidPolicyTransition {
            detail: "remove target is not a member",
        })?;
    Ok(policy.members.remove(position))
}

fn leaves_of(account_id: AccountId, devices: &[chat_protocol::DeviceId]) -> Vec<CredentialLeaf> {
    devices
        .iter()
        .map(|device| CredentialLeaf::new(account_id, *device))
        .collect()
}

/// DOMAIN-004：应用单一已授权策略操作。
///
/// 顺序：快照不变量 → 未解散 → head/policy 匹配 → 重新授权并与传入的授权结果比对
/// → 只应用该操作 → 校验唯一群主不变量 → 递增策略版本并派生新控制头。
/// 失败不返回任何候选状态；`op` 与当前快照不一致时 `STALE_CONTROL_HEAD`，
/// 授权/迁移不一致时 `INVALID_POLICY_TRANSITION`。
pub fn apply_policy_operation(
    before: &GroupSnapshot,
    op: &AuthorizedGroupOperation,
) -> Result<ProposedPolicy> {
    before.validate_invariants()?;
    if before.dissolved {
        return Err(DomainError::InvalidPolicyTransition {
            detail: "group is dissolved",
        });
    }
    if op.snapshot_head != before.control_head || op.policy_version != before.policy_version {
        return Err(DomainError::StaleControlHead {
            detail: "authorized operation is based on a stale control head",
        });
    }
    // 重新授权：传入的授权结果必须仍然等价于「按当前快照授权同一操作」的结果。
    let derived = authorize_group_operation(before, op.actor, op.operation.clone())?;
    if derived.operation != op.operation
        || derived.role_changes != op.role_changes
        || derived.owner_after != op.owner_after
    {
        return Err(DomainError::InvalidPolicyTransition {
            detail: "authorized operation does not match current permissions",
        });
    }

    let mut policy = before.clone();
    let mut device_changes = DeviceSetDiff::default();
    match &op.operation {
        GroupOperation::InviteMember {
            account,
            devices,
            role,
        } => {
            if *role == MemberRole::Owner {
                return Err(DomainError::InvalidPolicyTransition {
                    detail: "invite cannot grant ownership",
                });
            }
            let joined_sequence = before.verified_sequence.saturating_add(1);
            policy.members.push(GroupMember {
                account_id: *account,
                role: *role,
                devices: devices.clone(),
                muted: false,
                account_active: true,
                window: MembershipWindow::active(joined_sequence),
            });
            device_changes.added = leaves_of(*account, devices);
        }
        GroupOperation::RemoveMember { account } => {
            let removed = remove_member(&mut policy, *account)?;
            device_changes.removed = leaves_of(*account, &removed.devices);
        }
        GroupOperation::SetRole { account, role } => {
            if *role == MemberRole::Owner {
                return Err(DomainError::InvalidPolicyTransition {
                    detail: "role change cannot grant ownership; use TransferOwnership",
                });
            }
            let member =
                member_mut(&mut policy, *account).ok_or(DomainError::InvalidPolicyTransition {
                    detail: "role change target is not a member",
                })?;
            member.role = *role;
        }
        GroupOperation::SetMuted { account, muted } => {
            let member =
                member_mut(&mut policy, *account).ok_or(DomainError::InvalidPolicyTransition {
                    detail: "mute target is not a member",
                })?;
            member.muted = *muted;
        }
        GroupOperation::Rename { name_digest } => {
            if *name_digest == Digest::ZERO {
                return Err(DomainError::InvalidPolicyTransition {
                    detail: "new name digest is empty",
                });
            }
        }
        GroupOperation::TransferOwnership { target } => {
            for member in &mut policy.members {
                if member.account_id == before.owner {
                    member.role = MemberRole::Member;
                } else if member.account_id == *target {
                    member.role = MemberRole::Owner;
                }
            }
            policy.owner = *target;
        }
        GroupOperation::Leave => {
            let account = op.actor.account_id;
            let removed = remove_member(&mut policy, account)?;
            device_changes.removed = leaves_of(account, &removed.devices);
        }
        GroupOperation::Dissolve => {
            policy.dissolved = true;
        }
    }

    // 唯一群主不变量必须始终成立。
    match policy.owner_member() {
        Some(owner_member)
            if policy.owner_count() == 1
                && owner_member.account_id == policy.owner
                && owner_member.role.is_owner() => {}
        _ => {
            return Err(DomainError::InvalidPolicyTransition {
                detail: "policy would violate the single-owner invariant",
            });
        }
    }
    policy.policy_version =
        before
            .policy_version
            .checked_add(1)
            .ok_or(DomainError::InvalidPolicyTransition {
                detail: "policy version overflow",
            })?;
    policy.control_head =
        control_head_after(before.control_head, policy.policy_version, &op.operation)?;
    policy
        .validate_invariants()
        .map_err(|_| DomainError::InvalidPolicyTransition {
            detail: "resulting snapshot violates invariants",
        })?;

    Ok(ProposedPolicy {
        policy,
        device_changes,
        role_changes: derived.role_changes,
        owner_after: derived.owner_after,
    })
}

fn same_leaves(actual: &[CredentialLeaf], expected: &[CredentialLeaf]) -> bool {
    let mut actual: Vec<CredentialLeaf> = actual.to_vec();
    let mut expected: Vec<CredentialLeaf> = expected.to_vec();
    actual.sort();
    expected.sort();
    actual == expected
}

/// DOMAIN-005：核对控制结果与 MLS Commit 设备成员变化一致。
///
/// 顺序：群/epoch 与本地快照一致（否则 `LOG_CONFLICT`）→ epoch 必须前进 →
/// 公开控制元数据与已授权操作一致（否则 `INVALID_CONTROL_RESULT`）→
/// 重新授权并应用出预期业务结果 → 实际 added/removed 叶子必须与预期完全一致。
/// 纯角色操作（改名/禁言/角色）不允许夹带任何成员变化。
pub fn verify_control_result(
    before: &GroupSnapshot,
    metadata: ControlMetadata,
    commit: &StagedCommit,
) -> Result<VerifiedControlResult> {
    if commit.group_id != before.id {
        return Err(DomainError::LogConflict {
            detail: "staged commit belongs to another group",
        });
    }
    if commit.epoch_before != before.mls_epoch {
        return Err(DomainError::LogConflict {
            detail: "staged commit epoch does not match the local snapshot",
        });
    }
    if commit.epoch_after <= commit.epoch_before {
        return Err(DomainError::InvalidControlResult {
            detail: "commit must advance the mls epoch",
        });
    }
    let expected_kind = control_operation_of(&commit.operation.operation);
    if metadata.operation != expected_kind {
        return Err(DomainError::InvalidControlResult {
            detail: "control metadata operation does not match the staged commit",
        });
    }
    if metadata.target_digest != policy_operation_digest(&commit.operation.operation)? {
        return Err(DomainError::InvalidControlResult {
            detail: "control metadata digest does not match the operation",
        });
    }
    // 重新校验原作者权限并生成预期业务结果。
    let proposed = apply_policy_operation(before, &commit.operation)?;
    if !same_leaves(&commit.added, &proposed.device_changes.added) {
        return Err(DomainError::InvalidControlResult {
            detail: "added leaves do not match the authorized change",
        });
    }
    if !same_leaves(&commit.removed, &proposed.device_changes.removed) {
        return Err(DomainError::InvalidControlResult {
            detail: "removed leaves do not match the authorized change",
        });
    }
    Ok(VerifiedControlResult {
        policy: proposed,
        added_devices: commit.added.iter().map(|leaf| leaf.device_id).collect(),
        removed_devices: commit.removed.iter().map(|leaf| leaf.device_id).collect(),
        epoch_before: commit.epoch_before,
        epoch_after: commit.epoch_after,
    })
}
