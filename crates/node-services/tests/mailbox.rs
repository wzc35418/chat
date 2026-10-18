//! NODE-009~011 密文信箱的公开接口测试。
//!
//! 覆盖：投递幂等与相异密文拒绝、授权关联（必须指向本人已接受条目）、成员校验、
//! 配额硬上限与 ACK 释放、分页游标（跨设备/伪造/重放）、只读不删、过期缺口、
//! ACK 幂等且只作用于本设备条目。
mod support;

use chat_node_services::limits::MailboxPolicy;
use chat_node_services::services::{AuthenticatedDevice, NodeAuthPolicy};
use support::{Harness, NOW, TestDevice, group_text};

/// 小配额策略：便于在不造大密文的前提下证明上限与释放。
fn small_mailbox(max_bytes_per_device: u64, max_page_items: u32) -> NodeAuthPolicy {
    NodeAuthPolicy {
        mailbox: MailboxPolicy {
            max_page_items,
            max_page_bytes: 4096,
            max_item_bytes: 128,
            max_bytes_per_device,
            max_ack_batch: 4,
            default_ttl_seconds: 3_600,
        },
        ..NodeAuthPolicy::default()
    }
}

/// 建一个两成员群，并让 owner 追加一条日志条目，返回 (group, owner device, member device, 条目序号)。
fn group_with_entry(h: &mut Harness) -> (String, TestDevice, TestDevice, u64) {
    let a = Harness::device(21);
    let b = Harness::device(22);
    let a_actor = h.registered_actor(&a, NOW);
    h.register(&b, NOW).expect("second device registers");
    let group = group_text(21);
    h.service
        .register_group(
            &h.signed_creation(&a, &group, &[(21, "owner"), (22, "member")]),
            &a_actor,
            NOW,
        )
        .expect("group registers");
    let candidate = h.application_candidate(&a, &group, 200, b"authorized entry");
    let receipt = h
        .service
        .append_group_entry(&group, &candidate, &a_actor, NOW)
        .expect("entry accepted");
    (group, a, b, receipt.seq)
}

#[test]
fn store_is_idempotent_and_rejects_divergent_payload() {
    let mut h = Harness::in_memory();
    let (group, a, b, seq) = group_with_entry(&mut h);
    let a_actor = h.authenticate(&a, NOW).expect("auth");
    let item = h.mailbox_item("item-1", &group, &b, seq, b"welcome cipher", NOW);
    let receipt = h
        .service
        .store_mailbox_item(&item, &a_actor, NOW)
        .expect("item stored");
    receipt
        .verify(&h.node_public_key())
        .expect("node-signed receipt");
    receipt
        .verify_item(&item, b"welcome cipher")
        .expect("receipt binds item");
    assert_eq!(receipt.target_device_id, b.device_id);
    assert_eq!(receipt.sender_device_id, a.device_id);
    assert_eq!(receipt.source_seq, seq);
    assert_eq!(receipt.byte_size, 14);

    // 重复投递同一 item（同密文）：返回同一份回执。
    let a_actor = h.authenticate(&a, NOW).expect("auth");
    let repeat = h
        .service
        .store_mailbox_item(&item, &a_actor, NOW)
        .expect("idempotent store");
    assert_eq!(receipt, repeat);

    // 同一 item id 换密文：显式冲突。
    let mut divergent = item.clone();
    divergent.ciphertext_envelope = chat_node_services::canonical::hex_encode(b"other cipher");
    let a_actor = h.authenticate(&a, NOW).expect("auth");
    assert_eq!(
        h.service
            .store_mailbox_item(&divergent, &a_actor, NOW)
            .unwrap_err()
            .code(),
        "ITEM_ID_CONFLICT"
    );
}

