//! 身份账户 / 联系人 / 邀请一次性消费的 core 服务层测试。
//!
//! 覆盖：真实加密库上的保存/读回、重开（同主密钥）保留、重复保存幂等与冲突、
//! 错误主密钥拒绝且不覆盖、联系人状态机与列表、邀请重复/并发只成功一次、
//! 畸形输入与关闭后的显式失败。

use chat_core::service::{
    ContactUpsertRequest, ConversationCore, IdentityAccountInput, error_code,
};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const MASTER_KEY_HEX: &str = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
const OTHER_MASTER_KEY_HEX: &str =
    "ff02030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
const CREATED_AT: u64 = 1_700_000_000_000;

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "chat-core-identity-{tag}-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture directory");
        Self { dir }
    }

    fn database(&self) -> PathBuf {
        self.dir.join("chat.db")
    }

    fn database_text(&self) -> String {
        self.database().to_string_lossy().into_owned()
    }

    fn open(&self) -> ConversationCore {
        ConversationCore::open(&self.database_text(), MASTER_KEY_HEX).expect("open core")
    }

    fn open_with_key(&self, key_hex: &str) -> anyhow::Result<ConversationCore> {
        ConversationCore::open(&self.database_text(), key_hex)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn seed_hex(start: u8) -> String {
    let mut text = String::with_capacity(64);
    for offset in 0..32u8 {
        text.push_str(&format!("{:02x}", start.wrapping_add(offset)));
    }
    text
}

fn identity_input(display_name: &str) -> IdentityAccountInput {
    IdentityAccountInput {
        display_name: display_name.to_owned(),
        account_seed_hex: seed_hex(1),
        device_seed_hex: seed_hex(101),
        created_at_ms: CREATED_AT,
    }
}

fn contact(account: &str, name: &str, fingerprint: &str, state: &str, at: u64) -> ContactUpsertRequest {
    ContactUpsertRequest {
        account_id: account.to_owned(),
        display_name: name.to_owned(),
        fingerprint: fingerprint.to_owned(),
        state: state.to_owned(),
        updated_at_ms: at,
    }
}

fn account_text(seed: u8) -> String {
    chat_protocol::AccountId::from_bytes([seed; 32]).encode()
}

fn fingerprint_text(seed: u8) -> String {
    chat_identity::Fingerprint::from_bytes([seed; 32]).grouped()
}

#[test]
fn identity_account_round_trips_across_reopen() {
    let fixture = Fixture::new("round-trip");
    let mut core = fixture.open();
    assert!(core.load_identity_account().expect("load").is_none());

    let saved = core
        .save_identity_account(identity_input("Alice"))
        .expect("save identity");
    assert!(!saved.already_existed);
    assert!(saved.account_id.starts_with("acct:1:"), "{}", saved.account_id);
    assert!(saved.device_id.starts_with("dev:1:"), "{}", saved.device_id);
    assert_eq!(saved.display_name, "Alice");
    assert_eq!(saved.fingerprint.len(), 79, "grouped fingerprint text");
    assert_eq!(saved.created_at_ms, CREATED_AT);

    // 同内容重复保存：幂等。
    let again = core
        .save_identity_account(identity_input("Alice"))
        .expect("idempotent save");
    assert!(again.already_existed);
    assert_eq!(again.account_id, saved.account_id);
    assert_eq!(again.fingerprint, saved.fingerprint);

    // 显示名可更新，账户 ID/指纹不变。
    let renamed = core
        .save_identity_account(identity_input("Alice Renamed"))
        .expect("rename");
    assert_eq!(renamed.account_id, saved.account_id);
    assert_eq!(renamed.fingerprint, saved.fingerprint);
    assert_eq!(renamed.display_name, "Alice Renamed");
    assert_eq!(renamed.created_at_ms, saved.created_at_ms);
    core.close();

    // 同主密钥重开读回。
    let core = fixture.open();
    let loaded = core
        .load_identity_account()
        .expect("load")
        .expect("identity present");
    assert_eq!(loaded.account_id, saved.account_id);
    assert_eq!(loaded.device_id, saved.device_id);
    assert_eq!(loaded.display_name, "Alice Renamed");
    assert_eq!(loaded.fingerprint, saved.fingerprint);
}

#[test]
fn identity_duplicate_and_conflict_are_explicit_and_wrong_key_is_rejected() {
    let fixture = Fixture::new("conflict");
    let mut core = fixture.open();
    let saved = core
        .save_identity_account(identity_input("Alice"))
        .expect("save identity");

    // 不同账户种子 → 不同账户 → 明确冲突，不覆盖。
    let mut other = identity_input("Mallory");
    other.account_seed_hex = seed_hex(201);
    other.device_seed_hex = seed_hex(211);
    let error = core
        .save_identity_account(other)
        .expect_err("different account must conflict");
    assert_eq!(error_code(&error), "IDENTITY_ACCOUNT_CONFLICT");
    assert_eq!(
        core.load_identity_account()
            .expect("load")
            .expect("present")
            .account_id,
        saved.account_id
    );
    core.close();

    // 错误主密钥：拒绝打开且不覆盖数据。
    let error = fixture
        .open_with_key(OTHER_MASTER_KEY_HEX)
        .expect_err("wrong key must be rejected");
    assert_eq!(error_code(&error), "STATE_CORRUPTED");
    // 原密钥仍可读回同一账户。
    let core = fixture.open();
    assert_eq!(
        core.load_identity_account()
            .expect("load")
            .expect("present")
            .account_id,
        saved.account_id
    );
}

#[test]
fn contacts_upsert_list_and_enforce_the_identity_state_machine() {
    let fixture = Fixture::new("contacts");
    let mut core = fixture.open();
    let peer = account_text(7);
    let peer_two = account_text(8);

    let requested = core
        .upsert_contact(contact(&peer, "Bob", &fingerprint_text(0x41), "requested", 10))
        .expect("request");
    assert!(requested.contact_id.starts_with("contact-"));
    assert_eq!(requested.state, "requested");
    let accepted = core
        .upsert_contact(contact(&peer, "Bob", &fingerprint_text(0x41), "accepted", 20))
        .expect("accept");
    assert_eq!(accepted.contact_id, requested.contact_id);
    assert_eq!(accepted.state, "accepted");
    core.upsert_contact(contact(
        &peer_two,
        "Carol",
        &fingerprint_text(0x42),
        "accepted",
        30,
    ))
    .expect("second contact");

    // 非法迁移：blocked → accepted 被 identity 冻结表拒绝。
    core.upsert_contact(contact(&peer, "Bob", &fingerprint_text(0x41), "blocked", 40))
        .expect("block");
    let error = core
        .upsert_contact(contact(&peer, "Bob", &fingerprint_text(0x41), "accepted", 50))
        .expect_err("blocked cannot be accepted directly");
    assert_eq!(error_code(&error), "INVALID_STATE_TRANSITION");
    // 合法恢复路径：blocked → declined → requested → accepted。
    core.upsert_contact(contact(&peer, "Bob", &fingerprint_text(0x41), "declined", 60))
        .expect("decline");
    core.upsert_contact(contact(&peer, "Bob", &fingerprint_text(0x41), "requested", 70))
        .expect("re-request");
    core.upsert_contact(contact(&peer, "Bob", &fingerprint_text(0x41), "accepted", 80))
        .expect("accept after re-request");

    let listed = core.list_contacts().expect("list");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].account_id, peer, "most recent event first");
    assert_eq!(listed[0].state, "accepted");
    assert_eq!(listed[0].fingerprint, fingerprint_text(0x41));
    assert_eq!(listed[1].account_id, peer_two);
    core.close();

    // 重开保留状态。
    let core = fixture.open();
    let listed = core.list_contacts().expect("list after reopen");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].state, "accepted");
    assert_eq!(listed[0].updated_at_ms, 80);
}

