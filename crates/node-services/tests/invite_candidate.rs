//! 节点侧候选校验与 protocol 白名单的一致性（GROUPS-020 / NODE-006）。
//!
//! 放行：`kind=application` + `operation=invite` + 非零 `target_digest` + 无领域声明 + 无成员凭据。
//! 其余组合仍必须拒绝（4 类反例）。
use chat_node_services::canonical::hex_encode;
use chat_node_services::group::{CandidateControlMetadata, GroupCandidate, KIND_APPLICATION, KIND_CONTROL};
use chat_protocol::{DeviceId, GroupId, MAX_CIPHERTEXT_BYTES, SubmissionId, candidate_signing_bytes};

fn digest() -> String {
    hex_encode(&[0x11u8; 32])
}

fn zero_digest() -> String {
    hex_encode(&[0x00u8; 32])
}

fn control_head() -> String {
    hex_encode(&[0x22u8; 32])
}

fn device_id() -> String {
    DeviceId::from_bytes([0x33u8; 32]).encode()
}

fn group_id() -> String {
    GroupId::from_bytes([0x44u8; 32]).encode()
}

fn submission_id() -> String {
    SubmissionId::from_bytes([0x55u8; 32]).encode()
}

fn candidate(kind: &str, metadata: Option<CandidateControlMetadata>) -> GroupCandidate {
    GroupCandidate {
        protocol_major: 1,
        protocol_minor: 0,
        submission_id: submission_id(),
        group_id: group_id(),
        author_device_id: device_id(),
        kind: kind.to_owned(),
        expected_control_head: control_head(),
        policy_version: 1,
        mls_epoch: 0,
        encrypted_payload: "00".to_owned(),
        control_metadata: metadata,
    }
}

fn invite(target: &str) -> CandidateControlMetadata {
    CandidateControlMetadata {
        operation: "invite".to_owned(),
        target_digest: target.to_owned(),
        added_credentials: Vec::new(),
        removed_credentials: Vec::new(),
    }
}

fn accepts(candidate: &GroupCandidate) -> bool {
    match candidate.to_unsigned(MAX_CIPHERTEXT_BYTES) {
        Ok(unsigned) => candidate_signing_bytes(&unsigned).is_ok(),
        Err(_) => false,
    }
}

#[test]
fn an_invite_marked_application_candidate_is_accepted_by_the_node() {
    assert!(
        accepts(&candidate(KIND_APPLICATION, Some(invite(&digest())))),
        "protocol 白名单放行的组合，节点侧也必须放行"
    );
}

#[test]
fn an_application_candidate_with_a_zero_digest_invite_marker_is_rejected() {
    assert!(!accepts(&candidate(KIND_APPLICATION, Some(invite(&zero_digest())))));
}

#[test]
fn an_application_candidate_carrying_member_credentials_is_rejected() {
    let mut metadata = invite(&digest());
    metadata.added_credentials = vec![device_id()];
    assert!(!accepts(&candidate(KIND_APPLICATION, Some(metadata))));
}

#[test]
fn an_application_candidate_with_a_non_invite_operation_is_rejected() {
    let mut metadata = invite(&digest());
    metadata.operation = "role-change".to_owned();
    assert!(!accepts(&candidate(KIND_APPLICATION, Some(metadata))));
}

#[test]
fn a_control_candidate_with_an_invite_marker_is_still_accepted() {
    assert!(accepts(&candidate(KIND_CONTROL, Some(invite(&digest())))));
}