#[test]
fn store_requires_authorized_entry_and_group_membership() {
    let mut h = Harness::in_memory_with(NodeAuthPolicy {
        max_devices_per_account: 3,
        ..small_mailbox(4096, 10)
    });
    let (group, a, b, seq) = group_with_entry(&mut h);
    let outsider = TestDevice {
        seed: 23,
        device_id: support::device_text(23),
        key: ed25519_dalek::SigningKey::from_bytes(&[23_u8; 32]),
    };
    h.register(&outsider, NOW).expect("third device registers");
    let a_actor = h.authenticate(&a, NOW).expect("auth");

    // source_seq 指向不存在的条目：拒绝（没有授权关联）。
    let unlinked = h.mailbox_item("item-2", &group, &b, seq + 50, b"orphan", NOW);
    assert_eq!(
        h.service
            .store_mailbox_item(&unlinked, &a_actor, NOW)
            .unwrap_err()
            .code(),
        "PERMISSION_DENIED"
    );

    // 目标设备不在该群：拒绝。
    let a_actor = h.authenticate(&a, NOW).expect("auth");
    let foreign_target = h.mailbox_item("item-3", &group, &outsider, seq, b"not a member", NOW);
    assert_eq!(
        h.service
            .store_mailbox_item(&foreign_target, &a_actor, NOW)
            .unwrap_err()
            .code(),
        "PERMISSION_DENIED"
    );

    // 发送者不是该条目作者：不能借别人的条目投递。
    let b_actor = h.authenticate(&b, NOW).expect("auth");
    let hijacked = h.mailbox_item("item-4", &group, &b, seq, b"hijacked", NOW);
    assert_eq!(
        h.service
            .store_mailbox_item(&hijacked, &b_actor, NOW)
            .unwrap_err()
            .code(),
        "PERMISSION_DENIED"
    );

    // 期限必须在（now, now+TTL] 内。
    let a_actor = h.authenticate(&a, NOW).expect("auth");
    let mut expired = h.mailbox_item("item-5", &group, &b, seq, b"expired", NOW);
    expired.expires_at = NOW;
    assert_eq!(
        h.service
            .store_mailbox_item(&expired, &a_actor, NOW)
            .unwrap_err()
            .code(),
        "INVALID_REQUEST"
    );
    let mut too_long = h.mailbox_item("item-6", &group, &b, seq, b"too long", NOW);
    too_long.expires_at = NOW + 30 * 24 * 60 * 60;
    assert_eq!(
        h.service
            .store_mailbox_item(&too_long, &a_actor, NOW)
            .unwrap_err()
            .code(),
        "INVALID_REQUEST"
    );

    // 未登记设备伪造 actor：INVALID_CREDENTIAL。
    let forged = AuthenticatedDevice {
        account_id: "acct-a".into(),
        device_id: support::device_text(99),
        credential_version: 1,
        challenge_id: "forged".into(),
    };
    let item = h.mailbox_item("item-7", &group, &b, seq, b"forged sender", NOW);
    assert_eq!(
        h.service
            .store_mailbox_item(&item, &forged, NOW)
            .unwrap_err()
            .code(),
        "INVALID_CREDENTIAL"
    );
}

#[test]
fn quota_is_enforced_and_released_by_ack() {
    let mut h = Harness::in_memory_with(small_mailbox(200, 10));
    let (group, a, b, seq) = group_with_entry(&mut h);
    let payload = vec![0x5a_u8; 100];
    for index in 0..2_u8 {
        let a_actor = h.authenticate(&a, NOW).expect("auth");
        let item = h.mailbox_item(&format!("quota-{index}"), &group, &b, seq, &payload, NOW);
        h.service
            .store_mailbox_item(&item, &a_actor, NOW)
            .expect("within quota");
    }
    // 第三条超过 200 字节配额：显式拒绝，不静默丢弃。
    let a_actor = h.authenticate(&a, NOW).expect("auth");
    let overflow = h.mailbox_item("quota-2", &group, &b, seq, &payload, NOW);
    assert_eq!(
        h.service
            .store_mailbox_item(&overflow, &a_actor, NOW)
            .unwrap_err()
            .code(),
        "QUOTA_EXCEEDED"
    );
    // ACK 第一条后释放 100 字节，配额重新可用。
    let b_actor = h.authenticate(&b, NOW).expect("auth");
    let summary = h
        .service
        .ack_mailbox_items(&b_actor, &["quota-0".to_owned()], NOW)
        .expect("ack releases quota");
    assert_eq!(summary.acknowledged, vec!["quota-0".to_owned()]);
    assert_eq!(summary.released_bytes, 100);
    let a_actor = h.authenticate(&a, NOW).expect("auth");
    let item = h.mailbox_item("quota-2", &group, &b, seq, &payload, NOW);
    h.service
        .store_mailbox_item(&item, &a_actor, NOW)
        .expect("quota released");
}

