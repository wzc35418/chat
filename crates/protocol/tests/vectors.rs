//! 冻结字节的固定测试向量。
//!
//! 目的：任何规范编码/签名原文/游标封装的实现漂移都会让这些断言失败。
//! 向量来源：本 crate 实现首版生成的字节，已人工核对结构（见 docs/implementation/protocol.md）。
//! 修改向量必须走协议版本流程，不能为了让测试变绿而改期望值。

use chat_protocol::*;

/// 固定候选：Control / Invite。
fn candidate() -> UnsignedCandidate {
    UnsignedCandidate {
        protocol_version: ProtocolVersion::V1,
        submission_id: SubmissionId::from_bytes([0x11; 32]),
        group_id: GroupId::from_bytes([0x22; 32]),
        author_device_id: DeviceId::from_bytes([0x33; 32]),
        kind: CandidateKind::Control,
        expected_control_head: ControlHead::from_bytes([0x44; 32]),
        policy_version: 7,
        mls_epoch: 9,
        encrypted_payload: b"ciphertext".to_vec(),
        control_metadata: Some(ControlMetadata {
            operation: ControlOperation::Invite,
            target_digest: Digest::from_bytes([0x55; 32]),
            domain_operation: None,
        }),
    }
}

const CANDIDATE_SIGNING_VECTOR: &str = "636861742f70312f63616e6469646174652d617574686f722d7369676e61747572652f7631435043010007000d00010100000000000000010002010000000000000001000301000000000000000000040200000020111111111111111111111111111111111111111111111111111111111111111100050200000020222222222222222222222222222222222222222222222222222222222222222200060200000020333333333333333333333333333333333333333333333333333333333333333300070100000000000000020008020000002044444444444444444444444444444444444444444444444444444444444444440009010000000000000007000a010000000000000009000b0200000020305531dcc50ebca31cf1d5b31e9fc76ed51f66b3b6dd5a030c6539ae6532f979000c05010000000000000001000d0502000000205555555555555555555555555555555555555555555555555555555555555555";

const RECEIPT_SIGNING_VECTOR: &str = "636861742f70312f636f6f7264696e61746f722d726563656970742d7369676e61747572652f763143504301000700050001010000000000000001000502000000202222222222222222222222222222222222222222222222222222222222222222000e010000000000000003000f0200000020b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b200100200000020a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";

const CANONICAL_VECTOR: &str = "435043010007000500010100000000000000010005020000002022222222222222222222222222222222222222222222222222222222222222220009010000000000000014000a010000000000000029000c04";

const DIGEST_VECTOR: &str = "3c01257486b1735bca336750cd2eabf3781fd10640be341f84a8af3230afeb79";

const SIGNING_PUBKEY: &str = "2152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db12";

const SIGNATURE: &str = "c3e7b725b2524e01d34b73a3c8c4169f873b23f680259c8686cb8eef7854f0d8e9c426d0a9e4f1e0a1ddaff1806c6f2c2f785ff5a1b2163dd5eba70aaee98e0e";

const CURSOR_VECTOR: &str = "cur1.9fae285f9f8d9cc7934da8af.8a6bdad76b94ee2aeab5fdf28e76cb5d6d64b11422cbdc5144b870cd0d76a8431ffbf5cdd638fc9beb104192b7893282421210eca530b08ffd1441dd569dd64edfd739479d6bef6b4417c257f7e9dab3eb77dec10d9e214229f39f984a2c4fe4719d00b8c5c138c9b666e4f025219225489ecd035ea7608552cbb91368b39ffc969ef9ef86cbe34e0526069ae3a2d331219e2ed08639fd4c50487fda580d3a5200b9c7ec009f11c4a24090417fe3ca06068d00360cea10ccf90fb2aaa9dd87f14514283308c8663938c984e21a2c443d5e5fd4e97fb7fbff73ce864d495879573ab2cda3f0efb4206c9674b91a96eb325d8f55fb70e9d040bed375df174bffd6ea";

fn hex_decode(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2));
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let (pairs, remainder) = bytes.as_chunks::<2>();
    assert!(remainder.is_empty());
    for pair in pairs {
        let hi = (pair[0] as char).to_digit(16).expect("hex") as u8;
        let lo = (pair[1] as char).to_digit(16).expect("hex") as u8;
        out.push((hi << 4) | lo);
    }
    out
}

