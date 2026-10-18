//! DOMAIN-001..012 边界用例（跨函数）。
//!
//! 覆盖：管理员不能治理管理员/群主、成员不能给自己升权、唯一 owner 不变量、
//! 容量 19→20/20→21、群主退出必须转让/解散、信箱 ACK 不置送达、本人第二台设备
//! 不算对方、禁言不无限重试、加入前历史不可读。

use chat_domain::*;
use chat_protocol::{
    AccountId, ControlHead, ConversationId, CoordinatorId, DeviceId, Digest, ErrorCode, GroupId,
    InternalError, MessageId, SubmissionId, map_error,
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

fn with_member(mut snapshot: GroupSnapshot, target: GroupMember) -> GroupSnapshot {
    snapshot.members.push(target);
    snapshot
}

// ---------------------------------------------------------------- DOMAIN-001

#[test]
fn send_permission_returns_context_and_enforces_size() {
    let snapshot = base_snapshot();
    let authorized = validate_send_permission(&snapshot, actor(3, 33), 128).unwrap();
    assert_eq!(authorized.account_id, account(3));
    assert_eq!(authorized.device_id, device(33));
    assert_eq!(authorized.control_head, snapshot.control_head);
    assert_eq!(authorized.policy_version, 5);
    assert_eq!(authorized.mls_epoch, 7);
    assert_eq!(authorized.max_content_bytes, MAX_MESSAGE_BYTES);
    // 恰好达到上限仍然允许。
    assert!(validate_send_permission(&snapshot, actor(3, 33), MAX_MESSAGE_BYTES).is_ok());
    assert_eq!(
        validate_send_permission(&snapshot, actor(3, 33), MAX_MESSAGE_BYTES + 1)
            .unwrap_err()
            .code(),
        ErrorCode::ContentTooLarge
    );
}

#[test]
fn muted_or_revoked_or_foreign_device_cannot_send() {
    let mut snapshot = base_snapshot();
    snapshot.members[2].muted = true;
    assert_eq!(
        validate_send_permission(&snapshot, actor(3, 33), 16).unwrap_err(),
        DomainError::PermissionDenied {
            detail: "actor is muted"
        }
    );
    // 撤销设备（不在成员集合内）。
    let snapshot = base_snapshot();
    assert_eq!(
        validate_send_permission(&snapshot, actor(3, 99), 16).unwrap_err(),
        DomainError::DeviceRevoked
    );
    // 借用他人设备同样被拒绝。
    assert_eq!(
        validate_send_permission(&snapshot, actor(3, 22), 16).unwrap_err(),
        DomainError::DeviceRevoked
    );
    // 非成员。
    assert_eq!(
        validate_send_permission(&snapshot, actor(9, 99), 16).unwrap_err(),
        DomainError::PermissionDenied {
            detail: "actor is not a group member"
        }
    );
    // 账户被撤销。
    let mut revoked = base_snapshot();
    revoked.members[2].account_active = false;
    assert!(validate_send_permission(&revoked, actor(3, 33), 16).is_err());
}

#[test]
fn dissolved_or_paused_group_rejects_send() {
    let mut snapshot = base_snapshot();
    snapshot.dissolved = true;
    assert!(validate_send_permission(&snapshot, actor(3, 33), 16).is_err());
    let mut snapshot = base_snapshot();
    snapshot.paused_reason = Some(PauseReason::VerificationFailed);
    assert!(validate_send_permission(&snapshot, actor(3, 33), 16).is_err());
}

#[test]
fn snapshot_with_two_owners_is_rejected_before_any_permission() {
    let mut snapshot = base_snapshot();
    snapshot.members[3].role = MemberRole::Owner;
    assert_eq!(
        validate_send_permission(&snapshot, actor(3, 33), 16)
            .unwrap_err()
            .code(),
        ErrorCode::StateCorrupted
    );
    assert_eq!(
        authorize_group_operation(&snapshot, actor(1, 11), GroupOperation::Dissolve)
            .unwrap_err()
            .code(),
        ErrorCode::StateCorrupted
    );
}

// ---------------------------------------------------------------- DOMAIN-002

#[test]
fn admin_cannot_govern_admin_or_owner() {
    let snapshot = base_snapshot();
    let admin = actor(2, 22);
    assert_eq!(
        authorize_group_operation(
            &snapshot,
            admin,
            GroupOperation::RemoveMember {
                account: account(5)
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        authorize_group_operation(
            &snapshot,
            admin,
            GroupOperation::SetMuted {
                account: account(5),
                muted: true
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        authorize_group_operation(
            &snapshot,
            admin,
            GroupOperation::RemoveMember {
                account: account(1)
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::PermissionDenied
    );
    // 管理员不能踢/禁言自己以外的普通成员？普通成员可以，管理员对同级不行。
    let kicked = authorize_group_operation(
        &snapshot,
        admin,
        GroupOperation::RemoveMember {
            account: account(3),
        },
    )
    .unwrap();
    assert_eq!(kicked.owner_after, account(1));
    assert_eq!(kicked.capacity_after.accounts, 4);
}

#[test]
fn admin_cannot_change_roles_and_nobody_can_self_promote() {
    let snapshot = base_snapshot();
    // 普通成员给自己升权。
    assert_eq!(
        authorize_group_operation(
            &snapshot,
            actor(3, 33),
            GroupOperation::SetRole {
                account: account(3),
                role: MemberRole::Admin
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::PermissionDenied
    );
    // 管理员给自己升权。
    assert_eq!(
        authorize_group_operation(
            &snapshot,
            actor(2, 22),
            GroupOperation::SetRole {
                account: account(2),
                role: MemberRole::Admin
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::PermissionDenied
    );
    // 管理员改别人角色也不行。
    assert!(
        authorize_group_operation(
            &snapshot,
            actor(2, 22),
            GroupOperation::SetRole {
                account: account(3),
                role: MemberRole::Member
            }
        )
        .is_err()
    );
    // 群主给自己降级必须走转让。
    assert_eq!(
        authorize_group_operation(
            &snapshot,
            actor(1, 11),
            GroupOperation::SetRole {
                account: account(1),
                role: MemberRole::Member
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::PermissionDenied
    );
    // 群主不能直接任命另一个群主。
    assert_eq!(
        authorize_group_operation(
            &snapshot,
            actor(1, 11),
            GroupOperation::SetRole {
                account: account(3),
                role: MemberRole::Owner
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::PermissionDenied
    );
    // 群主把管理员降为普通成员是允许的，且给出明确 before/after。
    let demoted = authorize_group_operation(
        &snapshot,
        actor(1, 11),
        GroupOperation::SetRole {
            account: account(5),
            role: MemberRole::Member,
        },
    )
    .unwrap();
    assert_eq!(
        demoted.role_changes,
        vec![RoleChange {
            account_id: account(5),
            from: Some(MemberRole::Admin),
            to: MemberRole::Member
        }]
    );
    assert_eq!(demoted.owner_after, account(1));
}

#[test]
fn invite_rules_and_capacity() {
    let snapshot = base_snapshot();
    // 普通成员不能邀请。
    assert!(
        authorize_group_operation(
            &snapshot,
            actor(3, 33),
            GroupOperation::InviteMember {
                account: account(9),
                devices: vec![device(99)],
                role: MemberRole::Member
            }
        )
        .is_err()
    );
    // 管理员不能邀请管理员。
    assert!(
        authorize_group_operation(
            &snapshot,
            actor(2, 22),
            GroupOperation::InviteMember {
                account: account(9),
                devices: vec![device(99)],
                role: MemberRole::Admin
            }
        )
        .is_err()
    );
    // 不能邀请群主。
    assert!(
        authorize_group_operation(
            &snapshot,
            actor(1, 11),
            GroupOperation::InviteMember {
                account: account(9),
                devices: vec![device(99)],
                role: MemberRole::Owner
            }
        )
        .is_err()
    );
    // 已存在账户。
    assert_eq!(
        authorize_group_operation(
            &snapshot,
            actor(1, 11),
            GroupOperation::InviteMember {
                account: account(3),
                devices: vec![device(99)],
                role: MemberRole::Member
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::InvalidTarget
    );
    // 无设备。
    assert_eq!(
        authorize_group_operation(
            &snapshot,
            actor(1, 11),
            GroupOperation::InviteMember {
                account: account(9),
                devices: Vec::new(),
                role: MemberRole::Member
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::InvalidTarget
    );
    // 设备已在他人名下。
    assert_eq!(
        authorize_group_operation(
            &snapshot,
            actor(1, 11),
            GroupOperation::InviteMember {
                account: account(9),
                devices: vec![device(33)],
                role: MemberRole::Member
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::InvalidTarget
    );
    // 重复设备。
    assert_eq!(
        authorize_group_operation(
            &snapshot,
            actor(1, 11),
            GroupOperation::InviteMember {
                account: account(9),
                devices: vec![device(99), device(99)],
                role: MemberRole::Member
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::InvalidTarget
    );
    // 每账户 3 台设备超过基线。
    assert_eq!(
        authorize_group_operation(
            &snapshot,
            actor(1, 11),
            GroupOperation::InviteMember {
                account: account(9),
                devices: vec![device(91), device(92), device(93)],
                role: MemberRole::Member
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::DeviceLimitExceeded
    );
    // 合法邀请。
    let invited = authorize_group_operation(
        &snapshot,
        actor(1, 11),
        GroupOperation::InviteMember {
            account: account(9),
            devices: vec![device(91), device(92)],
            role: MemberRole::Admin,
        },
    )
    .unwrap();
    assert_eq!(invited.capacity_after.accounts, 6);
    assert_eq!(invited.role_changes.len(), 1);
    assert_eq!(invited.role_changes[0].from, None);
}

#[test]
fn capacity_limit_blocks_the_twenty_first_account() {
    let mut snapshot = base_snapshot();
    // 补到 20 个账户。
    for n in 6u8..=20 {
        // 设备号加 100，避免与基线快照已有设备冲突。
        snapshot = with_member(snapshot, member(n, MemberRole::Member, &[n + 100]));
    }
    assert_eq!(Capacity::of_members(&snapshot.members).accounts, 20);
    assert_eq!(
        authorize_group_operation(
            &snapshot,
            actor(1, 11),
            GroupOperation::InviteMember {
                account: account(21),
                devices: vec![device(21)],
                role: MemberRole::Member
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::GroupLimitExceeded
    );
}

#[test]
fn owner_removal_and_self_removal_are_rejected() {
    let snapshot = base_snapshot();
    assert_eq!(
        authorize_group_operation(
            &snapshot,
            actor(1, 11),
            GroupOperation::RemoveMember {
                account: account(1)
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        authorize_group_operation(
            &snapshot,
            actor(2, 22),
            GroupOperation::RemoveMember {
                account: account(2)
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::PermissionDenied
    );
}

#[test]
fn direct_and_self_sync_conversations_have_no_governance() {
    let mut direct = base_snapshot();
    direct.kind = ConversationKind::Direct;
    direct.members.truncate(2);
    assert_eq!(
        authorize_group_operation(
            &direct,
            actor(1, 11),
            GroupOperation::InviteMember {
                account: account(9),
                devices: vec![device(99)],
                role: MemberRole::Member
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::PermissionDenied
    );
    assert!(authorize_group_operation(&direct, actor(1, 11), GroupOperation::Dissolve).is_ok());
    // 普通成员在私聊里可以直接退出。
    assert!(authorize_group_operation(&direct, actor(2, 22), GroupOperation::Leave).is_ok());
}

#[test]
fn dissolve_requires_owner_and_owner_leave_requires_transfer() {
    let snapshot = base_snapshot();
    assert_eq!(
        authorize_group_operation(&snapshot, actor(2, 22), GroupOperation::Dissolve)
            .unwrap_err()
            .code(),
        ErrorCode::PermissionDenied
    );
    assert!(authorize_group_operation(&snapshot, actor(1, 11), GroupOperation::Dissolve).is_ok());
    assert_eq!(
        authorize_group_operation(&snapshot, actor(1, 11), GroupOperation::Leave).unwrap_err(),
        DomainError::OwnerTransferRequired
    );
    assert!(authorize_group_operation(&snapshot, actor(3, 33), GroupOperation::Leave).is_ok());
}

// ---------------------------------------------------------------- DOMAIN-003

#[test]
fn capacity_baseline_boundaries() {
    let limits = GroupLimits::BASELINE;
    assert_eq!(limits.max_accounts, 20);
    assert_eq!(limits.max_devices, 40);
    assert_eq!(limits.max_devices_per_account, 2);
    assert!(validate_capacity(19, 38, 2, limits).is_ok());
    assert!(validate_capacity(20, 40, 2, limits).is_ok());
    assert_eq!(
        validate_capacity(21, 40, 2, limits).unwrap_err().code(),
        ErrorCode::GroupLimitExceeded
    );
    assert_eq!(
        validate_capacity(20, 41, 2, limits).unwrap_err().code(),
        ErrorCode::DeviceLimitExceeded
    );
    assert_eq!(
        validate_capacity(20, 40, 3, limits).unwrap_err().code(),
        ErrorCode::DeviceLimitExceeded
    );
    // 更低配置也生效。
    let lower = GroupLimits {
        max_accounts: 2,
        max_devices: 2,
        max_devices_per_account: 1,
    };
    assert!(validate_capacity(2, 2, 1, lower).is_ok());
    assert!(validate_capacity(3, 2, 1, lower).is_err());
    // 溢出显式拒绝。
    assert_eq!(
        validate_capacity(usize::MAX, usize::MAX, 2, limits)
            .unwrap_err()
            .code(),
        ErrorCode::GroupLimitExceeded
    );
}

#[test]
fn duplicate_devices_are_counted_once() {
    let mut snapshot = base_snapshot();
    snapshot.members[2].devices = vec![device(33), device(33)];
    let capacity = Capacity::of_members(&snapshot.members);
    assert_eq!(capacity.devices, 6);
    assert_eq!(capacity.max_devices_per_account, 2);
    // 但快照不变量拒绝同一成员内重复设备。
    assert_eq!(
        snapshot.validate_invariants().unwrap_err().code(),
        ErrorCode::StateCorrupted
    );
}

// ---------------------------------------------------------------- DOMAIN-006

#[test]
fn owner_exit_requires_transfer_or_dissolve() {
    let snapshot = base_snapshot();
    assert_eq!(
        validate_owner_exit(&snapshot, account(1)).unwrap(),
        ExitMode::OwnerMustTransfer
    );
    assert_eq!(
        validate_owner_exit(&snapshot, account(3)).unwrap(),
        ExitMode::Leave
    );
    assert_eq!(
        validate_owner_exit(&snapshot, account(2)).unwrap(),
        ExitMode::Leave
    );
    assert_eq!(
        validate_owner_exit(&snapshot, account(9)).unwrap_err(),
        DomainError::NotMember
    );
    // 唯一成员时不能隐式退出，必须显式解散。
    let mut solo = base_snapshot();
    solo.members
        .retain(|member| member.account_id == account(1));
    assert_eq!(
        validate_owner_exit(&solo, account(1)).unwrap(),
        ExitMode::OwnerMustDissolve
    );
    let mut dissolved = base_snapshot();
    dissolved.dissolved = true;
    assert!(validate_owner_exit(&dissolved, account(3)).is_err());
}

// ---------------------------------------------------------------- DOMAIN-007

#[test]
fn ownership_transfer_is_single_explicit_operation() {
    let snapshot = base_snapshot();
    let authorized = validate_ownership_transfer(&snapshot, actor(1, 11), account(3)).unwrap();
    assert_eq!(authorized.owner_after, account(3));
    assert_eq!(authorized.role_changes.len(), 2);
    assert_eq!(
        authorized.role_changes[0],
        RoleChange {
            account_id: account(3),
            from: Some(MemberRole::Member),
            to: MemberRole::Owner
        }
    );
    assert_eq!(
        authorized.role_changes[1],
        RoleChange {
            account_id: account(1),
            from: Some(MemberRole::Owner),
            to: MemberRole::Member
        }
    );
    assert_eq!(authorized.snapshot_head, snapshot.control_head);
}

#[test]
fn transfer_rejects_non_owner_self_removed_and_deviceless_targets() {
    let snapshot = base_snapshot();
    assert_eq!(
        validate_ownership_transfer(&snapshot, actor(2, 22), account(3))
            .unwrap_err()
            .code(),
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        validate_ownership_transfer(&snapshot, actor(1, 11), account(1))
            .unwrap_err()
            .code(),
        ErrorCode::InvalidTarget
    );
    // 已离开（窗口关闭）的目标。
    let mut left = base_snapshot();
    left.members[2].window = MembershipWindow {
        joined_sequence: 1,
        left_sequence: Some(4),
    };
    assert_eq!(
        validate_ownership_transfer(&left, actor(1, 11), account(3))
            .unwrap_err()
            .code(),
        ErrorCode::InvalidTarget
    );
    // 无有效设备。
    let mut deviceless = base_snapshot();
    deviceless.members[2].devices.clear();
    assert_eq!(
        validate_ownership_transfer(&deviceless, actor(1, 11), account(3))
            .unwrap_err()
            .code(),
        ErrorCode::InvalidTarget
    );
    // 被禁言但仍有效的成员可以接任。
    let mut muted = base_snapshot();
    muted.members[2].muted = true;
    assert!(validate_ownership_transfer(&muted, actor(1, 11), account(3)).is_ok());
}

#[test]
fn former_owner_retry_is_re_evaluated_against_new_snapshot() {
    let snapshot = base_snapshot();
    let authorized = validate_ownership_transfer(&snapshot, actor(1, 11), account(3)).unwrap();
    // 假设转让已被正式接受：新快照里旧群主降为普通成员。
    let mut after = snapshot.clone();
    after.owner = authorized.owner_after;
    after.members[0].role = MemberRole::Member;
    after.members[2].role = MemberRole::Owner;
    after.control_head = ControlHead::from_bytes([0x01; 32]);
    after.validate_invariants().unwrap();
    assert_eq!(
        validate_ownership_transfer(&after, actor(1, 11), account(4))
            .unwrap_err()
            .code(),
        ErrorCode::PermissionDenied
    );
    // 新群主可以转让。
    assert!(validate_ownership_transfer(&after, actor(3, 33), account(4)).is_ok());
}

// ---------------------------------------------------------------- DOMAIN-008

fn accepted_receipt() -> DeliveryEvidence {
    DeliveryEvidence::VerifiedAcceptedReceipt {
        submission_id: SubmissionId::from_bytes([1; 32]),
        candidate_digest: Digest::from_bytes([2; 32]),
        group: GroupId::from_bytes([3; 32]),
        seq: 7,
    }
}

fn authenticated_receipt() -> DeliveryEvidence {
    DeliveryEvidence::AuthenticatedReceipt {
        message_id: MessageId::from_bytes([4; 32]),
        device_id: device(22),
    }
}

#[test]
fn send_state_advances_only_on_verified_evidence() {
    assert_eq!(
        transition_send_state(SendState::Queued, accepted_receipt()).unwrap(),
        SendState::Accepted
    );
    // 重复证据幂等。
    assert_eq!(
        transition_send_state(SendState::Accepted, accepted_receipt()).unwrap(),
        SendState::Accepted
    );
    assert_eq!(
        transition_send_state(SendState::Accepted, authenticated_receipt()).unwrap(),
        SendState::Delivered
    );
    assert_eq!(
        transition_send_state(SendState::Delivered, authenticated_receipt()).unwrap(),
        SendState::Delivered
    );
}

#[test]
fn ack_and_timeout_evidence_cannot_advance_state() {
    for evidence in [
        DeliveryEvidence::TransportAck,
        DeliveryEvidence::MailboxStored,
        DeliveryEvidence::LocalTimeout,
    ] {
        assert_eq!(
            transition_send_state(SendState::Queued, evidence)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidStateTransition
        );
        assert_eq!(
            transition_send_state(SendState::Accepted, evidence)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidStateTransition
        );
    }
    // 迟到送达证据必须有正式接受关联。
    assert_eq!(
        transition_send_state(SendState::Queued, authenticated_receipt())
            .unwrap_err()
            .code(),
        ErrorCode::InvalidStateTransition
    );
}

#[test]
fn only_terminal_verified_rejection_fails_an_attempt() {
    assert_eq!(
        transition_send_state(
            SendState::Queued,
            DeliveryEvidence::VerifiedRejection {
                code: ErrorCode::InvalidSignature
            }
        )
        .unwrap(),
        SendState::Failed
    );
    // 可重试错误不能转成正式失败。
    assert!(
        transition_send_state(
            SendState::Queued,
            DeliveryEvidence::VerifiedRejection {
                code: ErrorCode::NetworkUnavailable
            }
        )
        .is_err()
    );
    // 已接受后不得被拒绝。
    assert!(
        transition_send_state(
            SendState::Accepted,
            DeliveryEvidence::VerifiedRejection {
                code: ErrorCode::LogConflict
            }
        )
        .is_err()
    );
    // 失败是终态。
    assert!(transition_send_state(SendState::Failed, accepted_receipt()).is_err());
    // 畸形回执（seq=0）显式拒绝。
    let malformed = DeliveryEvidence::VerifiedAcceptedReceipt {
        submission_id: SubmissionId::from_bytes([1; 32]),
        candidate_digest: Digest::from_bytes([2; 32]),
        group: GroupId::from_bytes([3; 32]),
        seq: 0,
    };
    assert!(transition_send_state(SendState::Queued, malformed).is_err());
}

// ---------------------------------------------------------------- DOMAIN-009

fn message() -> MessageIdentity {
    MessageIdentity {
        conversation: ConversationId::from_bytes([7; 32]),
        message_id: MessageId::from_bytes([8; 32]),
        sender: account(1),
    }
}

fn recipients() -> RecipientSnapshot {
    RecipientSnapshot {
        accounts: vec![
            // 发件人自己的第二台设备：绝不计入「对方」。
            RecipientAccount {
                account_id: account(1),
                devices: vec![device(12)],
            },
            RecipientAccount {
                account_id: account(2),
                devices: vec![device(21), device(22)],
            },
            RecipientAccount {
                account_id: account(3),
                devices: vec![device(31)],
            },
        ],
    }
}

fn receipt(account_no: u8, device_no: u8) -> VerifiedReceipt {
    VerifiedReceipt {
        message_id: message().message_id,
        account_id: account(account_no),
        device_id: device(device_no),
    }
}

#[test]
fn delivery_aggregates_per_account_without_counting_self_devices() {
    let summary =
        aggregate_delivery(message(), &[receipt(2, 21), receipt(2, 21)], &recipients()).unwrap();
    assert_eq!(summary.total_accounts, 2);
    assert_eq!(summary.delivered_accounts, 1);
    assert_eq!(summary.total_devices, 3);
    assert_eq!(summary.delivered_devices, 1);
    assert_eq!(summary.missing_accounts, vec![account(3)]);
    // 两台 B 设备仍只算 1 个账户。
    let both =
        aggregate_delivery(message(), &[receipt(2, 21), receipt(2, 22)], &recipients()).unwrap();
    assert_eq!(both.delivered_accounts, 1);
    assert_eq!(both.delivered_devices, 2);
    // A 的第二台设备回执被排除。
    let self_receipt =
        aggregate_delivery(message(), &[receipt(1, 12), receipt(2, 21)], &recipients()).unwrap();
    assert_eq!(self_receipt.delivered_accounts, 1);
    assert_eq!(self_receipt.total_devices, 3);
}

#[test]
fn delivery_rejects_out_of_scope_receipts() {
    // 非收件账户。
    assert_eq!(
        aggregate_delivery(message(), &[receipt(9, 91)], &recipients())
            .unwrap_err()
            .code(),
        ErrorCode::InvalidReceiptScope
    );
    // 收件账户的未知设备（例如撤销后伪造）。
    assert_eq!(
        aggregate_delivery(message(), &[receipt(2, 99)], &recipients())
            .unwrap_err()
            .code(),
        ErrorCode::InvalidReceiptScope
    );
    // 其他消息。
    let mut other = VerifiedReceipt {
        message_id: MessageId::from_bytes([8; 32]),
        account_id: account(2),
        device_id: device(21),
    };
    other.message_id = MessageId::from_bytes([9; 32]);
    assert!(aggregate_delivery(message(), &[other], &recipients()).is_err());
    // 重复的收件账户。
    let mut duplicate = recipients();
    duplicate.accounts.push(RecipientAccount {
        account_id: account(2),
        devices: vec![device(23)],
    });
    assert_eq!(
        aggregate_delivery(message(), &[], &duplicate)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidReceiptScope
    );
}

// ---------------------------------------------------------------- DOMAIN-010

#[test]
fn retry_classification_keeps_submission_or_rebuilds() {
    assert_eq!(
        classify_retry(ErrorCode::NetworkUnavailable, OperationState::InFlight),
        RetryDecision::ResendSameBytes
    );
    assert!(RetryDecision::ResendSameBytes.retries_automatically());
    assert!(RetryDecision::ResendSameBytes.preserves_submission());
    // 超时/结果未知：先对账，绝不重新加密。
    assert_eq!(
        classify_retry(
            ErrorCode::NetworkUnavailable,
            OperationState::AcceptanceUnknown
        ),
        RetryDecision::QueryExisting
    );
    assert_eq!(
        classify_retry(ErrorCode::UnknownResult, OperationState::InFlight),
        RetryDecision::QueryExisting
    );
    assert!(RetryDecision::QueryExisting.preserves_submission());
    assert!(!RetryDecision::QueryExisting.retries_automatically());
    // 旧 head/epoch 先同步再重建（新 SubmissionId）。
    assert_eq!(
        classify_retry(ErrorCode::StaleEpoch, OperationState::InFlight),
        RetryDecision::SyncThenRebuild
    );
    assert_eq!(
        classify_retry(ErrorCode::MissingLogEntries, OperationState::Prepared),
        RetryDecision::SyncThenRebuild
    );
    assert!(RetryDecision::SyncThenRebuild.requires_sync());
    assert!(!RetryDecision::SyncThenRebuild.preserves_submission());
    // 已接受后网络错误不再重试。
    assert_eq!(
        classify_retry(ErrorCode::NetworkUnavailable, OperationState::Accepted),
        RetryDecision::PermanentFailure
    );
}

#[test]
fn muted_member_is_not_retried_forever() {
    let decision = classify_retry(ErrorCode::PermissionDenied, OperationState::InFlight);
    assert_eq!(decision, RetryDecision::WaitForUser);
    assert!(!decision.retries_automatically());
    assert!(decision.requires_user_action());
    assert!(!decision.preserves_submission());
    // 损坏/签名错误永久失败。
    assert_eq!(
        classify_retry(ErrorCode::InvalidSignature, OperationState::InFlight),
        RetryDecision::PermanentFailure
    );
    assert_eq!(
        classify_retry(ErrorCode::StateCorrupted, OperationState::Prepared),
        RetryDecision::PermanentFailure
    );
}

// ---------------------------------------------------------------- DOMAIN-011

#[test]
fn reply_targets_must_share_conversation() {
    let conversation = ConversationId::from_bytes([5; 32]);
    let message_id = MessageId::from_bytes([6; 32]);
    let reference = validate_reply_target(
        conversation,
        MessageRef {
            conversation,
            message_id,
            availability: MessageAvailability::Available,
        },
    )
    .unwrap();
    assert_eq!(reference.message_id, message_id);
    assert!(!reference.content_unavailable);
    // 跨会话拒绝。
    assert_eq!(
        validate_reply_target(
            conversation,
            MessageRef {
                conversation: ConversationId::from_bytes([9; 32]),
                message_id,
                availability: MessageAvailability::Available,
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::InvalidReply
    );
    // 过期/删除正文允许占位引用。
    for availability in [MessageAvailability::Expired, MessageAvailability::Deleted] {
        let placeholder = validate_reply_target(
            conversation,
            MessageRef {
                conversation,
                message_id,
                availability,
            },
        )
        .unwrap();
        assert!(placeholder.content_unavailable);
        assert_eq!(placeholder.message_id, message_id);
    }
    // 不可见消息拒绝。
    assert_eq!(
        validate_reply_target(
            conversation,
            MessageRef {
                conversation,
                message_id,
                availability: MessageAvailability::NotVisible,
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::MessageNotVisible
    );
}

// ---------------------------------------------------------------- DOMAIN-012

#[test]
fn join_visibility_hides_history_before_joining() {
    let window = MembershipWindow::active(11);
    let visible = enforce_join_visibility(
        window,
        SequenceRange {
            start: 1,
            end_inclusive: 100,
        },
    )
    .unwrap();
    assert_eq!(
        visible,
        SequenceRange {
            start: 11,
            end_inclusive: 100
        }
    );
    // 加入前完全不可见。
    assert_eq!(
        enforce_join_visibility(
            window,
            SequenceRange {
                start: 1,
                end_inclusive: 10
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::HistoryNotAuthorized
    );
    // 边界：请求正好从加入序号开始。
    assert_eq!(
        enforce_join_visibility(
            window,
            SequenceRange {
                start: 11,
                end_inclusive: 11
            }
        )
        .unwrap()
        .start,
        11
    );
}

#[test]
fn left_members_do_not_see_later_content() {
    let window = MembershipWindow {
        joined_sequence: 5,
        left_sequence: Some(9),
    };
    let visible = enforce_join_visibility(
        window,
        SequenceRange {
            start: 1,
            end_inclusive: 100,
        },
    )
    .unwrap();
    assert_eq!(
        visible,
        SequenceRange {
            start: 5,
            end_inclusive: 8
        }
    );
    assert_eq!(
        enforce_join_visibility(
            window,
            SequenceRange {
                start: 9,
                end_inclusive: 100
            }
        )
        .unwrap_err()
        .code(),
        ErrorCode::HistoryNotAuthorized
    );
    // 畸形窗口与畸形请求。
    assert!(
        enforce_join_visibility(
            MembershipWindow {
                joined_sequence: 0,
                left_sequence: None
            },
            SequenceRange {
                start: 1,
                end_inclusive: 5
            }
        )
        .is_err()
    );
    assert!(
        enforce_join_visibility(
            MembershipWindow::active(1),
            SequenceRange {
                start: 0,
                end_inclusive: 5
            }
        )
        .is_err()
    );
    assert!(
        enforce_join_visibility(
            MembershipWindow::active(1),
            SequenceRange {
                start: 5,
                end_inclusive: 4
            }
        )
        .is_err()
    );
}

// ---------------------------------------------------------------- 错误映射

#[test]
fn permission_errors_are_not_auto_retryable_after_mapping() {
    let public = map_error(
        InternalError::from(DomainError::OwnerTransferRequired),
        None,
    );
    assert_eq!(public.code(), ErrorCode::OwnerTransferRequired);
    assert!(!public.retryable());
    let public = map_error(
        InternalError::from(DomainError::PermissionDenied { detail: "muted" }),
        None,
    );
    assert_eq!(public.code(), ErrorCode::PermissionDenied);
    assert!(!public.retryable());
    let public = map_error(
        InternalError::from(DomainError::InvalidSnapshot {
            detail: "two owners",
        }),
        None,
    );
    assert_eq!(public.code(), ErrorCode::StateCorrupted);
    assert!(!public.retryable());
}