#[test]
fn invite_consumption_is_one_shot_even_under_concurrent_calls() {
    let fixture = Fixture::new("invite");
    let mut core = fixture.open();
    let token = "inv1.00010203040506070809101112131415161718192021222324252627282930313233343536373839404142434445464748495051525354555657585960616263.aa";
    let invite_id = ConversationCore::invite_id_for_token(token).expect("derive invite id");
    assert!(invite_id.starts_with("invite-"));
    assert_eq!(
        invite_id,
        ConversationCore::invite_id_for_token(token).expect("deterministic")
    );
    assert_ne!(
        invite_id,
        ConversationCore::invite_id_for_token("inv1.other.bb").expect("other id")
    );
    assert_eq!(
        error_code(&ConversationCore::invite_id_for_token("not-an-invite").unwrap_err()),
        "INVALID_INPUT"
    );
    let too_long = format!("inv1.{}", "a".repeat(2000));
    assert_eq!(
        error_code(&ConversationCore::invite_id_for_token(&too_long).unwrap_err()),
        "INVALID_INPUT"
    );

    let consumed = core.consume_invite(&invite_id).expect("first consume");
    assert_eq!(consumed.invite_id, invite_id);
    assert!(consumed.consumed_at_ms > 0);
    let error = core.consume_invite(&invite_id).expect_err("second consume");
    assert_eq!(error_code(&error), "INVALID_STATE_TRANSITION");
    core.close();

    // 重开仍是已消费。
    let mut core = fixture.open();
    let error = core
        .consume_invite(&invite_id)
        .expect_err("still consumed after reopen");
    assert_eq!(error_code(&error), "INVALID_STATE_TRANSITION");

    // 并发：多个线程同时对同一个 invite_id 发起消费，只允许恰好一个成功。
    let core = Arc::new(Mutex::new(core));
    let target = ConversationCore::invite_id_for_token("inv1.race.cc").expect("race id");
    let mut handles = Vec::new();
    for _ in 0..4 {
        let core = Arc::clone(&core);
        let target = target.clone();
        handles.push(std::thread::spawn(move || {
            let mut core = core.lock().expect("lock core");
            match core.consume_invite(&target) {
                Ok(_) => 1u32,
                Err(error) if error_code(&error) == "INVALID_STATE_TRANSITION" => 0,
                Err(_) => panic!("unexpected error kind"),
            }
        }));
    }
    let mut successes = 0u32;
    for handle in handles {
        successes += handle.join().expect("join thread");
    }
    assert_eq!(successes, 1, "exactly one concurrent consume may succeed");
}

