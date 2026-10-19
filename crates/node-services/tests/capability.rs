//! API-004 能力令牌的公开接口测试：绑定、过期、签名与不可跨对象重放。
mod support;

use chat_node_services::capability::{
    CapabilityScope, MAX_CAPABILITY_TTL_SECONDS, DEFAULT_CAPABILITY_TTL_SECONDS,
};
use chat_node_services::services::NodeAuthPolicy;
use support::{Harness, NOW, device_text};

fn object_text(seed: u8) -> String {
    chat_protocol::ObjectId::from_bytes([seed; 32]).encode()
}

/// 登记一台设备并返回 (harness, device_id)。
fn registered(seed: u8) -> (Harness, String) {
    let mut harness = Harness::in_memory();
    let device = Harness::device(seed);
    harness.register(&device, NOW).expect("device registers");
    let device_id = device.device_id.clone();
    (harness, device_id)
}

#[test]
fn issued_tokens_verify_and_report_their_binding() {
    let (mut harness, device_id) = registered(31);
    let object = object_text(41);
    let token = harness
        .service
        .issue_capability(&object, &device_id, CapabilityScope::BlobDownload, 60, NOW)
        .expect("token issued");
    assert!(token.starts_with("cap1."));
    let grant = harness
        .service
        .verify_capability(&token, &object, &device_id, CapabilityScope::BlobDownload, NOW + 1)
        .expect("token verifies");
    assert_eq!(grant.object_id, object);
    assert_eq!(grant.device_id, device_id);
    assert_eq!(grant.scope, CapabilityScope::BlobDownload);
    assert_eq!(grant.issued_at, NOW);
    assert_eq!(grant.expires_at, NOW + 60);
    assert_eq!(grant.node_id, harness.node_id);
}

#[test]
fn expired_tokens_are_rejected() {
    let (mut harness, device_id) = registered(32);
    let object = object_text(42);
    let token = harness
        .service
        .issue_capability(&object, &device_id, CapabilityScope::BlobDownload, 10, NOW)
        .expect("token issued");
    // 最后一个有效秒仍然通过；到点即过期。
    assert!(
        harness
            .service
            .verify_capability(&token, &object, &device_id, CapabilityScope::BlobDownload, NOW + 9)
            .is_ok()
    );
    let expired = harness
        .service
        .verify_capability(&token, &object, &device_id, CapabilityScope::BlobDownload, NOW + 10)
        .expect_err("expired token");
    assert_eq!(expired.code(), "AUTH_EXPIRED");
}

#[test]
fn wrong_object_device_or_scope_is_rejected() {
    let (mut harness, device_id) = registered(33);
    let other_device = Harness::device(34);
    harness.register(&other_device, NOW).expect("second device");
    let object = object_text(43);
    let other_object = object_text(44);
    let token = harness
        .service
        .issue_capability(&object, &device_id, CapabilityScope::BlobDownload, 60, NOW)
        .expect("token issued");

    let wrong_object = harness
        .service
        .verify_capability(
            &token,
            &other_object,
            &device_id,
            CapabilityScope::BlobDownload,
            NOW + 1,
        )
        .expect_err("cross-object replay");
    assert_eq!(wrong_object.code(), "INVALID_CAPABILITY");

    let wrong_device = harness
        .service
        .verify_capability(
            &token,
            &object,
            &other_device.device_id,
            CapabilityScope::BlobDownload,
            NOW + 1,
        )
        .expect_err("other device");
    assert_eq!(wrong_device.code(), "INVALID_CAPABILITY");

    let wrong_scope = harness
        .service
        .verify_capability(&token, &object, &device_id, CapabilityScope::BlobUpload, NOW + 1)
        .expect_err("other scope");
    assert_eq!(wrong_scope.code(), "INVALID_CAPABILITY");
}

