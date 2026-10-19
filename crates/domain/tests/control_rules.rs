//! DOMAIN-004/005 验收用例：策略操作应用与控制结果核对。
//!
//! 覆盖：管理员夹带管理员/群主角色拒绝、授权结果被篡改拒绝、head/policy 陈旧拒绝、
//! 唯一群主不变量、移除账户覆盖全部设备、纯角色操作不得夹带成员变化、
//! MLS Commit 的 epoch/群/操作摘要不一致拒绝。

use chat_domain::*;
use chat_protocol::{
    AccountId, ControlHead, ControlMetadata, ConversationId, CoordinatorId, DeviceId, Digest,
    ErrorCode, GroupId, InternalError, map_error,
};

fn account(n: u8) -> AccountId {
    AccountId::from_bytes([n; 32])
}

fn device(n: u8) -> DeviceId {
    DeviceId::from_bytes([n; 32])
}

fn member(account_no: u8, role: MemberRole, devices: &[u8]) -> GroupMember {
    GroupMember {
        account_id: account(account_no),
        role,
        devices: devices.iter().map(|n| device(*n)).collect(),
        muted: false,
        account_active: true,
        window: MembershipWindow::active(1),
    }
}

fn actor(account_no: u8, device_no: u8) -> Actor {
    Actor {
        account_id: account(account_no),
        device_id: device(device_no),
    }
}

fn base_snapshot() -> GroupSnapshot {
    GroupSnapshot {
        id: GroupId::from_bytes([0x77; 32]),
        kind: ConversationKind::Group,
        owner: account(1),
        members: vec![
            member(1, MemberRole::Owner, &[11]),
            member(2, MemberRole::Admin, &[22]),
            member(3, MemberRole::Member, &[33]),
            member(4, MemberRole::Member, &[44, 45]),
            member(5, MemberRole::Admin, &[55]),
        ],
        control_head: ControlHead::from_bytes([0xab; 32]),
        policy_version: 5,
        mls_epoch: 7,
        verified_sequence: 12,
        verified_log_hash: Digest::from_bytes([0xcd; 32]),
        coordinator: CoordinatorId::from_bytes([0xee; 32]),
        dissolved: false,
        paused_reason: None,
    }
}

fn invite_operation() -> GroupOperation {
    GroupOperation::InviteMember {
        account: account(9),
        devices: vec![device(91), device(92)],
        role: MemberRole::Member,
    }
}

fn authorized(
    snapshot: &GroupSnapshot,
    actor: Actor,
    operation: GroupOperation,
) -> AuthorizedGroupOperation {
    authorize_group_operation(snapshot, actor, operation).expect("fixture must authorize")
}

fn metadata_of(operation: &GroupOperation) -> ControlMetadata {
    ControlMetadata {
        operation: control_operation_of(operation),
        target_digest: policy_operation_digest(operation).expect("digest"),
        // 领域测试不涉及 PROTOCOL-004 的领域操作声明（55/56）与成员凭据（59/60）。
        domain_operation: None,
        added_credentials: Vec::new(),
        removed_credentials: Vec::new(),
        // PROTOCOL-004 字段 61：设备撤销列表（本测试不涉及）。
        revoked_devices: Vec::new(),
    }
}

fn staged(
    snapshot: &GroupSnapshot,
    operation: AuthorizedGroupOperation,
    added: Vec<CredentialLeaf>,
    removed: Vec<CredentialLeaf>,
) -> StagedCommit {
    StagedCommit {
        group_id: snapshot.id,
        epoch_before: snapshot.mls_epoch,
        epoch_after: snapshot.mls_epoch + 1,
        operation,
        added,
        removed,
    }
}

const POLICY_OPERATION_DIGEST_VECTOR: &str =
    "c5a31efcac0c2bb75bf71ba6952c498da7e90f8efded7318dcc43476adde07c6";
const CONTROL_HEAD_VECTOR: &str =
    "a49f0b5436bca37e1e251ee1974062b4d966ab5bd9e88c6f30bdd318d8853e2a";

// ---------------------------------------------------------------- DOMAIN-004