#[test]
fn malformed_identity_inputs_and_closed_core_fail_explicitly() {
    let fixture = Fixture::new("validation");
    let mut core = fixture.open();

    let mut bad_seed = identity_input("Alice");
    bad_seed.account_seed_hex = "zz".repeat(32);
    assert_eq!(
        error_code(&core.save_identity_account(bad_seed).unwrap_err()),
        "INVALID_INPUT"
    );
    let mut short_seed = identity_input("Alice");
    short_seed.device_seed_hex = "00".repeat(16);
    assert_eq!(
        error_code(&core.save_identity_account(short_seed).unwrap_err()),
        "INVALID_INPUT"
    );
    assert_eq!(
        error_code(&core.save_identity_account(identity_input("   ")).unwrap_err()),
        "INVALID_TARGET"
    );

    let peer = account_text(9);
    assert_eq!(
        error_code(
            &core
                .upsert_contact(contact(&peer, "Bob", "not-a-fingerprint", "requested", 1))
                .unwrap_err()
        ),
        "MALFORMED_FRAME"
    );
    assert_eq!(
        error_code(
            &core
                .upsert_contact(contact(&peer, "Bob", &fingerprint_text(0x41), "unknown", 1))
                .unwrap_err()
        ),
        "INVALID_INPUT"
    );
    assert_eq!(
        error_code(
            &core
                .upsert_contact(contact("bad id!", "Bob", &fingerprint_text(0x41), "requested", 1))
                .unwrap_err()
        ),
        "INVALID_INPUT"
    );
    assert_eq!(
        error_code(
            &core
                .upsert_contact(contact(&peer, &"x".repeat(65), &fingerprint_text(0x41), "requested", 1))
                .unwrap_err()
        ),
        "INVALID_INPUT"
    );
    assert_eq!(
        error_code(&core.consume_invite("bad invite!").unwrap_err()),
        "INVALID_INPUT"
    );

    core.close();
    assert_eq!(
        error_code(&core.save_identity_account(identity_input("Alice")).unwrap_err()),
        "CORE_CLOSED"
    );
    assert_eq!(
        error_code(&core.load_identity_account().unwrap_err()),
        "CORE_CLOSED"
    );
    assert_eq!(error_code(&core.list_contacts().unwrap_err()), "CORE_CLOSED");
    assert_eq!(
        error_code(&core.consume_invite("invite-1").unwrap_err()),
        "CORE_CLOSED"
    );
}

/// P1-02 可信恢复：写入种子后重开，能用库里的种子重建同一账户，并仍能签发邀请。
#[test]
fn persisted_identity_seeds_rebuild_the_same_account_after_reopen() {
    let fixture = Fixture::new("trusted-path");
    let view = {
        let mut core = fixture.open();
        core.save_identity_account(identity_input("恢复测试"))
            .expect("save identity")
    };

    let mut core = fixture.open();
    let identity = core
        .open_persisted_identity()
        .expect("open persisted identity")
        .expect("identity should exist");
    assert_eq!(identity.account.account_id().encode(), view.account_id);
    assert_eq!(identity.device_id.encode(), view.device_id);
    assert_eq!(
        identity.account.fingerprint().grouped(),
        view.fingerprint,
        "恢复后的指纹必须与库中投影一致"
    );

    // 用恢复出来的根密钥签发邀请；能签出说明私钥确实被恢复（不是空壳账号）。
    let node = chat_protocol::CoordinatorId::from_bytes([0x33; 32]);
    let invite = identity
        .account
        .create_contact_invite(
            &identity.device,
            node,
            chat_identity::InviteScope::ContactRequest,
            CREATED_AT + 1_000,
            60 * 60 * 1000,
        )
        .expect("recovered account must still be able to sign an invite");
    assert!(invite.as_str().starts_with("inv1."));

    // 换错主密钥必须打不开，也就读不到种子。
    assert!(
        fixture.open_with_key(OTHER_MASTER_KEY_HEX).is_err(),
        "错误主密钥必须拒绝打开"
    );

    // 打开但不写入时也应恢复同一身份（读取路径稳定）。
    drop(core);
    let mut reopened = fixture.open();
    let again = reopened
        .open_persisted_identity()
        .expect("reopen identity")
        .expect("identity should still exist");
    assert_eq!(again.account.account_id().encode(), view.account_id);
}
