//! IDENTITY-001~007 的公开契约测试（只用公开 API）。
//!
//! 覆盖：账户/设备/邀请的端到端流程、口令的过期/篡改/错误绑定、联系人状态机与
//! 指纹变化阻断、稳定错误码、边界上限与 Debug 脱敏。

use chat_identity::{
    AccountKeys, AccountView, CapabilitySet, Contact, ContactState, DeviceCapability, DeviceKeys,
    IdentityError, IdentityUpdate, InviteExpectations, InviteScope, LocalAccount, MAX_DISPLAY_NAME_BYTES,
    RevocationSet, TrustAnchor, accept_contact_invite, confirm_contact, current_account,
    verification_fingerprint, verify_credential,
};
use chat_protocol::{AccountId, CoordinatorId, DeviceId, ErrorCode};

const DEVICE_SEED: [u8; 32] = [0x11; 32];
const ISSUED_AT: u64 = 1_700_000_000_000;
const INVITE_TTL: u64 = 600_000;
const DEVICE_TTL: u64 = 60_000;

fn node() -> CoordinatorId {
    CoordinatorId::from_bytes([0x33; 32])
}

fn capabilities() -> CapabilitySet {
    CapabilitySet::try_new(&[
        DeviceCapability::SendDirect,
        DeviceCapability::SendGroup,
        DeviceCapability::Receive,
        DeviceCapability::SelfSync,
    ])
    .expect("capabilities")
}

fn local_account(display_name: &str) -> LocalAccount {
    LocalAccount::create(display_name, ISSUED_AT).expect("local account")
}

/// 真实端到端：建号 → 签发设备 → 校验设备 → 生成邀请 → 校验邀请 → 确认联系人。
#[test]
fn end_to_end_account_device_invite_and_contact() {
    let alice = local_account("  Alice  ");
    assert_eq!(alice.display_name(), "Alice");
    assert_account_id_shape(&alice);

    let device = DeviceKeys::generate();
    let credential = alice
        .issue_device(
            &device,
            capabilities(),
            Some([0x5a; 32]),
            ISSUED_AT,
            DEVICE_TTL,
            0,
        )
        .expect("issue device");
    let anchor = alice.trust_anchor();
    let revocations = RevocationSet::empty(alice.account_id(), 0);
    let verified = verify_credential(&credential, &anchor, &revocations, ISSUED_AT + 1)
        .expect("verified credential");
    assert_eq!(verified.account_id(), alice.account_id());
    assert_eq!(verified.device_id(), device.device_id(alice.account_id()));
    assert!(verified.has(DeviceCapability::SelfSync));
    assert_eq!(verified.fingerprint(), alice.fingerprint());
    assert_eq!(verified.endpoint(), Some(&[0x5a; 32]));

    let token = alice
        .create_contact_invite(
            &device,
            node(),
            InviteScope::ContactRequest,
            ISSUED_AT,
            INVITE_TTL,
        )
        .expect("invite");
    assert!(token.as_str().starts_with("inv1."));

    // 对端（Bob）用自己尚未固定的信任状态下扫描口令：不会自动信任。
    let mut request = accept_contact_invite(
        token.as_str(),
        &InviteExpectations::at(ISSUED_AT)
            .with_node(node())
            .with_device(device.device_id(alice.account_id())),
    )
    .expect("accept invite");
    assert_eq!(request.invite().account_id(), alice.account_id());
    assert_eq!(request.contact().state(), ContactState::Requested);
    assert!(!request.contact().can_send());
    assert_eq!(
        request.contact().require_sendable().unwrap_err(),
        IdentityError::ContactNotAccepted
    );

    // 用户核对指纹后确认。
    confirm_contact(request.contact_mut()).expect("confirm contact");
    assert_eq!(request.contact().state(), ContactState::Accepted);
    assert!(request.contact().can_send());
    assert_eq!(
        request.contact().pinned_fingerprint(),
        Some(alice.fingerprint())
    );

    // 相同指纹的再次观测不改变状态。
    assert_eq!(
        request
            .contact_mut()
            .record_identity(alice.fingerprint())
            .expect("record"),
        IdentityUpdate::Unchanged
    );
    assert!(request.contact().can_send());

    // 指纹变化：转入 IdentityChanged 并禁止发送。
    let stranger = AccountKeys::generate();
    assert_eq!(
        request
            .contact_mut()
            .record_identity(stranger.fingerprint())
            .expect("record changed"),
        IdentityUpdate::Changed
    );
    assert_eq!(request.contact().state(), ContactState::IdentityChanged);
    assert!(!request.contact().can_send());
    assert_eq!(
        request.contact().require_sendable().unwrap_err(),
        IdentityError::IdentityChanged
    );

    // 普通确认不能绕过；只有显式重新核对才行。
    assert!(confirm_contact(request.contact_mut()).is_err());
    request
        .contact_mut()
        .confirm_identity_change()
        .expect("explicit reconfirmation");
    assert!(request.contact().can_send());
    assert_eq!(
        request.contact().pinned_fingerprint(),
        Some(stranger.fingerprint())
    );
}