fn scope() -> CursorScope {
    CursorScope {
        account: AccountId::from_bytes([1; 32]),
        device: DeviceId::from_bytes([2; 32]),
        conversation: ConversationId::from_bytes([3; 32]),
        resource: CursorResource::Messages,
        sort_version: 4,
    }
}

fn position() -> CursorPosition {
    CursorPosition {
        sort_key: b"2026-10-18T00:00:00Z".to_vec(),
        seq: 42,
        tie: [9; 16],
    }
}

#[test]
fn candidate_signing_bytes_match_frozen_vector() {
    let bytes = candidate_signing_bytes(&candidate()).expect("candidate must encode");
    assert_eq!(bytes.to_hex(), CANDIDATE_SIGNING_VECTOR);
    // 前缀就是用途域，长度可复核。
    assert!(bytes.as_bytes().starts_with(CANDIDATE_SIGNING_DOMAIN));
}

#[test]
fn receipt_signing_bytes_match_frozen_vector() {
    let bytes = receipt_signing_bytes(
        Digest::from_bytes([0xa1; 32]),
        3,
        Digest::from_bytes([0xb2; 32]),
        GroupId::from_bytes([0x22; 32]),
    )
    .expect("receipt must encode");
    assert_eq!(bytes.to_hex(), RECEIPT_SIGNING_VECTOR);
    assert!(bytes.as_bytes().starts_with(RECEIPT_SIGNING_DOMAIN));
}

#[test]
fn canonical_encoding_matches_frozen_vector() {
    let body = WireValue::structure([
        WireField::new(field::SCHEMA_VERSION, WireValue::U64(1)),
        WireField::new(field::GROUP_ID, WireValue::id_bytes(&[0x22; 32])),
        WireField::new(field::POLICY_VERSION, WireValue::U64(20)),
        WireField::new(field::MLS_EPOCH, WireValue::U64(41)),
        WireField::new(field::CONTROL_OPERATION, WireValue::optional(None)),
    ]);
    let bytes = encode_canonical(&body, ProtocolVersion::V1).expect("canonical encode");
    assert_eq!(bytes.to_hex(), CANONICAL_VECTOR);
    // 解码必须回到同一值，且字段顺序无关。
    let mut fields = vec![
        WireField::new(field::CONTROL_OPERATION, WireValue::optional(None)),
        WireField::new(field::MLS_EPOCH, WireValue::U64(41)),
        WireField::new(field::SCHEMA_VERSION, WireValue::U64(1)),
        WireField::new(field::POLICY_VERSION, WireValue::U64(20)),
        WireField::new(field::GROUP_ID, WireValue::id_bytes(&[0x22; 32])),
    ];
    fields.reverse();
    let reordered = encode_canonical(&WireValue::Struct(fields), ProtocolVersion::V1)
        .expect("canonical encode");
    assert_eq!(reordered.as_bytes(), bytes.as_bytes());
    let frame = decode_bounded(bytes.as_bytes(), DecodeLimits::DEFAULT).expect("decode");
    assert_eq!(frame.value(), &body);
}

#[test]
fn digest_matches_frozen_vector() {
    assert_eq!(Digest::sha256(b"fixed-vector").to_hex(), DIGEST_VECTOR);
}

#[test]
fn ed25519_signature_is_deterministic_and_verifies() {
    let signing_key = signing_key_from_seed(&[0x42; 32]);
    let public_key = signing_key.verifying_key().to_bytes();
    assert_eq!(public_key_hex(&public_key), SIGNING_PUBKEY);
    let bytes = candidate_signing_bytes(&candidate()).expect("candidate");
    let signature = sign_bytes(&signing_key, bytes.as_bytes());
    assert_eq!(signature_hex(&signature), SIGNATURE, "ed25519 必须确定性");
    let signature_bytes: [u8; 64] = hex_decode(SIGNATURE).try_into().expect("64 bytes");
    let public_bytes: [u8; 32] = hex_decode(SIGNING_PUBKEY).try_into().expect("32 bytes");
    verify_ed25519(&public_bytes, bytes.as_bytes(), &signature_bytes).expect("signature verifies");
    // 改一个被签字段 -> 验签失败。
    let mut changed = candidate();
    changed.mls_epoch += 1;
    let changed_bytes = candidate_signing_bytes(&changed).expect("changed candidate");
    assert!(verify_ed25519(&public_bytes, changed_bytes.as_bytes(), &signature_bytes).is_err());
}