#[test]
fn tampered_or_foreign_tokens_are_rejected() {
    let (mut harness, device_id) = registered(35);
    let object = object_text(45);
    let token = harness
        .service
        .issue_capability(&object, &device_id, CapabilityScope::BlobDownload, 60, NOW)
        .expect("token issued");

    // 篡改 payload 的一个十六进制字符（保持长度）：规范形式复核/验签必失败。
    let mut bytes = token.into_bytes();
    let position = bytes.len() - 3;
    bytes[position] = if bytes[position] == b"a"[0] { b"b"[0] } else { b"a"[0] };
    let tampered = String::from_utf8(bytes).expect("utf8");
    let error = harness
        .service
        .verify_capability(&tampered, &object, &device_id, CapabilityScope::BlobDownload, NOW + 1)
        .expect_err("tampered token");
    assert!(
        matches!(error.code(), "INVALID_SIGNATURE" | "INVALID_REQUEST"),
        "unexpected code: {}",
        error.code()
    );

    // 结构非法。
    for malformed in ["", "cap1.", "cap1.zz.zz", "nope"] {
        assert!(
            harness
                .service
                .verify_capability(malformed, &object, &device_id, CapabilityScope::BlobDownload, NOW)
                .is_err(),
            "malformed token {malformed} must fail"
        );
    }
}

#[test]
fn tokens_from_another_node_are_rejected() {
    let (mut first, device_id) = registered(36);
    let object = object_text(46);
    let token = first
        .service
        .issue_capability(&object, &device_id, CapabilityScope::BlobDownload, 60, NOW)
        .expect("token issued");
    // 另一个节点（不同 node_id/签名钥）不认这个令牌。
    let mut other = Harness::in_memory_with(NodeAuthPolicy::default());
    other.node_id = "node-other".into();
    let other_key = chat_node_services::services::NodeServiceConfig {
        node_id: "node-other".into(),
        signing_seed: [9_u8; 32],
        policy: NodeAuthPolicy::default(),
    };
    let other_service = chat_node_services::services::NodeService::open_in_memory(other_key)
        .expect("other node");
    let error = other_service
        .verify_capability(&token, &object, &device_id, CapabilityScope::BlobDownload, NOW + 1)
        .expect_err("foreign node token");
    assert!(
        matches!(error.code(), "NODE_ID_MISMATCH" | "INVALID_SIGNATURE"),
        "unexpected code: {}",
        error.code()
    );
}

#[test]
fn ttl_and_object_shape_are_bounded() {
    let (mut harness, device_id) = registered(37);
    let object = object_text(47);
    assert_eq!(
        harness
            .service
            .issue_capability(&object, &device_id, CapabilityScope::BlobDownload, 0, NOW)
            .expect_err("zero ttl")
            .code(),
        "INVALID_REQUEST"
    );
    assert_eq!(
        harness
            .service
            .issue_capability(
                &object,
                &device_id,
                CapabilityScope::BlobDownload,
                MAX_CAPABILITY_TTL_SECONDS + 1,
                NOW
            )
            .expect_err("ttl too long")
            .code(),
        "INVALID_REQUEST"
    );
    // 下载用途的 object 必须是 PROTOCOL-001 object 文本。
    assert_eq!(
        harness
            .service
            .issue_capability("blob-1", &device_id, CapabilityScope::BlobDownload, 60, NOW)
            .expect_err("bad blob id")
            .code(),
        "INVALID_REQUEST"
    );
    // 上传用途的 object 是上传会话文本（节点随机 id，不做 PROTOCOL-001 校验）。
    assert!(
        harness
            .service
            .issue_capability("upl_0123456789abcdef", &device_id, CapabilityScope::BlobUpload, 60, NOW)
            .is_ok()
    );
    // 未登记设备拿不到令牌。
    assert_eq!(
        harness
            .service
            .issue_capability(&object, &device_text(99), CapabilityScope::BlobDownload, 60, NOW)
            .expect_err("unknown device")
            .code(),
        "INVALID_CREDENTIAL"
    );
    // 默认 TTL 也在允许范围内。
    assert!(DEFAULT_CAPABILITY_TTL_SECONDS <= MAX_CAPABILITY_TTL_SECONDS);
}
// ---------------------------------------------------------------- CAP-REV 撤销