fn assert_account_id_shape(account: &LocalAccount) {
    let text = account.account_id().encode();
    assert!(text.starts_with("acct:1:"), "unexpected account id {text}");
    assert_eq!(AccountId::parse(&text).expect("parse"), account.account_id());
    let fingerprint_text = account.verification_fingerprint();
    assert_eq!(fingerprint_text.len(), 79);
    assert_eq!(fingerprint_text.split(' ').count(), 16);
    assert_eq!(
        verification_fingerprint(&account.trust_anchor()).expect("fingerprint"),
        fingerprint_text
    );
}

/// 邀请口令的过期/篡改/跨节点拒绝，且错误码稳定。
#[test]
fn invite_rejections_have_stable_codes() {
    let alice = local_account("Alice");
    let device = DeviceKeys::from_seed(&DEVICE_SEED);
    let token = alice
        .create_contact_invite(
            &device,
            node(),
            InviteScope::ContactRequest,
            ISSUED_AT,
            INVITE_TTL,
        )
        .expect("invite");

    let expired = accept_contact_invite(
        token.as_str(),
        &InviteExpectations::at(ISSUED_AT + INVITE_TTL),
    )
    .unwrap_err();
    assert_eq!(expired, IdentityError::InviteExpired);
    assert_eq!(expired.code(), ErrorCode::CursorExpired);
    assert!(!expired.retryable());

    let wrong_node = accept_contact_invite(
        token.as_str(),
        &InviteExpectations::at(ISSUED_AT).with_node(CoordinatorId::from_bytes([0x44; 32])),
    )
    .unwrap_err();
    assert_eq!(wrong_node, IdentityError::WrongInviteNode);
    assert_eq!(wrong_node.code(), ErrorCode::InvalidTarget);

    // 篡改签名的最后一个字符。
    let mut tampered = token.as_str().to_owned();
    let last = tampered.pop().expect("last char");
    tampered.push(if last == '0' { '1' } else { '0' });
    let bad_signature =
        accept_contact_invite(&tampered, &InviteExpectations::at(ISSUED_AT)).unwrap_err();
    assert_eq!(bad_signature, IdentityError::InvalidSignature);
    assert_eq!(bad_signature.code(), ErrorCode::InvalidSignature);

    // 超长输入有界拒绝。
    let oversized = format!("inv1.{}", "a".repeat(2048));
    let too_long =
        accept_contact_invite(&oversized, &InviteExpectations::at(ISSUED_AT)).unwrap_err();
    assert_eq!(too_long.code(), ErrorCode::ContentTooLarge);

    // 已固定信任锚与邀请公钥不一致：不能静默替换信任根。
    let other = AccountKeys::generate().trust_anchor();
    let wrong_anchor = accept_contact_invite(
        token.as_str(),
        &InviteExpectations::at(ISSUED_AT).with_trust(&other),
    )
    .unwrap_err();
    assert_eq!(wrong_anchor, IdentityError::WrongInviteAccount);
    assert_eq!(wrong_anchor.code(), ErrorCode::InvalidTarget);
}

/// 撤销的设备不能通过校验；过期凭据也不行。
#[test]
fn revoked_and_expired_devices_are_rejected() {
    let alice = local_account("Alice");
    let device = DeviceKeys::from_seed(&DEVICE_SEED);
    let credential = alice
        .issue_device(&device, capabilities(), None, ISSUED_AT, DEVICE_TTL, 4)
        .expect("issue");
    let anchor = alice.trust_anchor();

    let revoked = RevocationSet::new(alice.account_id(), 4, [device.device_id(alice.account_id())])
        .expect("revocations");
    let error = verify_credential(&credential, &anchor, &revoked, ISSUED_AT + 1).unwrap_err();
    assert_eq!(error, IdentityError::DeviceRevoked);
    assert_eq!(error.code(), ErrorCode::DeviceRevoked);

    let fresh = RevocationSet::empty(alice.account_id(), 4);
    let expired = verify_credential(&credential, &anchor, &fresh, ISSUED_AT + DEVICE_TTL).unwrap_err();
    assert_eq!(expired, IdentityError::CredentialExpired);
    assert_eq!(expired.code(), ErrorCode::InvalidCredential);
    // 撤销版本落后的凭据必须重新签发。
    let newer = RevocationSet::empty(alice.account_id(), 5);
    assert_eq!(
        verify_credential(&credential, &anchor, &newer, ISSUED_AT + 1).unwrap_err(),
        IdentityError::CredentialStale
    );
    // 信任锚属于别的账户。
    assert_eq!(
        verify_credential(&credential, &AccountKeys::generate().trust_anchor(), &fresh, ISSUED_AT + 1)
            .unwrap_err(),
        IdentityError::CrossAccountDevice
    );
}