#[test]
fn read_page_paginates_and_rejects_foreign_or_forged_cursors() {
    let mut h = Harness::in_memory_with(small_mailbox(4096, 2));
    let (group, a, b, seq) = group_with_entry(&mut h);
    for index in 0..3_u8 {
        let a_actor = h.authenticate(&a, NOW).expect("auth");
        let item = h.mailbox_item(&format!("page-{index}"), &group, &b, seq, b"cipher", NOW);
        h.service
            .store_mailbox_item(&item, &a_actor, NOW)
            .expect("stored");
    }
    let b_actor = h.authenticate(&b, NOW).expect("auth");
    let first = h
        .service
        .read_mailbox_page(&b_actor, None, 2, NOW)
        .expect("page 1");
    assert_eq!(first.items.len(), 2);
    let cursor = first.next_cursor.clone().expect("next cursor");
    assert_eq!(first.expired_items, 0);
    // 读不删除：同一请求重复读得到同一批材料。
    let repeat = h
        .service
        .read_mailbox_page(&b_actor, None, 2, NOW)
        .expect("page 1 repeat");
    assert_eq!(first.items, repeat.items);
    let second = h
        .service
        .read_mailbox_page(&b_actor, Some(&cursor), 2, NOW)
        .expect("page 2");
    assert_eq!(second.items.len(), 1);
    assert!(second.next_cursor.is_none());
    assert_ne!(second.items[0].item_id, first.items[0].item_id);
    assert_eq!(second.items[0].item_id, "page-2");
    // 密文原样返回，不截断。
    assert_eq!(second.items[0].ciphertext_envelope, "636970686572");

    // 非法 limit。
    assert_eq!(
        h.service
            .read_mailbox_page(&b_actor, None, 0, NOW)
            .unwrap_err()
            .code(),
        "INVALID_REQUEST"
    );
    assert_eq!(
        h.service
            .read_mailbox_page(&b_actor, None, 3, NOW)
            .unwrap_err()
            .code(),
        "INVALID_REQUEST"
    );

    // 伪造/跨设备游标：INVALID_CURSOR，绝不退化到“跳到最新页”。
    let a_actor = h.authenticate(&a, NOW).expect("auth");
    assert_eq!(
        h.service
            .read_mailbox_page(&a_actor, Some(&cursor), 2, NOW)
            .unwrap_err()
            .code(),
        "INVALID_CURSOR"
    );
    assert_eq!(
        h.service
            .read_mailbox_page(&b_actor, Some("cur1.deadbeef.not-a-token"), 2, NOW)
            .unwrap_err()
            .code(),
        "INVALID_CURSOR"
    );
    assert_eq!(
        h.service
            .read_mailbox_page(&b_actor, Some("garbage"), 2, NOW)
            .unwrap_err()
            .code(),
        "INVALID_CURSOR"
    );
    // 过期游标同样拒绝（游标有效期 10 分钟）。
    assert_eq!(
        h.service
            .read_mailbox_page(&b_actor, Some(&cursor), 2, NOW + 3_600)
            .unwrap_err()
            .code(),
        "INVALID_CURSOR"
    );
}

#[test]
fn read_hides_expired_items_and_reports_the_gap() {
    let mut h = Harness::in_memory_with(small_mailbox(4096, 10));
    let (group, a, b, seq) = group_with_entry(&mut h);
    let a_actor = h.authenticate(&a, NOW).expect("auth");
    let mut item = h.mailbox_item("ttl-1", &group, &b, seq, b"cipher", NOW);
    item.expires_at = NOW + 30;
    h.service
        .store_mailbox_item(&item, &a_actor, NOW)
        .expect("stored");
    let b_actor = h.authenticate(&b, NOW).expect("auth");
    let page = h
        .service
        .read_mailbox_page(&b_actor, None, 10, NOW)
        .expect("page");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.expired_items, 0);
    // 过期后不再返回，但以缺口计数明确提示，而不是假装“没有消息”。
    let b_actor = h.authenticate(&b, NOW + 31).expect("auth");
    let page = h
        .service
        .read_mailbox_page(&b_actor, None, 10, NOW + 31)
        .expect("page after expiry");
    assert!(page.items.is_empty());
    assert_eq!(page.expired_items, 1);
}

