//! NODE-016 过期回收的公开接口测试。
//!
//! 覆盖：过期上传会话（含临时块）被清且未过期保留、过期已发布对象被回收而未过期对象保留、
//! 过期信箱条目按已 ACK/未 ACK 分别计数清理、过期挑战清理、每轮上限生效、重复调用幂等。
mod support;

use chat_node_services::blob::{PublicBlobDescriptor, CipherManifest};
use chat_node_services::cleanup::CleanupStats;
use chat_node_services::limits::{BlobPolicy, CleanupPolicy, MailboxPolicy};
use chat_node_services::mailbox::MailboxItem;
use chat_node_services::auth::ChallengePurpose;
use chat_node_services::canonical::{hex_encode, sha256};
use chat_node_services::services::{AuthenticatedDevice, NodeAuthPolicy};
use chat_protocol::{ObjectId, OperationId};
use support::{Harness, NOW, TestDevice, group_text};

const UPLOAD_TTL: u64 = 10;
const PUBLISHED_TTL: u64 = 10;

fn object_text(seed: u8) -> String {
    ObjectId::from_bytes([seed; 32]).encode()
}

fn operation_text(seed: u8) -> String {
    OperationId::from_bytes([seed; 32]).encode()
}

fn policy(rows_per_kind: u32) -> NodeAuthPolicy {
    NodeAuthPolicy {
        blob: BlobPolicy {
            upload_ttl_seconds: UPLOAD_TTL,
            ..BlobPolicy::default()
        },
        mailbox: MailboxPolicy {
            default_ttl_seconds: 3_600,
            ..MailboxPolicy::default()
        },
        cleanup: CleanupPolicy {
            enabled: true,
            max_rows_per_kind: rows_per_kind,
            rate_limit_bucket_windows: 2,
            published_blob_ttl_seconds: PUBLISHED_TTL,
        },
        ..NodeAuthPolicy::default()
    }
}

/// 两成员群 + 一条 owner 已接受的日志条目（上传会话与信箱条目都要它）。
fn fixture(rows_per_kind: u32) -> (Harness, String, TestDevice, TestDevice, u64) {
    let mut harness = Harness::in_memory_with(policy(rows_per_kind));
    let owner = Harness::device(21);
    let member = Harness::device(22);
    let owner_actor = harness.registered_actor(&owner, NOW);
    harness.register(&member, NOW).expect("member registers");
    let group = group_text(21);
    harness
        .service
        .register_group(
            &harness.signed_creation(&owner, &group, &[(21, "owner"), (22, "member")]),
            &owner_actor,
            NOW,
        )
        .expect("group registers");
    let candidate = harness.application_candidate(&owner, &group, 200, b"authorized entry");
    let receipt = harness
        .service
        .append_group_entry(&group, &candidate, &owner_actor, NOW)
        .expect("entry accepted");
    (harness, group, owner, member, receipt.seq)
}

fn descriptor(seed: u8, group: &str, source_seq: u64, chunk: &[u8], count: u32) -> PublicBlobDescriptor {
    PublicBlobDescriptor {
        blob_id: object_text(seed),
        group_id: group.to_owned(),
        source_seq,
        ciphertext_bytes: (chunk.len() as u64) * u64::from(count),
        chunk_bytes: u32::try_from(chunk.len()).expect("chunk size"),
        chunk_count: count,
    }
}

fn begin_upload(
    harness: &mut Harness,
    actor: &AuthenticatedDevice,
    descriptor: &PublicBlobDescriptor,
    operation_seed: u8,
    now: u64,
) -> String {
    harness
        .service
        .begin_blob_upload(descriptor, &operation_text(operation_seed), actor, now)
        .expect("upload begins")
        .upload_id
}