/// 联系人状态机非法迁移一律拒绝。
#[test]
fn contact_state_machine_rejects_illegal_transitions() {
    let peer: AccountId = AccountId::from_bytes([0x77; 32]);
    let fingerprint = AccountKeys::from_seed(&[0x01; 32]).fingerprint();

    let mut contact = Contact::requested(peer, fingerprint);
    assert_eq!(contact.state(), ContactState::Requested);
    // Requested 不能直接转为 Declined 之外的"未请求"路径；直接拉黑允许。
    contact.block().expect("block");
    assert_eq!(
        contact.accept().unwrap_err(),
        IdentityError::InvalidStateTransition {
            from: ContactState::Blocked,
            to: ContactState::Accepted
        }
    );
    assert_eq!(
        contact.request(fingerprint).unwrap_err(),
        IdentityError::InvalidStateTransition {
            from: ContactState::Blocked,
            to: ContactState::Requested
        }
    );
    contact.decline().expect("unblock to declined");
    assert_eq!(
        contact.accept().unwrap_err(),
        IdentityError::InvalidStateTransition {
            from: ContactState::Declined,
            to: ContactState::Accepted
        }
    );
    contact.request(fingerprint).expect("re-request");
    confirm_contact(&mut contact).expect("confirm");
    assert!(contact.can_send());
    // Accepted → Requested 非法。
    assert_eq!(
        contact.request(fingerprint).unwrap_err(),
        IdentityError::InvalidStateTransition {
            from: ContactState::Accepted,
            to: ContactState::Requested
        }
    );
    // 拉黑后不可发送。
    contact.block().expect("block");
    assert!(!contact.can_send());
    assert_eq!(
        contact.require_sendable().unwrap_err(),
        IdentityError::ContactNotAccepted
    );
}

/// 稳定错误码：全部映射到 protocol 冻结码表，且身份错误不可自动重试。
#[test]
fn identity_errors_map_to_frozen_protocol_codes() {
    let known: std::collections::BTreeSet<&'static str> =
        ErrorCode::ALL.iter().map(|code| code.as_str()).collect();
    let samples = [
        IdentityError::InvalidDisplayName,
        IdentityError::InvalidTimestamp,
        IdentityError::InvalidAccountPublicKey,
        IdentityError::AccountKeyMismatch,
        IdentityError::InvalidSignature,
        IdentityError::InvalidCredential,
        IdentityError::CredentialExpired,
        IdentityError::CredentialNotYetValid,
        IdentityError::CredentialStale,
        IdentityError::DeviceRevoked,
        IdentityError::CrossAccountDevice,
        IdentityError::DeviceIdMismatch,
        IdentityError::EmptyCapabilities,
        IdentityError::UnknownCapability(0x8000_0000),
        IdentityError::TooManyCapabilities { limit: 8 },
        IdentityError::TooManyRevocations { limit: 64 },
        IdentityError::MalformedToken,
        IdentityError::TokenTooLong { limit: 1024 },
        IdentityError::UnsupportedTokenVersion,
        IdentityError::InviteExpired,
        IdentityError::InviteNotYetValid,
        IdentityError::InvalidInviteLifetime { max_ms: 1 },
        IdentityError::WrongInviteNode,
        IdentityError::WrongInviteAccount,
        IdentityError::WrongInviteDevice,
        IdentityError::WrongInviteScope,
        IdentityError::InvalidStateTransition {
            from: ContactState::Blocked,
            to: ContactState::Accepted,
        },
        IdentityError::IdentityChanged,
        IdentityError::ContactNotAccepted,
        IdentityError::MissingIdentity,
    ];
    for error in samples {
        let code = error.code();
        assert!(
            known.contains(code.as_str()),
            "{code:?} is not in the frozen protocol code table"
        );
        assert!(!error.retryable(), "{error:?} must not be auto retried");
        assert!(error.detail_key().starts_with("identity."));
        assert!(!error.to_string().is_empty());
    }
}

/// 显示名与输入上限。
#[test]
fn display_name_and_bounds_are_enforced() {
    assert_eq!(MAX_DISPLAY_NAME_BYTES, 64);
    assert!(LocalAccount::create("Alice", ISSUED_AT).is_ok());
    for invalid in ["", "   ", "\u{0}Alice", "\u{7}"] {
        assert_eq!(
            LocalAccount::create(invalid, ISSUED_AT).unwrap_err(),
            IdentityError::InvalidDisplayName,
            "display name {invalid:?} must be rejected"
        );
    }
    let long = "a".repeat(MAX_DISPLAY_NAME_BYTES + 1);
    assert_eq!(
        LocalAccount::create(&long, ISSUED_AT).unwrap_err(),
        IdentityError::InvalidDisplayName
    );
    assert_eq!(
        LocalAccount::create("Alice", 0).unwrap_err(),
        IdentityError::InvalidTimestamp
    );
}

