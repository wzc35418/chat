//! DM 包：领域操作声明（PROTOCOL-004 字段 55/56）的接收、校验与日志投影。
mod support;

use chat_node_services::canonical::hex_encode;
use chat_node_services::group::{
    CandidateControlMetadata, CandidateDomainOperation, GroupCandidate, KIND_APPLICATION, KIND_CONTROL,
    SignedCandidate, domain_operation_text,
};
use chat_protocol::{MAX_CIPHERTEXT_BYTES, candidate_signing_bytes};
use ed25519_dalek::{Signature, Signer};
use support::{Harness, INITIAL_CONTROL_HEAD, NOW, TestDevice, group_text, submission_text};

fn declaration(operation: &str, digest: [u8; 32]) -> CandidateDomainOperation {
    CandidateDomainOperation {
        operation: operation.to_owned(),
        target_digest: hex_encode(&digest),
    }
}

fn text(value: &str) -> CandidateControlMetadata {
    CandidateControlMetadata {
        operation: value.to_owned(),
        target_digest: hex_encode(&[0x33_u8; 32]),
        added_credentials: Vec::new(),
        removed_credentials: Vec::new(),
    }
}

/// 客户端侧构造：先按“含声明”的形态算签名原文，再签名。
fn signed(
    device: &TestDevice,
    group: &str,
    submission_seed: u8,
    operation: &str,
    domain: Option<CandidateDomainOperation>,
) -> SignedCandidate {
    let candidate = GroupCandidate {
        protocol_major: 1,
        protocol_minor: 0,
        submission_id: submission_text(submission_seed),
        group_id: group.to_owned(),
        author_device_id: device.device_id.clone(),
        kind: KIND_CONTROL.to_owned(),
        expected_control_head: hex_encode(&INITIAL_CONTROL_HEAD),
        policy_version: 1,
        mls_epoch: 0,
        encrypted_payload: hex_encode(b"sealed-control-commit"),
        control_metadata: Some(text(operation)),
    };
    let unsigned = candidate
        .to_unsigned_with_domain(MAX_CIPHERTEXT_BYTES, domain.as_ref())
        .expect("candidate converts");
    let bytes = candidate_signing_bytes(&unsigned).expect("signing bytes");
    let signature: Signature = device.key.sign(bytes.as_bytes());
    SignedCandidate {
        candidate,
        author_signature: hex_encode(&signature.to_bytes()),
    }
}

fn registered_group() -> (Harness, String, TestDevice) {
    let mut harness = Harness::in_memory();
    let device = Harness::device(61);
    let actor = harness.registered_actor(&device, NOW);
    let group = group_text(61);
    harness
        .service
        .register_group(
            &harness.signed_creation(&device, &group, &[(61, "owner")]),
            &actor,
            NOW,
        )
        .expect("group registers");
    (harness, group, device)
}

#[test]
fn declared_domain_operation_is_accepted_and_visible_in_the_log() {
    let (mut harness, group, device) = registered_group();
    let actor = harness.authenticate(&device, NOW).expect("auth");
    let digest = [0x5a_u8; 32];
    let declared = declaration("transfer_ownership", digest);
    let candidate = signed(&device, &group, 201, "set_role", Some(declared.clone()));
    let receipt = harness
        .service
        .append_group_entry_with_domain(&group, &candidate, Some(&declared), &actor, NOW)
        .expect("declared control candidate is accepted");
    assert_eq!(receipt.seq, 1);
    let page = harness
        .service
        .fetch_group_log(&group, 0, 10, &actor)
        .expect("log page");
    assert_eq!(page.entries.len(), 1);
    let entry = &page.entries[0];
    assert_eq!(entry.domain_operation.as_deref(), Some("transfer_ownership"));
    assert_eq!(
        entry.domain_target_digest.as_deref(),
        Some(hex_encode(&digest).as_str())
    );
    // 投影不破坏哈希链：自检仍通过。
    assert_eq!(harness.service.audit_group_log(&group).expect("audit"), 1);
}