#[test]
fn revocation_takes_effect_immediately_and_is_idempotent() {
    let (mut harness, device_id) = registered(35);
    let object = object_text(45);
    let issued = harness
        .service
        .issue_capability_with_id(&object, &device_id, CapabilityScope::BlobDownload, 600, NOW)
        .expect("token issued with id");
    assert_eq!(issued.token_id.len(), 64);
    assert_eq!(
        chat_node_services::capability::capability_token_id(&issued.token).expect("derived id"),
        issued.token_id,
        "token id must be derived deterministically from the token text"
    );
    harness
        .service
        .verify_capability(
            &issued.token,
            &object,
            &device_id,
            CapabilityScope::BlobDownload,
            NOW + 1,
        )
        .expect("token verifies before revocation");
    let revoked_ms = u64::try_from(NOW).expect("now") * 1_000;
    assert!(
        harness
            .service
            .revoke_capability(&issued.token_id, "operator_request", revoked_ms)
            .expect("revocation recorded")
    );
    let error = harness
        .service
        .verify_capability(
            &issued.token,
            &object,
            &device_id,
            CapabilityScope::BlobDownload,
            NOW + 2,
        )
        .expect_err("revoked token must be rejected");
    assert_eq!(error.code(), "CAPABILITY_REVOKED");
    assert!(
        !harness
            .service
            .revoke_capability(&issued.token_id, "operator_request", revoked_ms + 1)
            .expect("idempotent revocation")
    );
    // 注意：无状态令牌是确定性的（同一 (对象, 设备, 用途, 时间窗) 逐字节相同），
    // 因此这里用**另一个对象**来拿一条不同的令牌。
    let other_object = object_text(48);
    let other = harness
        .service
        .issue_capability(&other_object, &device_id, CapabilityScope::BlobDownload, 600, NOW)
        .expect("second token");
    harness
        .service
        .verify_capability(
            &other,
            &other_object,
            &device_id,
            CapabilityScope::BlobDownload,
            NOW + 2,
        )
        .expect("unrelated token still verifies");
}

#[test]
fn legacy_cap1_tokens_still_verify_after_the_revocation_feature_lands() {
    let (mut harness, device_id) = registered(36);
    let object = object_text(46);
    let token = harness
        .service
        .issue_capability(&object, &device_id, CapabilityScope::BlobDownload, 60, NOW)
        .expect("legacy-style issue");
    assert!(token.starts_with(chat_node_services::capability::CAPABILITY_TOKEN_PREFIX));
    let grant = harness
        .service
        .verify_capability(&token, &object, &device_id, CapabilityScope::BlobDownload, NOW + 1)
        .expect("cap1 token still verifies");
    assert_eq!(grant.object_id, object);
    let token_id = chat_node_services::capability::capability_token_id(&token).expect("id");
    assert!(
        !harness
            .service
            .capability_revoked(&token_id)
            .expect("revocation lookup")
    );
}

#[test]
fn expired_revocations_are_purged_but_recent_ones_survive() {
    let (mut harness, device_id) = registered(37);
    // 确定性令牌：用不同对象保证两条令牌的 token_id 不同。
    let retired = harness
        .service
        .issue_capability_with_id(
            &object_text(49),
            &device_id,
            CapabilityScope::BlobDownload,
            60,
            NOW,
        )
        .expect("old token");
    let recent = harness
        .service
        .issue_capability_with_id(
            &object_text(50),
            &device_id,
            CapabilityScope::BlobDownload,
            60,
            NOW,
        )
        .expect("recent token");
    let retention_ms = chat_node_services::capability::REVOCATION_RETENTION_SECONDS * 1_000;
    let old_ms = u64::try_from(NOW).expect("now") * 1_000;
    harness
        .service
        .revoke_capability(&retired.token_id, "operator_request", old_ms)
        .expect("old revocation");
    harness
        .service
        .revoke_capability(&recent.token_id, "operator_request", old_ms + retention_ms + 1)
        .expect("recent revocation");
    let now_ms = old_ms + retention_ms + 2;
    let removed = harness
        .service
        .purge_expired_revocations(now_ms)
        .expect("purge runs");
    assert_eq!(removed, 1, "only the out-of-retention record is removed");
    assert!(
        !harness
            .service
            .capability_revoked(&retired.token_id)
            .expect("lookup")
    );
    assert!(
        harness
            .service
            .capability_revoked(&recent.token_id)
            .expect("lookup"),
        "recent revocation must survive the purge"
    );
}


