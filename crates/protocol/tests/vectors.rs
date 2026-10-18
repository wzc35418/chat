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