/// 重启一致性：同种子恒得同账户 ID/指纹；换种子必变。
#[test]
fn account_identity_is_stable_across_restarts() {
    let seed = [0x2a; 32];
    let first = LocalAccount::from_seed("Alice", ISSUED_AT, &seed).expect("restored");
    let second = LocalAccount::from_seed("Alice", ISSUED_AT + 10, &seed).expect("restored");
    assert_eq!(first.account_id(), second.account_id());
    assert_eq!(first.fingerprint(), second.fingerprint());
    assert_eq!(first.verification_fingerprint(), second.verification_fingerprint());

    let other = LocalAccount::from_seed("Alice", ISSUED_AT, &[0x2b; 32]).expect("other");
    assert_ne!(other.account_id(), first.account_id());
    assert_ne!(other.fingerprint(), first.fingerprint());
    // 昵称不影响 ID。
    let renamed = LocalAccount::from_seed("Alice Cooper", ISSUED_AT, &seed).expect("renamed");
    assert_eq!(renamed.account_id(), first.account_id());
}

/// 脱敏：账户视图与 Debug 输出不含密钥材料。
#[test]
fn account_view_and_debug_do_not_leak_secrets() {
    let seed = [0xab; 32];
    let account = LocalAccount::from_seed("Alice", ISSUED_AT, &seed).expect("account");
    let view: AccountView = current_account(Some(&account), 2)
        .expect("view")
        .expect("some view");
    assert_eq!(view.account_id(), account.account_id());
    assert_eq!(view.active_devices(), 2);
    assert_eq!(view.fingerprint(), account.fingerprint());
    assert_eq!(view.fingerprint_text(), account.verification_fingerprint());
    assert_eq!(view.display_name(), "Alice");

    let rendered = format!("{account:?} {view:?}");
    assert!(rendered.contains("<redacted>"));
    for needle in ["abab", "ABAB", "seed", "private", "SigningKey"] {
        assert!(
            !rendered.contains(needle),
            "debug output leaked {needle}: {rendered}"
        );
    }
    assert!(current_account(None, 0).expect("none").is_none());

    let device = DeviceKeys::from_seed(&DEVICE_SEED);
    let credential = account
        .issue_device(&device, capabilities(), None, ISSUED_AT, DEVICE_TTL, 0)
        .expect("issue");
    let credential_debug = format!("{credential:?}");
    assert!(!credential_debug.contains("seed"));
}

/// 设备凭据与邀请都必须绑定同一账户/设备/节点，不接受跨绑定。
#[test]
fn bindings_cannot_be_substituted() {
    let alice = local_account("Alice");
    let device = DeviceKeys::from_seed(&DEVICE_SEED);
    let credential = alice
        .issue_device(&device, capabilities(), None, ISSUED_AT, DEVICE_TTL, 0)
        .expect("issue");
    let anchor = alice.trust_anchor();
    let revocations = RevocationSet::empty(alice.account_id(), 0);
    let verified = verify_credential(&credential, &anchor, &revocations, ISSUED_AT).expect("verified");

    let mut swapped = credential.clone();
    swapped.device_id = DeviceId::from_bytes([0x99; 32]);
    assert_eq!(
        verify_credential(&swapped, &anchor, &revocations, ISSUED_AT).unwrap_err(),
        IdentityError::DeviceIdMismatch
    );

    let mut wrong_account = credential.clone();
    wrong_account.account_id = AccountId::from_bytes([0x98; 32]);
    assert_eq!(
        verify_credential(&wrong_account, &anchor, &revocations, ISSUED_AT).unwrap_err(),
        IdentityError::CrossAccountDevice
    );

    let token = alice
        .create_contact_invite(
            &device,
            node(),
            InviteScope::ContactRequest,
            ISSUED_AT,
            INVITE_TTL,
        )
        .expect("invite");
    let token_for_other_device = accept_contact_invite(
        token.as_str(),
        &InviteExpectations::at(ISSUED_AT).with_device(DeviceId::from_bytes([0x97; 32])),
    )
    .unwrap_err();
    assert_eq!(token_for_other_device, IdentityError::WrongInviteDevice);
    assert_eq!(verified.device_id(), device.device_id(alice.account_id()));

    let anchor = alice.trust_anchor();
    let rebuilt = TrustAnchor::from_public_key(*anchor.account_public_key()).expect("anchor");
    assert_eq!(rebuilt, anchor);
    assert_eq!(rebuilt.account_id(), alice.account_id());
}