#[test]
fn invite_application_updates_snapshot_and_device_diff() {
    let before = base_snapshot();
    let op = authorized(&before, actor(1, 11), invite_operation());
    let proposed = apply_policy_operation(&before, &op).expect("apply");
    assert_eq!(proposed.policy.policy_version, before.policy_version + 1);
    assert_eq!(
        proposed.policy.mls_epoch, before.mls_epoch,
        "MLS epoch 不在策略层推进"
    );
    assert_ne!(proposed.policy.control_head, before.control_head);
    assert_eq!(proposed.policy.owner, account(1));
    assert_eq!(proposed.policy.owner_count(), 1);
    let invited = proposed.policy.member(account(9)).expect("invited member");
    assert_eq!(invited.role, MemberRole::Member);
    assert_eq!(invited.devices, vec![device(91), device(92)]);
    assert_eq!(
        invited.window,
        MembershipWindow::active(before.verified_sequence + 1)
    );
    assert_eq!(
        proposed.device_changes.added,
        vec![
            CredentialLeaf::new(account(9), device(91)),
            CredentialLeaf::new(account(9), device(92))
        ]
    );
    assert!(proposed.device_changes.removed.is_empty());
    assert_eq!(proposed.owner_after, account(1));
}

#[test]
fn owner_authorized_admin_invite_is_allowed_but_smuggled_authorization_is_not() {
    let before = base_snapshot();
    // 群主可以邀请管理员：重新授权得到同一结论。
    let op = authorized(
        &before,
        actor(1, 11),
        GroupOperation::InviteMember {
            account: account(9),
            devices: vec![device(91)],
            role: MemberRole::Admin,
        },
    );
    assert!(apply_policy_operation(&before, &op).is_ok());

    // 管理员伪造「邀请管理员」的授权结果：重新授权必须拒绝。
    let mut forged = authorized(
        &before,
        actor(2, 22),
        GroupOperation::RemoveMember {
            account: account(3),
        },
    );
    forged.operation = GroupOperation::InviteMember {
        account: account(9),
        devices: vec![device(91)],
        role: MemberRole::Admin,
    };
    assert_eq!(
        apply_policy_operation(&before, &forged).unwrap_err().code(),
        ErrorCode::PermissionDenied
    );
    // 普通成员伪造邀请同样拒绝。
    let mut member_forged = authorized(&before, actor(3, 33), GroupOperation::Leave);
    member_forged.operation = invite_operation();
    member_forged.role_changes.clear();
    assert_eq!(
        apply_policy_operation(&before, &member_forged)
            .unwrap_err()
            .code(),
        ErrorCode::PermissionDenied
    );
}

#[test]
fn tampered_authorization_is_rejected() {
    let before = base_snapshot();
    let mut op = authorized(&before, actor(1, 11), invite_operation());
    // 篡改角色迁移。
    op.role_changes = vec![RoleChange {
        account_id: account(9),
        from: None,
        to: MemberRole::Admin,
    }];
    assert_eq!(
        apply_policy_operation(&before, &op).unwrap_err().code(),
        ErrorCode::InvalidPolicyTransition
    );
    // 篡改 owner_after。
    let mut op = authorized(&before, actor(1, 11), invite_operation());
    op.owner_after = account(9);
    assert_eq!(
        apply_policy_operation(&before, &op).unwrap_err().code(),
        ErrorCode::InvalidPolicyTransition
    );
}

#[test]
fn stale_head_or_policy_is_rejected_before_any_mutation() {
    let before = base_snapshot();
    let op = authorized(&before, actor(1, 11), invite_operation());
    let mut newer_head = before.clone();
    newer_head.control_head = ControlHead::from_bytes([0x01; 32]);
    assert_eq!(
        apply_policy_operation(&newer_head, &op).unwrap_err().code(),
        ErrorCode::StaleControlHead
    );
    let mut newer_policy = before.clone();
    newer_policy.policy_version += 1;
    assert_eq!(
        apply_policy_operation(&newer_policy, &op)
            .unwrap_err()
            .code(),
        ErrorCode::StaleControlHead
    );
}