#[test]
fn illegal_declarations_are_rejected_before_any_write() {
    let (mut harness, group, device) = registered_group();
    let actor = harness.authenticate(&device, NOW).expect("auth");
    // 公开操作 invite + 领域声明 transfer_ownership：协议层不允许这种组合。
    let invite = GroupCandidate {
        protocol_major: 1,
        protocol_minor: 0,
        submission_id: submission_text(202),
        group_id: group.clone(),
        author_device_id: device.device_id.clone(),
        kind: KIND_CONTROL.to_owned(),
        expected_control_head: hex_encode(&INITIAL_CONTROL_HEAD),
        policy_version: 1,
        mls_epoch: 0,
        encrypted_payload: hex_encode(b"sealed"),
        control_metadata: Some(text("invite")),
    };
    assert_eq!(
        invite
            .to_unsigned_with_domain(MAX_CIPHERTEXT_BYTES, Some(&declaration("transfer_ownership", [0x44_u8; 32])))
            .expect_err("illegal combination")
            .code(),
        "INVALID_CANDIDATE"
    );
    // 未知枚举 / 全零摘要 / 摘要长度不对。
    let dissolve_ok = GroupCandidate {
        control_metadata: Some(text("dissolve")),
        ..invite.clone()
    };
    for invalid in [
        declaration("rotate_everything", [0x44_u8; 32]),
        declaration("dissolve", [0_u8; 32]),
        CandidateDomainOperation {
            operation: "dissolve".to_owned(),
            target_digest: "0011".to_owned(),
        },
    ] {
        assert_eq!(
            dissolve_ok
                .to_unsigned_with_domain(MAX_CIPHERTEXT_BYTES, Some(&invalid))
                .expect_err("invalid declaration")
                .code(),
            "INVALID_CANDIDATE",
            "declaration {invalid:?} must be rejected"
        );
    }
    // 应用候选不能带领域声明。
    let application = GroupCandidate {
        kind: KIND_APPLICATION.to_owned(),
        control_metadata: None,
        ..invite.clone()
    };
    assert_eq!(
        application
            .to_unsigned_with_domain(MAX_CIPHERTEXT_BYTES, Some(&declaration("dissolve", [0x44_u8; 32])))
            .expect_err("application candidate cannot declare")
            .code(),
        "INVALID_CANDIDATE"
    );
    // 以上都在触库之前失败：日志仍为空。
    let _ = actor;
    assert_eq!(harness.service.audit_group_log(&group).expect("audit"), 0);
}

#[test]
fn legacy_clients_without_a_declaration_still_work() {
    let (mut harness, group, device) = registered_group();
    let actor = harness.authenticate(&device, NOW).expect("auth");
    let legacy = harness.control_candidate(&device, &group, 203, [0x11_u8; 32], b"legacy commit");
    let receipt = harness
        .service
        .append_group_entry(&group, &legacy, &actor, NOW)
        .expect("legacy candidate is accepted");
    assert_eq!(receipt.seq, 1);
    let page = harness
        .service
        .fetch_group_log(&group, 0, 10, &actor)
        .expect("log page");
    assert_eq!(page.entries[0].domain_operation, None);
    assert_eq!(page.entries[0].domain_target_digest, None);

    // 旧签名只覆盖“无声明”形态：事后补声明会让签名原文变化 → 验签失败。
    let mut tampered = legacy.clone();
    tampered.candidate.control_metadata = Some(text("dissolve"));
    let error = harness
        .service
        .append_group_entry_with_domain(
            &group,
            &tampered,
            Some(&declaration("dissolve", [0x66_u8; 32])),
            &actor,
            NOW,
        )
        .expect_err("a declaration added after signing must not verify");
    assert_eq!(error.code(), "INVALID_SIGNATURE");
    assert_eq!(harness.service.audit_group_log(&group).expect("audit"), 1);
}

#[test]
fn domain_texts_map_to_the_frozen_protocol_enum() {
    let cases = [
        ("set_role", chat_protocol::DomainOperation::SetRole),
        (
            "transfer_ownership",
            chat_protocol::DomainOperation::TransferOwnership,
        ),
        ("leave_self", chat_protocol::DomainOperation::LeaveSelf),
        ("dissolve", chat_protocol::DomainOperation::Dissolve),
    ];
    for (name, expected) in cases {
        let parsed = declaration(name, [0x77_u8; 32])
            .to_declaration()
            .expect("parses");
        assert_eq!(parsed.operation, expected);
        assert_eq!(domain_operation_text(parsed.operation), name);
        assert_ne!(parsed.target_digest, chat_protocol::Digest::ZERO);
    }
}