#[test]
fn cursor_token_matches_frozen_vector() {
    let key = CursorKey::from_bytes([0x5a; 32]);
    let token = OpaqueCursor::from_text(CURSOR_VECTOR);
    let decoded = decode_page_cursor(&token, &scope(), &key, 1_700_000_000_001).expect("decode");
    assert_eq!(decoded, position());
    // 19 分钟后过期。
    assert_eq!(
        decode_page_cursor(
            &token,
            &scope(),
            &key,
            1_700_000_000_000 + CURSOR_TTL_MS + 1
        )
        .unwrap_err(),
        ProtocolError::CursorExpired
    );
    // 换设备必须拒绝。
    let mut other = scope();
    other.device = DeviceId::from_bytes([9; 32]);
    assert_eq!(
        decode_page_cursor(&token, &other, &key, 1_700_000_000_001).unwrap_err(),
        ProtocolError::InvalidCursorScope
    );
}

fn public_key_hex(key: &[u8; 32]) -> String {
    key.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn signature_hex(signature: &[u8; 64]) -> String {
    signature.iter().map(|byte| format!("{byte:02x}")).collect()
}

// ---------------------------------------------------------------- U 包新增向量

const APPLICATION_EVENT_VECTOR: &str = "435043010007001000010100000000000000010002010000000000000001000301000000000000000000050200000020777777777777777777777777777777777777777777777777777777777777777700060200000020b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b200230200000020e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e100240200000020a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a10025010000000000000001002605030000000568656c6c6f002704002804002904002a04002b010000000000000001002c0200000020c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1002d010000018bcfe56800";

const EVENT_DIGEST_VECTOR: &str =
    "7503ce5f884c6df8cba51b25080f38b91c1873b04ce744b21d67007c9e5133eb";

const CANDIDATE_DIGEST_VECTOR: &str =
    "1d7c8fc3eb80fc79540587fc8da4aeb04b308b6ded456998e469e3b29d730624";

const LOG_ENTRY_HASH_VECTOR: &str =
    "0af01d62b8dd46518257859d109991cacd34721c365f231566d6c7dff4564f0d";

const RECEIPT_SIGNATURE_VECTOR: &str = "18219f668a618ea8398e85d1bf69123c84feed9cf402c4065c16aaa0665b660d8ceb13dfca0aade8b7bab301e56076b56f39830884b7f9faaf484a5776753502";

const SESSION_FINGERPRINT_VECTOR: &str =
    "6b46e70602d4b07a512c1c1d16365923e92e305114521324457ce65b5610dc84";

fn u_package_event() -> PlainApplicationEvent {
    PlainApplicationEvent {
        protocol_version: ProtocolVersion::V1,
        conversation_id: ConversationId::from_bytes([0xc1; 32]),
        group_id: GroupId::from_bytes([0x77; 32]),
        event_id: EventId::from_bytes([0xe1; 32]),
        account_id: AccountId::from_bytes([0xa1; 32]),
        device_id: DeviceId::from_bytes([0xb2; 32]),
        content: ApplicationContent::Text {
            text: "hello".to_owned(),
        },
        reply: None,
        origin: MessageOrigin::UserInitiated,
        sent_at_unix_ms: 1_700_000_000_000,
    }
}

fn session_input() -> (Capabilities, Capabilities) {
    let local = Capabilities::new(
        vec![ProtocolVersion::V1],
        vec![
            "e2ee.v1".to_owned(),
            "mls.v1".to_owned(),
            "log.v1".to_owned(),
            "attachments.v1".to_owned(),
            "receipts.v1".to_owned(),
        ],
        CapabilityLimits::new(64 * 1024, 64 * 1024, 64 * 1024),
    );
    let remote = Capabilities::new(
        vec![ProtocolVersion::V1],
        vec![
            "mls.v1".to_owned(),
            "e2ee.v1".to_owned(),
            "extensions.v1".to_owned(),
        ],
        CapabilityLimits::new(32 * 1024, 16 * 1024, 8 * 1024),
    );
    (local, remote)
}

fn hex(bytes: &[u8]) -> String {
    const HEX: [u8; 16] = *b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[(byte >> 4) as usize]));
        text.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    text
}