/// 过期上传会话的临时块被清；未过期会话保留。
#[test]
fn expired_upload_sessions_and_their_chunks_are_collected() {
    let (mut harness, group, owner, _member, seq) = fixture(512);
    let owner_actor = harness.authenticate(&owner, NOW).expect("auth");
    let chunk = b"12345678";
    let stale = descriptor(101, &group, seq, chunk, 1);
    let fresh = descriptor(102, &group, seq, chunk, 1);
    let stale_id = begin_upload(&mut harness, &owner_actor, &stale, 201, NOW);
    harness
        .service
        .put_blob_chunk(&stale_id, 0, chunk, &owner_actor, NOW)
        .expect("chunk stored");
    let fresh_id = begin_upload(&mut harness, &owner_actor, &fresh, 202, NOW + 5);

    let stats = harness.service.collect_expired(NOW + UPLOAD_TTL).expect("collect");
    assert_eq!(stats.expired_upload_sessions, 1, "only the stale session expires");
    assert_eq!(stats.expired_upload_chunks, 1, "its temporary chunk is removed");
    assert_eq!(stats.expired_blob_objects, 0);
    // 过期会话不能再写块（状态已置 expired）。
    assert_eq!(
        harness
            .service
            .put_blob_chunk(&stale_id, 0, chunk, &owner_actor, NOW + UPLOAD_TTL)
            .expect_err("expired session")
            .code(),
        "UPLOAD_EXPIRED"
    );
    // 未过期会话仍可继续写块。
    harness
        .service
        .put_blob_chunk(&fresh_id, 0, chunk, &owner_actor, NOW + UPLOAD_TTL)
        .expect("fresh session still accepts chunks");
    // 幂等：同一时刻再扫一次没有新东西。
    let again = harness.service.collect_expired(NOW + UPLOAD_TTL).expect("collect");
    assert_eq!(again, CleanupStats::ZERO);
}

/// 过期已发布对象被回收；未过期对象与未设置保留期的对象都不动。
#[test]
fn expired_blobs_are_collected_but_fresh_ones_survive() {
    let (mut harness, group, owner, member, seq) = fixture(512);
    let owner_actor = harness.authenticate(&owner, NOW).expect("auth");
    let chunk = b"abcdefgh";
    // 早在 NOW+5 发布（保留期到 NOW+15）。
    let manifest = CipherManifest {
        chunk_count: 1,
        ciphertext_bytes: chunk.len() as u64,
        ciphertext_digest: hex_encode(&sha256(chunk)),
    };
    let early = descriptor(103, &group, seq, chunk, 1);
    let early_id = begin_upload(&mut harness, &owner_actor, &early, 203, NOW + 5);
    harness
        .service
        .put_blob_chunk(&early_id, 0, chunk, &owner_actor, NOW + 5)
        .expect("chunk");
    harness
        .service
        .complete_blob_upload(&early_id, &manifest, &owner_actor, NOW + 5)
        .expect("published");
    // 在 NOW+10 发布（保留期到 NOW+20）。
    let late = descriptor(104, &group, seq, chunk, 1);
    let late_id = begin_upload(&mut harness, &owner_actor, &late, 204, NOW + 10);
    harness
        .service
        .put_blob_chunk(&late_id, 0, chunk, &owner_actor, NOW + 10)
        .expect("chunk");
    harness
        .service
        .complete_blob_upload(&late_id, &manifest, &owner_actor, NOW + 10)
        .expect("published");

    // 未到期：什么都不回收。
    assert_eq!(
        harness.service.collect_expired(NOW + 14).expect("collect"),
        CleanupStats::ZERO
    );
    // 早期对象到期：只回收它。
    let stats = harness.service.collect_expired(NOW + 15).expect("collect");
    assert_eq!(stats.expired_blob_objects, 1);
    assert_eq!(stats.expired_blob_chunks, 1);
    harness
        .service
        .read_blob_chunk(&early.blob_id, 0, &owner_actor, NOW + 15)
        .expect_err("collected blob is gone");
    // 晚期对象仍可读（未过期对象绝不被删）。
    let chunk_read = harness
        .service
        .read_blob_chunk(&late.blob_id, 0, &owner_actor, NOW + 15)
        .expect("fresh blob survives");
    assert_eq!(chunk_read.bytes, chunk);
    let _ = member;
}

