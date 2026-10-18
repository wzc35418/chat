//! NODE-005~008 群登记/日志追加/分页/签名 head 的公开接口测试。
//!
//! 覆盖：重复登记幂等与换内容冲突、跨账户拒绝、同 submission 幂等/相异摘要拒绝、
//! seq 连续与 prev_hash 哈希链、提交后响应丢失重试同结果、8 路并发连续序号、
//! 单事务原子性（进程中途 abort 后不留半条日志）、分页上限与成员窗口、head 绑定与重启稳定。
mod support;

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use chat_node_services::group::{
    AppendResult, KIND_APPLICATION, KIND_CONTROL, ROLE_MEMBER, ROLE_OWNER, SignedLogHead,
};
use chat_node_services::services::NodeAuthPolicy;
use ed25519_dalek::{Signature, Signer};
use support::{Harness, NOW, TestDevice, device_text, group_text};

static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// 每个测试独占一个临时目录（进程内并行安全）。
fn temp_dir(tag: &str) -> PathBuf {
    let unique = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "chat-node-group-{}-{}-{}",
        std::process::id(),
        unique,
        tag
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn sign_creation(
    creation: &mut chat_node_services::group::SignedGroupCreation,
    signer: &TestDevice,
) {
    let signature: Signature = signer.key.sign(&creation.creation.signing_bytes().unwrap());
    creation.device_signature = chat_node_services::canonical::hex_encode(&signature.to_bytes());
}

#[test]
fn register_group_is_idempotent_and_rejects_conflicts() {
    let mut h = Harness::in_memory();
    let a = Harness::device(21);
    let b = Harness::device(22);
    let a_actor = h.registered_actor(&a, NOW);
    h.register(&b, NOW).expect("second device registers");
    let group = group_text(1);
    let creation = h.signed_creation(&a, &group, &[(21, ROLE_OWNER), (22, ROLE_MEMBER)]);

    let first = h
        .service
        .register_group(&creation, &a_actor, NOW)
        .expect("group registers");
    first
        .verify(&h.node_public_key())
        .expect("node-signed registration");
    assert_eq!(first.group_id, group);
    assert_eq!(first.owner_account_id, "acct-a");
    assert_eq!(first.coordinator_id, "node-group-test");
    assert_eq!(first.policy_version, 1);
    assert_eq!(first.mls_epoch, 0);
    assert_eq!(first.members.len(), 2);
    assert_eq!(first.members[0].role, ROLE_OWNER);

    // 同 ID 同摘要重试（新挑战）：返回同一份持久回执。
    let again_actor = h.authenticate(&a, NOW).expect("re-authenticate");
    let again = h
        .service
        .register_group(&creation, &again_actor, NOW)
        .expect("idempotent repeat");
    assert_eq!(first, again);

    // 同 ID 换内容（policy_version）→ GROUP_ID_CONFLICT，不覆盖已登记状态。
    let mut divergent = creation.clone();
    divergent.creation.policy_version = 2;
    sign_creation(&mut divergent, &a);
    let actor = h.authenticate(&a, NOW).expect("auth");
    assert_eq!(
        h.service
            .register_group(&divergent, &actor, NOW)
            .unwrap_err()
            .code(),
        "GROUP_ID_CONFLICT"
    );

    // 同 ID 换 owner 也拒绝：另一账户设备声称 acct-a 为群主。
    h.switch_account(31, "acct-b");
    let c = Harness::device(41);
    h.register(&c, NOW).expect("acct-b device registers");
    let mut claims_a = h.signed_creation(&c, &group_text(2), &[(41, ROLE_OWNER)]);
    claims_a.creation.owner_account_id = "acct-a".into();
    sign_creation(&mut claims_a, &c);
    let c_actor = h.authenticate(&c, NOW).expect("auth");
    assert_eq!(
        h.service
            .register_group(&claims_a, &c_actor, NOW)
            .unwrap_err()
            .code(),
        "PERMISSION_DENIED"
        );

    // 初始成员跨账户同样拒绝。
    let mut foreign_member = h.signed_creation(&c, &group_text(3), &[(41, ROLE_OWNER)]);
    foreign_member.creation.members.push(
        chat_node_services::group::GroupMemberSpec {
            device_id: device_text(21),
            role: ROLE_MEMBER.into(),
        },
    );
    sign_creation(&mut foreign_member, &c);
    let actor = h.authenticate(&c, NOW).expect("auth");
    assert_eq!(
        h.service
            .register_group(&foreign_member, &actor, NOW)
            .unwrap_err()
            .code(),
        "PERMISSION_DENIED"
    );
}

#[test]
fn append_is_idempotent_and_rejects_divergent_bytes() {
    let mut h = Harness::in_memory();
    let a = Harness::device(21);
    let a_actor = h.registered_actor(&a, NOW);
    let group = group_text(4);
    h.service
        .register_group(&h.single_member_group(&a, &group), &a_actor, NOW)
        .expect("group registers");

    let first_candidate = h.application_candidate(&a, &group, 71, b"first cipher");
    let first = h
        .service
        .append_group_entry(&group, &first_candidate, &a_actor, NOW)
        .expect("append accepted");
    assert_eq!(first.seq, 1);
    assert_eq!(
        first.previous_hash,
        chat_node_services::canonical::hex_encode(&[0_u8; 32])
    );
    first
        .verify(&h.node_public_key())
        .expect("node-signed receipt");
    first
        .verify_candidate(&first_candidate, support::MAX_CIPHERTEXT)
        .expect("receipt binds candidate");

    // 同密文、同 submission 重试（提交后结果丢失）：同一份回执。
    let retry_actor = h.authenticate(&a, NOW).expect("auth");
    let retried = h
        .service
        .append_group_entry(&group, &first_candidate, &retry_actor, NOW)
        .expect("idempotent retry");
    assert_eq!(first, retried);

    // 同 submission 相异密文：显式拒绝，且不产生第二条日志。
    let divergent = h.application_candidate(&a, &group, 71, b"different cipher");
    let actor = h.authenticate(&a, NOW).expect("auth");
    assert_eq!(
        h.service
            .append_group_entry(&group, &divergent, &actor, NOW)
            .unwrap_err()
            .code(),
        "SUBMISSION_ID_CONFLICT"
    );
    assert_eq!(h.service.audit_group_log(&group).unwrap(), 1);

    // seq 连续且 prev_hash 形成哈希链。
    let second_candidate = h.application_candidate(&a, &group, 72, b"second cipher");
    let second = h
        .service
        .append_group_entry(&group, &second_candidate, &actor, NOW)
        .expect("second append");
    assert_eq!(second.seq, 2);
    assert_eq!(second.previous_hash, first.log_hash);
    assert_eq!(h.service.audit_group_log(&group).unwrap(), 2);
    let third_candidate = h.application_candidate(&a, &group, 73, b"third cipher");
    let third = h
        .service
        .append_group_entry(&group, &third_candidate, &actor, NOW)
        .expect("third append");
    assert_eq!(third.seq, 3);
    assert_eq!(third.previous_hash, second.log_hash);
}

#[test]
fn stale_control_head_and_epoch_are_rejected() {
    let mut h = Harness::in_memory();
    let a = Harness::device(21);
    let b = Harness::device(22);
    let a_actor = h.registered_actor(&a, NOW);
    h.register(&b, NOW).expect("second device registers");
    let group = group_text(5);
    h.service
        .register_group(
            &h.signed_creation(&a, &group, &[(21, ROLE_OWNER), (22, ROLE_MEMBER)]),
            &a_actor,
            NOW,
        )
        .expect("group registers");

    // 两个管理员都基于同一个前置控制头提交控制候选：只有第一个成功。
    let first = h.control_candidate(&a, &group, 81, support::INITIAL_CONTROL_HEAD, b"commit one");
    let accepted = h
        .service
        .append_group_entry(&group, &first, &a_actor, NOW)
        .expect("first control entry accepted");
    assert_eq!(accepted.seq, 1);
    let head_after = h.service.get_group_head(&group, &a_actor).expect("head");
    assert_eq!(head_after.seq, 1);
    assert_eq!(head_after.log_hash, accepted.log_hash);
    assert_eq!(
        head_after.control_head, accepted.log_hash,
        "accepted control entry advances the node-observable control chain head"
    );
    assert_eq!(head_after.policy_version, 1, "node never guesses policy version");
    assert_eq!(head_after.mls_epoch, 0, "node never guesses mls epoch");

    // 第二个管理员仍基于旧控制头：STALE_CONTROL_HEAD。
    let b_actor = h.authenticate(&b, NOW).expect("auth");
    let stale = h.control_candidate(&b, &group, 82, support::INITIAL_CONTROL_HEAD, b"commit two");
    assert_eq!(
        h.service
            .append_group_entry(&group, &stale, &b_actor, NOW)
            .unwrap_err()
            .code(),
        "STALE_CONTROL_HEAD"
    );
    assert_eq!(h.service.audit_group_log(&group).unwrap(), 1);

    // epoch 不是当前值：STALE_EPOCH。
    let new_head = chat_node_services::canonical::hex_decode::<32>("head", &accepted.log_hash).unwrap();
    let wrong_epoch = h.signed_candidate_full(
        &b,
        &group,
        83,
        b"commit three",
        KIND_CONTROL,
        new_head,
        Some(("invite", [0x33; 32])),
        1,
        3,
    );
    assert_eq!(
        h.service
            .append_group_entry(&group, &wrong_epoch, &b_actor, NOW)
            .unwrap_err()
            .code(),
        "STALE_EPOCH"
    );

    // 应用候选同样要求 head/policy/epoch 一致；用新控制头即可通过。
    let application = h.signed_candidate_full(
        &b,
        &group,
        84,
        b"application",
        KIND_APPLICATION,
        new_head,
        None,
        1,
        0,
    );
    let receipt = h
        .service
        .append_group_entry(&group, &application, &b_actor, NOW)
        .expect("application accepted after control head update");
    assert_eq!(receipt.seq, 2);
}

#[test]
fn unauthorized_appends_are_rejected() {
    let mut h = Harness::in_memory();
    let a = Harness::device(21);
    let b = Harness::device(22);
    let a_actor = h.registered_actor(&a, NOW);
    h.register(&b, NOW).expect("second device registers");
    let group = group_text(6);
    h.service
        .register_group(
            &h.signed_creation(&a, &group, &[(21, ROLE_OWNER), (22, ROLE_MEMBER)]),
            &a_actor,
            NOW,
        )
        .expect("group registers");

    // 路由 group 与候选 group 不一致。
    let mismatched = h.application_candidate(&a, &group_text(7), 91, b"wrong group");
    let actor = h.authenticate(&a, NOW).expect("auth");
    assert_eq!(
        h.service
            .append_group_entry(&group, &mismatched, &actor, NOW)
            .unwrap_err()
            .code(),
        "PERMISSION_DENIED"
    );

    // 代别人追加（作者不是演员）。
    let impersonated = h.application_candidate(&b, &group, 92, b"as bob");
    let actor = h.authenticate(&a, NOW).expect("auth");
    assert_eq!(
        h.service
            .append_group_entry(&group, &impersonated, &actor, NOW)
            .unwrap_err()
            .code(),
        "PERMISSION_DENIED"
    );

    // 作者签名被篡改。
    let mut tampered = h.application_candidate(&b, &group, 93, b"tampered");
    tampered.author_signature = chat_node_services::canonical::hex_encode(&[0_u8; 64]);
    let b_actor = h.authenticate(&b, NOW).expect("auth");
    assert_eq!(
        h.service
            .append_group_entry(&group, &tampered, &b_actor, NOW)
            .unwrap_err()
            .code(),
        "INVALID_SIGNATURE"
    );

    // 超上限密文有界拒绝。
    let oversized = h.application_candidate(
        &b,
        &group,
        94,
        &vec![0_u8; support::MAX_CIPHERTEXT + 1],
    );
    assert_eq!(
        h.service
            .append_group_entry(&group, &oversized, &b_actor, NOW)
            .unwrap_err()
            .code(),
        "PAYLOAD_TOO_LARGE"
    );

    // 未知群。
    let actor = h.authenticate(&b, NOW).expect("auth");
    assert_eq!(
        h.service
            .append_group_entry(&group_text(8), &h.application_candidate(&b, &group_text(8), 95, b"x"), &actor, NOW)
            .unwrap_err()
            .code(),
        "GROUP_NOT_FOUND"
    );
}
#[test]
fn log_paging_and_head_are_consistent() {
    let mut h = Harness::in_memory_with(NodeAuthPolicy {
        max_devices_per_account: 3,
        ..NodeAuthPolicy::default()
    });
    let a = Harness::device(21);
    let b = Harness::device(22);
    let outsider = Harness::device(23);
    let a_actor = h.registered_actor(&a, NOW);
    h.register(&b, NOW).expect("second device registers");
    h.register(&outsider, NOW).expect("third device registers");
    let group = group_text(12);
    h.service
        .register_group(
            &h.signed_creation(&a, &group, &[(21, ROLE_OWNER), (22, ROLE_MEMBER)]),
            &a_actor,
            NOW,
        )
        .expect("group registers");
    for (index, payload) in [b"one".as_slice(), b"two".as_slice(), b"three".as_slice()]
        .into_iter()
        .enumerate()
    {
        let actor = h.authenticate(&a, NOW).expect("auth");
        let candidate = h.application_candidate(&a, &group, 100 + index as u8, payload);
        h.service
            .append_group_entry(&group, &candidate, &actor, NOW)
            .expect("append");
    }

    let b_actor = h.authenticate(&b, NOW).expect("auth");
    let first_page = h.service.fetch_group_log(&group, 0, 2, &b_actor).expect("page 1");
    assert_eq!(first_page.entries.len(), 2);
    assert_eq!(first_page.next_after, 2);
    assert!(first_page.has_more);
    assert_eq!(first_page.head.seq, 3);
    first_page.head.verify(&h.node_public_key()).expect("signed head");
    assert_eq!(first_page.entries[0].receipt.seq, 1);
    assert_eq!(
        first_page.entries[1].receipt.previous_hash,
        first_page.entries[0].receipt.log_hash
    );
    // 返回的条目本身可独立验签并联链。
    for entry in &first_page.entries {
        entry.receipt.verify(&h.node_public_key()).expect("receipt");
        entry
            .receipt
            .verify_candidate(&entry.candidate, support::MAX_CIPHERTEXT)
            .expect("candidate binding");
    }

    let second_page = h
        .service
        .fetch_group_log(&group, first_page.next_after, 2, &b_actor)
        .expect("page 2");
    assert_eq!(second_page.entries.len(), 1);
    assert_eq!(second_page.entries[0].receipt.seq, 3);
    assert!(!second_page.has_more);
    assert_eq!(second_page.head, first_page.head, "head is stable across pages");

    // 独立 head 端点给出同一份签名头。
    let head = h.service.get_group_head(&group, &b_actor).expect("head");
    assert_eq!(head, first_page.head);

    // 超出 head 的 after：明确空页而不是错误地回到开头。
    let beyond = h.service.fetch_group_log(&group, 9, 5, &b_actor).expect("beyond");
    assert!(beyond.entries.is_empty());
    assert!(!beyond.has_more);
    assert_eq!(beyond.next_after, 9);

    // limit 上限与非法值显式拒绝。
    assert_eq!(
        h.service
            .fetch_group_log(&group, 0, 0, &b_actor)
            .unwrap_err()
            .code(),
        "INVALID_REQUEST"
    );
    let too_large = h.service.policy().group_log.max_page_entries + 1;
    assert_eq!(
        h.service
            .fetch_group_log(&group, 0, too_large, &b_actor)
            .unwrap_err()
            .code(),
        "INVALID_REQUEST"
    );

    // 未加入该群的已登记设备无权读历史，也不能读 head。
    let outsider_actor = h.authenticate(&outsider, NOW).expect("auth");
    assert_eq!(
        h.service
            .fetch_group_log(&group, 0, 5, &outsider_actor)
            .unwrap_err()
            .code(),
        "HISTORY_NOT_AUTHORIZED"
    );
    assert_eq!(
        h.service
            .get_group_head(&group, &outsider_actor)
            .unwrap_err()
            .code(),
        "PERMISSION_DENIED"
    );
    assert_eq!(
        h.service
            .get_group_head(&group_text(13), &b_actor)
            .unwrap_err()
            .code(),
        "GROUP_NOT_FOUND"
    );
}

#[test]
fn membership_window_limits_history_and_left_device_cannot_append() {
    let dir = temp_dir("window");
    let path = dir.join("node.db");
    let mut h = Harness::open(&path, NodeAuthPolicy::default());
    let a = Harness::device(21);
    let b = Harness::device(22);
    let a_actor = h.registered_actor(&a, NOW);
    h.register(&b, NOW).expect("second device registers");
    let group = group_text(14);
    h.service
        .register_group(
            &h.signed_creation(&a, &group, &[(21, ROLE_OWNER), (22, ROLE_MEMBER)]),
            &a_actor,
            NOW,
        )
        .expect("group registers");
    for index in 0..3_u8 {
        let actor = h.authenticate(&a, NOW).expect("auth");
        let candidate = h.application_candidate(&a, &group, 120 + index, b"history");
        h.service
            .append_group_entry(&group, &candidate, &actor, NOW)
            .expect("append");
    }

    // 设备 b 的成员窗口从 seq 3 开始（模拟后加入的成员）。
    {
        let connection = rusqlite::Connection::open(&path).expect("direct db");
        connection
            .execute(
                "UPDATE group_members SET joined_seq = 3 WHERE group_id = ?1 AND device_id = ?2",
                rusqlite::params![&group, device_text(22)],
            )
            .expect("join window update");
    }
    let b_actor = h.authenticate(&b, NOW).expect("auth");
    let page = h.service.fetch_group_log(&group, 0, 10, &b_actor).expect("window page");
    let sequences: Vec<u64> = page.entries.iter().map(|entry| entry.receipt.seq).collect();
    assert_eq!(
        sequences,
        vec![3],
        "new members cannot read entries before their join position"
    );

    // 退出后既不能读新条目，也不能继续追加。
    {
        let connection = rusqlite::Connection::open(&path).expect("direct db");
        connection
            .execute(
                "UPDATE group_members SET joined_seq = 1, left_seq = 3 WHERE group_id = ?1 AND device_id = ?2",
                rusqlite::params![&group, device_text(22)],
            )
            .expect("leave window update");
    }
    let b_actor = h.authenticate(&b, NOW).expect("auth");
    let page = h.service.fetch_group_log(&group, 0, 10, &b_actor).expect("window page");
    let sequences: Vec<u64> = page.entries.iter().map(|entry| entry.receipt.seq).collect();
    assert_eq!(sequences, vec![1, 2], "left devices lose access to newer entries");
    let candidate = h.application_candidate(&b, &group, 130, b"after leave");
    let b_actor = h.authenticate(&b, NOW).expect("auth");
    assert_eq!(
        h.service
            .append_group_entry(&group, &candidate, &b_actor, NOW)
            .unwrap_err()
            .code(),
        "PERMISSION_DENIED"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn concurrent_appends_allocate_contiguous_sequence() {
    let dir = temp_dir("concurrent");
    let path = dir.join("node.db");
    let a = Harness::device(21);
    let group = group_text(15);
    {
        let mut setup = Harness::open(&path, NodeAuthPolicy::default());
        let actor = setup.registered_actor(&a, NOW);
        let creation = setup.single_member_group(&a, &group);
        setup.service.register_group(&creation, &actor, NOW).expect("group");
    }
    let mut handles = Vec::new();
    for index in 0..8_u8 {
        let path = path.clone();
        let key = a.key.clone();
        let device_id = a.device_id.clone();
        let group = group.clone();
        handles.push(std::thread::spawn(move || {
            let mut node = Harness::open(&path, NodeAuthPolicy::default());
            let device = TestDevice {
                seed: 21,
                device_id,
                key,
            };
            let actor = node.authenticate(&device, NOW).expect("auth");
            let candidate = node.application_candidate(
                &device,
                &group,
                140 + index,
                format!("cipher-{index}").as_bytes(),
            );
            node.service
                .append_group_entry(&group, &candidate, &actor, NOW)
                .expect("concurrent append")
                .seq
        }));
    }
    let mut sequences: Vec<u64> = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread joins"))
        .collect();
    sequences.sort_unstable();
    assert_eq!(
        sequences,
        (1..=8_u64).collect::<Vec<_>>(),
        "concurrent appends must allocate a contiguous sequence"
    );
    let mut node = Harness::open(&path, NodeAuthPolicy::default());
    assert_eq!(node.service.audit_group_log(&group).expect("audit"), 8);
    let actor = node.authenticate(&a, NOW).expect("auth");
    let head = node.service.get_group_head(&group, &actor).expect("head");
    assert_eq!(head.seq, 8);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn head_survives_restart_and_binds_group_sequence_and_hash() {
    let dir = temp_dir("head");
    let path = dir.join("node.db");
    let a = Harness::device(21);
    let group = group_text(16);
    let initial_head: SignedLogHead = {
        let mut h = Harness::open(&path, NodeAuthPolicy::default());
        let actor = h.registered_actor(&a, NOW);
        let creation = h.single_member_group(&a, &group);
        h.service.register_group(&creation, &actor, NOW).expect("group");
        let head = h.service.get_group_head(&group, &actor).expect("head");
        assert_eq!(head.seq, 0);
        assert_eq!(
            head.log_hash,
            chat_node_services::canonical::hex_encode(&[0_u8; 32])
        );
        head.verify(&h.node_public_key()).expect("signed empty head");
        head
    };
    let receipt: AppendResult = {
        let mut h = Harness::open(&path, NodeAuthPolicy::default());
        let actor = h.authenticate(&a, NOW).expect("auth");
        let candidate = h.application_candidate(&a, &group, 150, b"durable");
        h.service
            .append_group_entry(&group, &candidate, &actor, NOW)
            .expect("append")
    };
    let mut h = Harness::open(&path, NodeAuthPolicy::default());
    let actor = h.authenticate(&a, NOW).expect("auth");
    let head = h.service.get_group_head(&group, &actor).expect("head");
    assert_eq!(head.seq, 1);
    assert_eq!(head.log_hash, receipt.log_hash);
    assert_ne!(head.log_hash, initial_head.log_hash);
    head.verify(&h.node_public_key()).expect("head verifies after restart");

    // 篡改 group/seq/hash 都会让验签失败。
    let mut tampered = head.clone();
    tampered.group_id = group_text(17);
    assert!(tampered.verify(&h.node_public_key()).is_err());
    let mut tampered = head.clone();
    tampered.seq = 2;
    assert!(tampered.verify(&h.node_public_key()).is_err());
    let mut tampered = head;
    tampered.mls_epoch = 5;
    assert!(tampered.verify(&h.node_public_key()).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
/// 崩溃实验夹具：已登记设备 + 已登记群的文件节点库。
struct CrashFixture {
    dir: PathBuf,
    db: PathBuf,
    seed_file: PathBuf,
}

fn crash_fixture(tag: &str) -> (CrashFixture, TestDevice, String, chat_node_services::group::SignedCandidate) {
    let dir = temp_dir(tag);
    let db = dir.join("node.db");
    let seed_file = dir.join("node-seed.bin");
    std::fs::write(&seed_file, [7_u8; 32]).expect("seed file");
    let device = Harness::device(21);
    let group = group_text(18);
    let mut h = Harness::open(&db, NodeAuthPolicy::default());
    let actor = h.registered_actor(&device, NOW);
    let creation = h.single_member_group(&device, &group);
    h.service
        .register_group(&creation, &actor, NOW)
        .expect("group registers");
    let candidate = h.application_candidate(&device, &group, 160, b"crash cipher");
    (
        CrashFixture { dir, db, seed_file },
        device,
        group,
        candidate,
    )
}

/// 在指定检查点用子进程 abort()，返回子进程 stderr。
fn run_crash(
    fixture: &CrashFixture,
    group: &str,
    device: &TestDevice,
    candidate: &chat_node_services::group::SignedCandidate,
    point: &str,
) -> String {
    let candidate_file = fixture.dir.join(format!("candidate-{point}.json"));
    std::fs::write(
        &candidate_file,
        serde_json::to_vec(candidate).expect("candidate json"),
    )
    .expect("write candidate");
    let arguments = vec![
        fixture.db.to_string_lossy().into_owned(),
        "node-group-test".to_owned(),
        fixture.seed_file.to_string_lossy().into_owned(),
        group.to_owned(),
        candidate_file.to_string_lossy().into_owned(),
        point.to_owned(),
        "acct-a".to_owned(),
        device.device_id.clone(),
        NOW.to_string(),
    ];
    let output = Command::new(env!("CARGO_BIN_EXE_node-append-crash-worker"))
        .args(&arguments)
        .output()
        .expect("crash worker runs");
    assert!(!output.status.success(), "crash worker must not succeed");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn crash_before_commit_rolls_back_the_whole_log_entry() {
    for point in ["log", "before-commit"] {
        let (fixture, device, group, candidate) = crash_fixture(&format!("crash-{point}"));
        let stderr = run_crash(&fixture, &group, &device, &candidate, point);
        let expected = if point == "log" {
            "AfterLogInsert"
        } else {
            "BeforeCommit"
        };
        assert!(
            stderr.contains(&format!("NODE_APPEND_CRASH_CHECKPOINT={expected}")),
            "child missed checkpoint {expected}: {stderr}"
        );
        let mut h = Harness::open(&fixture.db, NodeAuthPolicy::default());
        assert_eq!(
            h.service.audit_group_log(&group).expect("audit"),
            0,
            "no half log may survive a crash at {point}"
        );
        let actor = h.authenticate(&device, NOW).expect("auth");
        assert_eq!(
            h.service.get_group_head(&group, &actor).expect("head").seq,
            0,
            "head must not advance when the transaction aborted at {point}"
        );
        let retry = h
            .service
            .append_group_entry(&group, &candidate, &actor, NOW)
            .expect("retry after crash");
        assert_eq!(retry.seq, 1);
        assert_eq!(h.service.audit_group_log(&group).expect("audit"), 1);
        assert_eq!(
            h.service.get_group_head(&group, &actor).expect("head").seq,
            1
        );
        let _ = std::fs::remove_dir_all(&fixture.dir);
    }
}

#[test]
fn crash_after_commit_keeps_result_and_retry_is_idempotent() {
    let (fixture, device, group, candidate) = crash_fixture("crash-after");
    let mut h = Harness::open(&fixture.db, NodeAuthPolicy::default());
    let actor = h.authenticate(&device, NOW).expect("auth");
    let first = h.service
        .append_group_entry(&group, &candidate, &actor, NOW)
        .expect("first append");
    assert_eq!(first.seq, 1);
    let committed_by_child = h.application_candidate(&device, &group, 162, b"committed then crashed");
    drop(h);
    let stderr = run_crash(&fixture, &group, &device, &committed_by_child, "after-commit");
    assert!(
        stderr.contains("NODE_APPEND_CRASH_CHECKPOINT=AfterCommit"),
        "child missed commit checkpoint: {stderr}"
    );
    let mut h = Harness::open(&fixture.db, NodeAuthPolicy::default());
    assert_eq!(
        h.service.audit_group_log(&group).expect("audit"),
        2,
        "the committed entry survives the crash"
    );
    let actor = h.authenticate(&device, NOW).expect("auth");
    // 提交后响应丢失：客户端用同一密文重试，必须拿回原 seq/回执。
    let retry = h
        .service
        .append_group_entry(&group, &committed_by_child, &actor, NOW)
        .expect("idempotent retry after lost response");
    assert_eq!(retry.seq, 2);
    let head = h.service.get_group_head(&group, &actor).expect("head");
    assert_eq!(head.seq, 2);
    assert_eq!(head.log_hash, retry.log_hash);
    assert_eq!(head.log_hash, retry.log_hash);
    assert_ne!(head.log_hash, first.log_hash);
    let _ = std::fs::remove_dir_all(&fixture.dir);
}