#[test]
fn application_event_matches_frozen_vector() {
    let bytes = encode_application_event(u_package_event()).expect("encode");
    assert_eq!(hex(bytes.as_bytes()), APPLICATION_EVENT_VECTOR);
    assert_eq!(bytes.as_bytes().len() * 2, APPLICATION_EVENT_VECTOR.len());
    // 解码回同一事件，且重复编码字节一致。
    let decoded =
        decode_application_event(bytes.as_bytes(), DecodeLimits::DEFAULT).expect("decode");
    assert_eq!(decoded, u_package_event());
    let again = encode_application_event(decoded).expect("re-encode");
    assert_eq!(again.as_bytes(), bytes.as_bytes());
    // 事件摘要带域分隔，且与裸字节摘要不同。
    let mut preimage = EVENT_DIGEST_DOMAIN.to_vec();
    preimage.extend_from_slice(bytes.as_bytes());
    let digest = Digest::sha256(&preimage);
    assert_eq!(digest.to_hex(), EVENT_DIGEST_VECTOR);
    assert_ne!(digest, Digest::sha256(bytes.as_bytes()));
}

#[test]
fn candidate_digest_matches_signing_vector() {
    let signing = candidate_signing_bytes(&candidate()).expect("candidate");
    assert_eq!(
        Digest::sha256(signing.as_bytes()).to_hex(),
        CANDIDATE_DIGEST_VECTOR
    );
    // 通过 PROTOCOL-005 得到的候选摘要必须与签名原文摘要一致。
    let device_key = signing_key_from_seed(&[0x42; 32]);
    let signed = SignedCandidate {
        unsigned: candidate(),
        author_signature: sign_bytes(&device_key, signing.as_bytes()),
    };
    let credential = VerifiedCredential {
        account_id: AccountId::from_bytes([0x99; 32]),
        device_id: DeviceId::from_bytes([0x33; 32]),
        device_public_key: device_key.verifying_key().to_bytes(),
        credential_version: 1,
        revoked: false,
    };
    let context = GroupContext {
        group_id: GroupId::from_bytes([0x22; 32]),
        control_head: ControlHead::from_bytes([0x44; 32]),
        policy_version: 7,
        mls_epoch: 9,
    };
    let verified = verify_candidate(signed, credential, context).expect("verify");
    assert_eq!(
        verified.candidate_digest().to_hex(),
        CANDIDATE_DIGEST_VECTOR
    );
    assert_eq!(verified.author_account(), AccountId::from_bytes([0x99; 32]));
}

#[test]
fn log_entry_hash_and_receipt_signature_match_frozen_vectors() {
    let group = GroupId::from_bytes([0x22; 32]);
    let candidate_digest = Digest::from_bytes(
        hex_decode(CANDIDATE_DIGEST_VECTOR)
            .try_into()
            .expect("32 bytes"),
    );
    let log_hash =
        log_entry_hash(group, 1, Digest::ZERO, candidate_digest, &[0x5c; 64]).expect("log hash");
    assert_eq!(log_hash.to_hex(), LOG_ENTRY_HASH_VECTOR);
    // 节点回执签名是确定性的 Ed25519：固定种子必须得到固定签名。
    let receipt = receipt_signing_bytes(candidate_digest, 1, Digest::ZERO, group).expect("receipt");
    let node_key = signing_key_from_seed(&[0x33; 32]);
    let signature = sign_bytes(&node_key, receipt.as_bytes());
    assert_eq!(hex(&signature), RECEIPT_SIGNATURE_VECTOR);
    verify_ed25519(
        &node_key.verifying_key().to_bytes(),
        receipt.as_bytes(),
        &signature,
    )
    .expect("receipt verifies");
    // 改变序号/前序摘要即改变日志哈希。
    assert_ne!(
        log_entry_hash(group, 2, Digest::ZERO, candidate_digest, &[0x5c; 64])
            .expect("hash")
            .to_hex(),
        LOG_ENTRY_HASH_VECTOR
    );
    assert_ne!(
        log_entry_hash(
            group,
            1,
            Digest::from_bytes([9; 32]),
            candidate_digest,
            &[0x5c; 64]
        )
        .expect("hash")
        .to_hex(),
        LOG_ENTRY_HASH_VECTOR
    );
}