#[test]
fn removal_covers_every_device_and_keeps_single_owner() {
    let before = base_snapshot();
    let op = authorized(
        &before,
        actor(1, 11),
        GroupOperation::RemoveMember {
            account: account(4),
        },
    );
    let proposed = apply_policy_operation(&before, &op).expect("apply");
    assert!(proposed.policy.member(account(4)).is_none());
    assert_eq!(
        proposed.device_changes.removed,
        vec![
            CredentialLeaf::new(account(4), device(44)),
            CredentialLeaf::new(account(4), device(45))
        ]
    );
    assert_eq!(proposed.policy.owner_count(), 1);
    assert_eq!(proposed.policy.owner, account(1));
    // 移除群主在授权层即拒绝（必须转让或解散）。
    assert_eq!(
        authorize_group_operation(
            &before,
            actor(1, 11),
            GroupOperation::RemoveMember {
                account: account(1)
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::PermissionDenied
    );
}

#[test]
fn role_mute_rename_and_transfer_apply_exactly() {
    let before = base_snapshot();
    // 角色迁移。
    let op = authorized(
        &before,
        actor(1, 11),
        GroupOperation::SetRole {
            account: account(5),
            role: MemberRole::Member,
        },
    );
    let proposed = apply_policy_operation(&before, &op).expect("apply");
    assert_eq!(
        proposed.policy.member(account(5)).unwrap().role,
        MemberRole::Member
    );
    assert!(proposed.device_changes.added.is_empty());
    assert!(proposed.device_changes.removed.is_empty());
    assert_eq!(proposed.policy.owner_count(), 1);
    // 禁言。
    let op = authorized(
        &before,
        actor(1, 11),
        GroupOperation::SetMuted {
            account: account(3),
            muted: true,
        },
    );
    let proposed = apply_policy_operation(&before, &op).expect("apply");
    assert!(proposed.policy.member(account(3)).unwrap().muted);
    // 改名（公开层只放摘要）。
    let op = authorized(
        &before,
        actor(1, 11),
        GroupOperation::Rename {
            name_digest: Digest::from_bytes([0x42; 32]),
        },
    );
    assert!(apply_policy_operation(&before, &op).is_ok());
    // 转让：目标成为唯一群主，原群主降为 **admin** 且不退出（与 GROUPS/节点元数据统一）。
    let target = account(3);
    let op = validate_ownership_transfer(&before, actor(1, 11), target).expect("transfer");
    let proposed = apply_policy_operation(&before, &op).expect("apply");
    assert_eq!(proposed.policy.owner, target);
    assert_eq!(
        proposed.policy.member(target).unwrap().role,
        MemberRole::Owner
    );
    assert_eq!(
        proposed.policy.member(account(1)).unwrap().role,
        MemberRole::Admin
    );
    assert_eq!(proposed.policy.owner_count(), 1);
    assert!(proposed.device_changes.added.is_empty());
    assert!(proposed.device_changes.removed.is_empty());
}

#[test]
fn leave_and_dissolve_apply_exactly() {
    let before = base_snapshot();
    let op = authorized(&before, actor(3, 33), GroupOperation::Leave);
    let proposed = apply_policy_operation(&before, &op).expect("apply");
    assert!(proposed.policy.member(account(3)).is_none());
    assert_eq!(
        proposed.device_changes.removed,
        vec![CredentialLeaf::new(account(3), device(33))]
    );
    assert_eq!(proposed.policy.owner_count(), 1);
    // 群主不能通过伪造 Leave 授权绕过转让/解散。
    let mut forged = authorized(&before, actor(2, 22), GroupOperation::Leave);
    forged.operation = GroupOperation::Leave;
    forged.actor = actor(1, 11);
    assert_eq!(
        apply_policy_operation(&before, &forged).unwrap_err().code(),
        ErrorCode::OwnerTransferRequired
    );
    // 解散。
    let op = authorized(&before, actor(1, 11), GroupOperation::Dissolve);
    let proposed = apply_policy_operation(&before, &op).expect("apply");
    assert!(proposed.policy.dissolved);
    assert_eq!(proposed.policy.policy_version, before.policy_version + 1);
    assert_eq!(proposed.policy.owner_count(), 1);
}

#[test]
fn frozen_policy_digest_and_control_head_vectors() {
    // 固定输入：Invite(account 9, devices [91,92], Member)。
    let operation = invite_operation();
    let digest = policy_operation_digest(&operation).expect("digest");
    assert_eq!(digest.to_hex(), POLICY_OPERATION_DIGEST_VECTOR);
    let head =
        control_head_after(ControlHead::from_bytes([0xab; 32]), 6, &operation).expect("head");
    assert_eq!(head.to_hex(), CONTROL_HEAD_VECTOR);
    // 确定性：同操作同字节；不同目标/角色/设备都改变摘要。
    assert_eq!(
        policy_operation_digest(&operation).unwrap(),
        policy_operation_digest(&invite_operation()).unwrap()
    );
    let mut changed_role = invite_operation();
    if let GroupOperation::InviteMember { role, .. } = &mut changed_role {
        *role = MemberRole::Admin;
    }
    assert_ne!(policy_operation_digest(&changed_role).unwrap(), digest);
    let mut changed_devices = invite_operation();
    if let GroupOperation::InviteMember { devices, .. } = &mut changed_devices {
        devices.push(device(93));
    }
    assert_ne!(policy_operation_digest(&changed_devices).unwrap(), digest);
    let mut changed_account = invite_operation();
    if let GroupOperation::InviteMember { account, .. } = &mut changed_account {
        *account = self::account(8);
    }
    assert_ne!(policy_operation_digest(&changed_account).unwrap(), digest);
    // 控制头绑定前一控制头与策略版本。
    assert_ne!(
        control_head_after(ControlHead::from_bytes([0x01; 32]), 6, &operation).unwrap(),
        head
    );
    assert_ne!(
        control_head_after(ControlHead::from_bytes([0xab; 32]), 7, &operation).unwrap(),
        head
    );
}

// ---------------------------------------------------------------- DOMAIN-005

#[test]
fn invite_commit_matches_business_change() {
    let before = base_snapshot();
    let op = authorized(&before, actor(1, 11), invite_operation());
    let metadata = metadata_of(&op.operation);
    let commit = staged(
        &before,
        op,
        vec![
            CredentialLeaf::new(account(9), device(91)),
            CredentialLeaf::new(account(9), device(92)),
        ],
        Vec::new(),
    );
    let result = verify_control_result(&before, metadata.clone(), &commit).expect("verify");
    assert_eq!(result.epoch_before, before.mls_epoch);
    assert_eq!(result.epoch_after, before.mls_epoch + 1);
    assert_eq!(result.added_devices.len(), 2);
    assert!(result.removed_devices.is_empty());
    assert_eq!(
        result.policy.policy.policy_version,
        before.policy_version + 1
    );
    assert!(result.policy.policy.member(account(9)).is_some());
}

#[test]
fn commit_cannot_smuggle_extra_member_changes() {
    let before = base_snapshot();
    // 邀请操作却顺便移除群主设备。
    let op = authorized(&before, actor(1, 11), invite_operation());
    let metadata = metadata_of(&op.operation);
    let commit = staged(
        &before,
        op,
        vec![CredentialLeaf::new(account(9), device(91))],
        vec![CredentialLeaf::new(account(1), device(11))],
    );
    assert_eq!(
        verify_control_result(&before, metadata.clone(), &commit)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidControlResult
    );
    // 纯角色操作（禁言）不得夹带任何成员变化。
    let op = authorized(
        &before,
        actor(1, 11),
        GroupOperation::SetMuted {
            account: account(3),
            muted: true,
        },
    );
    let metadata = metadata_of(&op.operation);
    let commit = staged(
        &before,
        op,
        vec![CredentialLeaf::new(account(9), device(91))],
        Vec::new(),
    );
    assert_eq!(
        verify_control_result(&before, metadata.clone(), &commit)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidControlResult
    );
    // 空变化通过。
    let op = authorized(
        &before,
        actor(1, 11),
        GroupOperation::SetMuted {
            account: account(3),
            muted: true,
        },
    );
    let metadata = metadata_of(&op.operation);
    let commit = staged(&before, op, Vec::new(), Vec::new());
    assert!(verify_control_result(&before, metadata.clone(), &commit).is_ok());
}

#[test]
fn account_removal_requires_every_device() {
    let before = base_snapshot();
    let op = authorized(
        &before,
        actor(1, 11),
        GroupOperation::RemoveMember {
            account: account(4),
        },
    );
    let metadata = metadata_of(&op.operation);
    // 只移除一台设备：不通过。
    let partial = staged(
        &before,
        op.clone(),
        Vec::new(),
        vec![CredentialLeaf::new(account(4), device(44))],
    );
    assert_eq!(
        verify_control_result(&before, metadata.clone(), &partial)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidControlResult
    );
    // 移除该账户全部设备：通过。
    let full = staged(
        &before,
        op,
        Vec::new(),
        vec![
            CredentialLeaf::new(account(4), device(44)),
            CredentialLeaf::new(account(4), device(45)),
        ],
    );
    let result = verify_control_result(&before, metadata.clone(), &full).expect("verify");
    assert_eq!(result.removed_devices.len(), 2);
    assert!(result.policy.policy.member(account(4)).is_none());
}

#[test]
fn metadata_must_match_operation() {
    let before = base_snapshot();
    let op = authorized(&before, actor(1, 11), invite_operation());
    // 操作种类不符。
    let mut metadata = metadata_of(&op.operation);
    metadata.operation = chat_protocol::ControlOperation::Remove;
    let commit = staged(
        &before,
        op.clone(),
        vec![CredentialLeaf::new(account(9), device(91))],
        Vec::new(),
    );
    assert_eq!(
        verify_control_result(&before, metadata.clone(), &commit)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidControlResult
    );
    // 目标摘要不符。
    let mut metadata = metadata_of(&op.operation);
    metadata.target_digest = Digest::from_bytes([0x99; 32]);
    assert_eq!(
        verify_control_result(&before, metadata.clone(), &commit)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidControlResult
    );
}

#[test]
fn wrong_group_or_epoch_is_a_log_conflict() {
    let before = base_snapshot();
    let op = authorized(&before, actor(1, 11), invite_operation());
    let metadata = metadata_of(&op.operation);
    let mut commit = staged(
        &before,
        op.clone(),
        vec![CredentialLeaf::new(account(9), device(91))],
        Vec::new(),
    );
    commit.group_id = GroupId::from_bytes([0x99; 32]);
    assert_eq!(
        verify_control_result(&before, metadata.clone(), &commit)
            .unwrap_err()
            .code(),
        ErrorCode::LogConflict
    );
    let mut commit = staged(
        &before,
        op.clone(),
        vec![CredentialLeaf::new(account(9), device(91))],
        Vec::new(),
    );
    commit.epoch_before = before.mls_epoch + 1;
    assert_eq!(
        verify_control_result(&before, metadata.clone(), &commit)
            .unwrap_err()
            .code(),
        ErrorCode::LogConflict
    );
    // epoch 不前进。
    let mut commit = staged(
        &before,
        op,
        vec![CredentialLeaf::new(account(9), device(91))],
        Vec::new(),
    );
    commit.epoch_after = commit.epoch_before;
    assert_eq!(
        verify_control_result(&before, metadata.clone(), &commit)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidControlResult
    );
}

#[test]
fn stale_authorization_and_bad_actor_are_rejected() {
    let before = base_snapshot();
    let op = authorized(&before, actor(1, 11), invite_operation());
    let metadata = metadata_of(&op.operation);
    // 授权基于旧 head：apply 内先拒绝。
    let mut newer = before.clone();
    newer.control_head = ControlHead::from_bytes([0x01; 32]);
    let commit = staged(
        &newer,
        op.clone(),
        vec![CredentialLeaf::new(account(9), device(91))],
        Vec::new(),
    );
    assert_eq!(
        verify_control_result(&newer, metadata.clone(), &commit)
            .unwrap_err()
            .code(),
        ErrorCode::StaleControlHead
    );
    // 操作者设备已撤销。
    let mut revoked = before.clone();
    revoked.members[0].devices.clear();
    let commit = staged(
        &revoked,
        op,
        vec![CredentialLeaf::new(account(9), device(91))],
        Vec::new(),
    );
    assert_eq!(
        verify_control_result(&revoked, metadata, &commit)
            .unwrap_err()
            .code(),
        ErrorCode::DeviceRevoked
    );
}

#[test]
fn control_failures_map_to_frozen_codes_without_retry() {
    let before = base_snapshot();
    let op = authorized(&before, actor(1, 11), invite_operation());
    let metadata = metadata_of(&op.operation);
    let commit = staged(&before, op, Vec::new(), Vec::new());
    let error = verify_control_result(&before, metadata.clone(), &commit).unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidControlResult);
    let public = map_error(InternalError::from(error), None);
    assert_eq!(public.code(), ErrorCode::InvalidControlResult);
    assert!(!public.retryable());
    // 陈旧控制头是「先同步再重试」，码表允许自动重试；策略迁移/控制结果/日志冲突不允许。
    let stale = map_error(
        InternalError::from(DomainError::StaleControlHead { detail: "x" }),
        None,
    );
    assert_eq!(stale.code(), ErrorCode::StaleControlHead);
    assert!(stale.retryable(), "陈旧状态按码表先同步再重试");
    assert!(!DomainError::InvalidPolicyTransition { detail: "x" }.retryable());
    assert!(!DomainError::InvalidControlResult { detail: "x" }.retryable());
    let conflict = map_error(
        InternalError::from(DomainError::LogConflict { detail: "x" }),
        None,
    );
    assert_eq!(conflict.code(), ErrorCode::LogConflict);
    assert!(!conflict.retryable());
    let _ = ConversationId::from_bytes([1; 32]);
}