/// 信箱条目：未 ACK 与已 ACK 分别计数清理。
#[test]
fn expired_mailbox_items_are_counted_by_ack_state() {
    let (mut harness, group, owner, member, seq) = fixture(512);
    let owner_actor = harness.authenticate(&owner, NOW).expect("auth");
    let item = |id: &str, expires_at: u64, payload: &[u8]| MailboxItem {
        item_id: id.to_owned(),
        group_id: group.clone(),
        target_device_id: member.device_id.clone(),
        source_seq: seq,
        expires_at,
        ciphertext_envelope: hex_encode(payload),
    };
    harness
        .service
        .store_mailbox_item(&item("item-acked", NOW + 10, b"acked"), &owner_actor, NOW)
        .expect("stored");
    harness
        .service
        .store_mailbox_item(&item("item-pending", NOW + 10, b"pending"), &owner_actor, NOW)
        .expect("stored");
    harness
        .service
        .store_mailbox_item(&item("item-future", NOW + 3_600, b"future"), &owner_actor, NOW)
        .expect("stored");
    let member_actor = harness.authenticate(&member, NOW).expect("auth");
    harness
        .service
        .ack_mailbox_items(&member_actor, &["item-acked".to_owned()], NOW)
        .expect("ack");

    let stats = harness.service.collect_expired(NOW + 10).expect("collect");
    assert_eq!(stats.expired_mailbox_unacked, 1, "only the unacked expired item");
    assert_eq!(stats.expired_mailbox_acked, 1, "the acked expired item is also collected");
    // 未过期条目仍在。
    let page = harness
        .service
        .read_mailbox_page(&member_actor, None, 20, NOW + 10)
        .expect("page");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].item_id, "item-future");
    assert_eq!(page.expired_items, 0, "expired rows are gone, not just hidden");
}

/// 每秒最多清理 max_rows_per_kind 条：多轮收敛，且重复调用归零。
#[test]
fn per_round_limit_applies_and_repeat_runs_are_idempotent() {
    let (mut harness, group, owner, member, seq) = fixture(1);
    let owner_actor = harness.authenticate(&owner, NOW).expect("auth");
    for (index, id) in ["bulk-1", "bulk-2", "bulk-3"].iter().enumerate() {
        let item = MailboxItem {
            item_id: (*id).to_owned(),
            group_id: group.clone(),
            target_device_id: member.device_id.clone(),
            source_seq: seq,
            expires_at: NOW + 10,
            ciphertext_envelope: hex_encode(format!("payload-{index}").as_bytes()),
        };
        harness
            .service
            .store_mailbox_item(&item, &owner_actor, NOW)
            .expect("stored");
    }
    let mut rounds = 0;
    loop {
        let stats = harness.service.collect_expired(NOW + 10).expect("collect");
        rounds += 1;
        if stats.is_empty() {
            break;
        }
        assert_eq!(stats.expired_mailbox_unacked, 1, "one row per round");
        assert!(rounds <= 4, "three rows must converge in three rounds");
    }
    assert_eq!(rounds, 4, "three productive rounds then an empty one");
}

/// 过期挑战与过期公告记录也被回收（同一轮）。
#[test]
fn expired_challenges_are_collected() {
    let (mut harness, ..) = fixture(512);
    for _ in 0..2 {
        harness
            .service
            .issue_auth_challenge(ChallengePurpose::RequestAuth, NOW)
            .expect("challenge");
    }
    let stats = harness.service.collect_expired(NOW + 3_600).expect("collect");
    assert!(stats.expired_challenges >= 2);
    // 再扫一遍没有新挑战可删（同一时刻新签发的挑战未过期）。
    harness
        .service
        .issue_auth_challenge(ChallengePurpose::RequestAuth, NOW + 3_600)
        .expect("challenge");
    let again = harness.service.collect_expired(NOW + 3_600).expect("collect");
    assert_eq!(again.expired_challenges, 0);
    assert!(again.is_empty());
}