#[test]
fn session_fingerprint_matches_frozen_vector() {
    let (local, remote) = session_input();
    let session = negotiate_capabilities(local, remote).expect("session");
    assert_eq!(session.protocol_version(), ProtocolVersion::V1);
    assert_eq!(session.fingerprint().to_hex(), SESSION_FINGERPRINT_VECTOR);
    // 限额取较小值，未知能力忽略。
    assert_eq!(session.limits().max_message_bytes, 16 * 1024);
    assert!(!session.supports("extensions.v1"), "远端声明但本地未声明");
    let (local, mut remote) = session_input();
    remote.limits.max_frame_bytes = 1;
    let changed = negotiate_capabilities(local, remote).expect("session");
    assert_ne!(changed.fingerprint().to_hex(), SESSION_FINGERPRINT_VECTOR);
}
/// 带领域操作声明的控制候选（其余字段与 candidate() 相同）。
fn declared_candidate(operation: ControlOperation, domain: DomainOperation) -> UnsignedCandidate {
    let mut candidate = candidate();
    candidate.control_metadata = Some(
        ControlMetadata::new(operation, Digest::from_bytes([0x55; 32])).with_domain_operation(
            DomainOperationDeclaration {
                operation: domain,
                target_digest: Digest::from_bytes([0x66; 32]),
            },
        ),
    );
    candidate
}

const SELF_UPDATE_TRANSFER_VECTOR: &str = "636861742f70312f63616e6469646174652d617574686f722d7369676e61747572652f7631435043010007000f00010100000000000000010002010000000000000001000301000000000000000000040200000020111111111111111111111111111111111111111111111111111111111111111100050200000020222222222222222222222222222222222222222222222222222222222222222200060200000020333333333333333333333333333333333333333333333333333333333333333300070100000000000000020008020000002044444444444444444444444444444444444444444444444444444444444444440009010000000000000007000a010000000000000009000b0200000020305531dcc50ebca31cf1d5b31e9fc76ed51f66b3b6dd5a030c6539ae6532f979000c05010000000000000003000d05020000002055555555555555555555555555555555555555555555555555555555555555550037010000000000000002003802000000206666666666666666666666666666666666666666666666666666666666666666";
const LEAVE_SELF_VECTOR: &str = "636861742f70312f63616e6469646174652d617574686f722d7369676e61747572652f7631435043010007000f00010100000000000000010002010000000000000001000301000000000000000000040200000020111111111111111111111111111111111111111111111111111111111111111100050200000020222222222222222222222222222222222222222222222222222222222222222200060200000020333333333333333333333333333333333333333333333333333333333333333300070100000000000000020008020000002044444444444444444444444444444444444444444444444444444444444444440009010000000000000007000a010000000000000009000b0200000020305531dcc50ebca31cf1d5b31e9fc76ed51f66b3b6dd5a030c6539ae6532f979000c05010000000000000007000d05020000002055555555555555555555555555555555555555555555555555555555555555550037010000000000000003003802000000206666666666666666666666666666666666666666666666666666666666666666";
const DISSOLVE_VECTOR: &str = "636861742f70312f63616e6469646174652d617574686f722d7369676e61747572652f7631435043010007000f00010100000000000000010002010000000000000001000301000000000000000000040200000020111111111111111111111111111111111111111111111111111111111111111100050200000020222222222222222222222222222222222222222222222222222222222222222200060200000020333333333333333333333333333333333333333333333333333333333333333300070100000000000000020008020000002044444444444444444444444444444444444444444444444444444444444444440009010000000000000007000a010000000000000009000b0200000020305531dcc50ebca31cf1d5b31e9fc76ed51f66b3b6dd5a030c6539ae6532f979000c05010000000000000008000d05020000002055555555555555555555555555555555555555555555555555555555555555550037010000000000000004003802000000206666666666666666666666666666666666666666666666666666666666666666";