#[test]
fn ack_is_idempotent_and_only_acks_own_items() {
    let mut h = Harness::in_memory_with(small_mailbox(4096, 10));
    let (group, a, b, seq) = group_with_entry(&mut h);
    for index in 0..2_u8 {
        let a_actor = h.authenticate(&a, NOW).expect("auth");
        let item = h.mailbox_item(&format!("ack-{index}"), &group, &b, seq, b"cipher", NOW);
        h.service
            .store_mailbox_item(&item, &a_actor, NOW)
            .expect("stored");
    }
    // 也给 a 自己发一条，用来证明 b 不能 ACK 别人的条目。
    let a_actor = h.authenticate(&a, NOW).expect("auth");
    let own = h.mailbox_item("ack-a", &group, &a, seq, b"cipher", NOW);
    h.service
        .store_mailbox_item(&own, &a_actor, NOW)
        .expect("stored");

    let b_actor = h.authenticate(&b, NOW).expect("auth");
    let summary = h
        .service
        .ack_mailbox_items(&b_actor, &["ack-0".to_owned(), "ack-1".to_owned()], NOW)
        .expect("ack own items");
    assert_eq!(summary.acknowledged.len(), 2);
    assert_eq!(summary.released_bytes, 12);
    assert!(summary.already_consumed.is_empty());

    // 重复 ACK：幂等，不重复释放配额。
    let b_actor = h.authenticate(&b, NOW).expect("auth");
    let repeat = h
        .service
        .ack_mailbox_items(&b_actor, &["ack-0".to_owned(), "ack-1".to_owned()], NOW)
        .expect("repeat ack");
    assert!(repeat.acknowledged.is_empty());
    assert_eq!(repeat.already_consumed.len(), 2);
    assert_eq!(repeat.released_bytes, 0);

    // 他设备条目：整批拒绝，且 a 的条目仍然是 stored。
    let b_actor = h.authenticate(&b, NOW).expect("auth");
    assert_eq!(
        h.service
            .ack_mailbox_items(&b_actor, &["ack-a".to_owned()], NOW)
            .unwrap_err()
            .code(),
        "PERMISSION_DENIED"
    );
    let a_actor = h.authenticate(&a, NOW).expect("auth");
    let page = h
        .service
        .read_mailbox_page(&a_actor, None, 10, NOW)
        .expect("a page");
    assert_eq!(page.items.len(), 1);
    let summary = h
        .service
        .ack_mailbox_items(&a_actor, &["ack-a".to_owned()], NOW)
        .expect("owner acks own item");
    assert_eq!(summary.acknowledged, vec!["ack-a".to_owned()]);

    // 未知条目与畸形批量。
    let b_actor = h.authenticate(&b, NOW).expect("auth");
    assert_eq!(
        h.service
            .ack_mailbox_items(&b_actor, &["unknown-item".to_owned()], NOW)
            .unwrap_err()
            .code(),
        "PERMISSION_DENIED"
    );
    assert_eq!(
        h.service
            .ack_mailbox_items(&b_actor, &[], NOW)
            .unwrap_err()
            .code(),
        "INVALID_ACK_BATCH"
    );
    let too_many: Vec<String> = (0..5).map(|index| format!("bulk-{index}")).collect();
    assert_eq!(
        h.service
            .ack_mailbox_items(&b_actor, &too_many, NOW)
            .unwrap_err()
            .code(),
        "INVALID_ACK_BATCH"
    );
    // 批量里混入未知条目会让整批失败，不产生部分消费。
    let a_actor = h.authenticate(&a, NOW).expect("auth");
    let item = h.mailbox_item("ack-partial", &group, &a, seq, b"cipher", NOW);
    h.service
        .store_mailbox_item(&item, &a_actor, NOW)
        .expect("stored");
    let a_actor = h.authenticate(&a, NOW).expect("auth");
    assert_eq!(
        h.service
            .ack_mailbox_items(&a_actor, &["ack-partial".to_owned(), "nope".to_owned()], NOW)
            .unwrap_err()
            .code(),
        "PERMISSION_DENIED"
    );
    let a_actor = h.authenticate(&a, NOW).expect("auth");
    let page = h
        .service
        .read_mailbox_page(&a_actor, None, 10, NOW)
        .expect("a page");
    assert_eq!(page.items.len(), 1, "no partial consumption on a failed batch");
}
/// 每个测试独占一个临时目录（进程内并行安全）。
fn temp_dir(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "chat-node-mailbox-{}-{}-{}",
        std::process::id(),
        unique,
        tag
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

#[test]
fn mailbox_survives_restart_and_ack_stays_consumed() {
    let dir = temp_dir("restart");
    let path = dir.join("node.db");
    let (group, a, b, seq) = {
        let mut h = Harness::open(&path, small_mailbox(4096, 10));
        group_with_entry(&mut h)
    };
    {
        let mut h = Harness::open(&path, small_mailbox(4096, 10));
        let a_actor = h.authenticate(&a, NOW).expect("auth");
        let item = h.mailbox_item("restart-1", &group, &b, seq, b"cipher", NOW);
        h.service
            .store_mailbox_item(&item, &a_actor, NOW)
            .expect("stored");
    }
    // 节点重启后条目仍可拉取。
    let mut h = Harness::open(&path, small_mailbox(4096, 10));
    let b_actor = h.authenticate(&b, NOW).expect("auth");
    let page = h
        .service
        .read_mailbox_page(&b_actor, None, 10, NOW)
        .expect("page after restart");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].item_id, "restart-1");
    let summary = h
        .service
        .ack_mailbox_items(&b_actor, &["restart-1".to_owned()], NOW)
        .expect("ack");
    assert_eq!(summary.released_bytes, 6);
    drop(h);
    // 重启后 ACK 状态仍是已消费，且不会二次释放配额。
    let mut h = Harness::open(&path, small_mailbox(4096, 10));
    let b_actor = h.authenticate(&b, NOW).expect("auth");
    let page = h
        .service
        .read_mailbox_page(&b_actor, None, 10, NOW)
        .expect("page after ack");
    assert!(page.items.is_empty());
    let summary = h
        .service
        .ack_mailbox_items(&b_actor, &["restart-1".to_owned()], NOW)
        .expect("repeat ack");
    assert_eq!(summary.released_bytes, 0);
    assert_eq!(summary.already_consumed, vec!["restart-1".to_owned()]);
    let _ = std::fs::remove_dir_all(&dir);
}