#[test]
fn domain_declaration_vectors_are_frozen() {
    let cases = [
        (
            declared_candidate(
                ControlOperation::SetRole,
                DomainOperation::TransferOwnership,
            ),
            SELF_UPDATE_TRANSFER_VECTOR,
        ),
        (
            declared_candidate(ControlOperation::Leave, DomainOperation::LeaveSelf),
            LEAVE_SELF_VECTOR,
        ),
        (
            declared_candidate(ControlOperation::Dissolve, DomainOperation::Dissolve),
            DISSOLVE_VECTOR,
        ),
    ];
    let mut actual = Vec::new();
    let mut expected_all = Vec::new();
    for (candidate, expected) in cases {
        let bytes = candidate_signing_bytes(&candidate).expect("declared candidate must encode");
        assert!(bytes.as_bytes().starts_with(CANDIDATE_SIGNING_DOMAIN));
        actual.push(bytes.to_hex());
        expected_all.push(expected.to_owned());
    }
    assert_eq!(actual, expected_all);
}

#[test]
fn domain_declaration_round_trips_and_old_bytes_stay_none() {
    // 旧固定向量（无声明）解码回 None，字节未漂移。
    let old = candidate_signing_bytes(&candidate()).expect("old candidate");
    let old_body = decode_bounded(
        &old.as_bytes()[CANDIDATE_SIGNING_DOMAIN.len()..],
        DecodeLimits::DEFAULT,
    )
    .expect("old body decodes");
    assert_eq!(decode_domain_operation(old_body.value()).unwrap(), None);
    // 新向量解码回同一声明。
    let declared = declared_candidate(
        ControlOperation::SetRole,
        DomainOperation::TransferOwnership,
    );
    let new = candidate_signing_bytes(&declared).expect("declared candidate");
    let new_body = decode_bounded(
        &new.as_bytes()[CANDIDATE_SIGNING_DOMAIN.len()..],
        DecodeLimits::DEFAULT,
    )
    .expect("new body decodes");
    assert_eq!(
        decode_domain_operation(new_body.value()).unwrap(),
        Some(DomainOperationDeclaration {
            operation: DomainOperation::TransferOwnership,
            target_digest: Digest::from_bytes([0x66; 32]),
        })
    );
    // 新字段号必须登记，否则旧解码器会拒绝这些字节。
    assert!(is_known_field(field::CONTROL_DOMAIN_OPERATION));
    assert!(is_known_field(field::CONTROL_DOMAIN_TARGET_DIGEST));
}
/// 带 metadata 的附件事件（其余字段取自 u_package_event）。
fn attachment_metadata_event() -> PlainApplicationEvent {
    let mut event = u_package_event();
    event.content = ApplicationContent::Attachment {
        attachment: AttachmentRef {
            file_id: ObjectId::from_bytes([3; 32]),
            attachment_metadata: Some(vec![0xa5; 48]),
        },
    };
    event
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: [u8; 16] = *b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[(byte >> 4) as usize]));
        text.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    text
}

const ATTACHMENT_METADATA_EVENT_VECTOR: &str = "435043010007001100010100000000000000010002010000000000000001000301000000000000000000050200000020777777777777777777777777777777777777777777777777777777777777777700060200000020b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b200230200000020e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e100240200000020a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1002501000000000000000200260400270400280502000000200303030303030303030303030303030303030303030303030303030303030303002904002a04002b010000000000000001002c0200000020c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1002d010000018bcfe5680000390200000030a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5";
const ATTACHMENT_METADATA_DIGEST_VECTOR: &str =
    "64d9113ad3f6e854cc5027bc676645be88e49b6df5e1995c81f2dc899cc35a85";

#[test]
fn attachment_metadata_event_matches_frozen_vector() {
    let event = attachment_metadata_event();
    let bytes = encode_application_event(event.clone()).expect("encode");
    assert_eq!(
        hex_encode(bytes.as_bytes()),
        ATTACHMENT_METADATA_EVENT_VECTOR
    );
    // 内层去重键（事件摘要）也冻结：metadata 改变必然改变该摘要。
    let mut preimage = Vec::new();
    preimage.extend_from_slice(EVENT_DIGEST_DOMAIN);
    preimage.extend_from_slice(bytes.as_bytes());
    assert_eq!(
        Digest::sha256(&preimage).to_hex(),
        ATTACHMENT_METADATA_DIGEST_VECTOR
    );
    assert_eq!(
        decode_application_event(bytes.as_bytes(), DecodeLimits::DEFAULT).unwrap(),
        event
    );
    // 新事件比旧事件多一个 57 字段；旧事件编码走既有冻结向量测试。
    let old = encode_application_event(u_package_event()).expect("old event");
    assert!(bytes.as_bytes().len() > old.as_bytes().len());
}
