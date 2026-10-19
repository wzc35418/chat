//! CTRLREV：撤销设备随控制提交传播（真双设备 + 两个独立临时库）。
//!
//! 正例：A 用 `stage_self_update_commit` 生成**纯撤销**提交（无成员变化）→ 投递 → B 处理
//! 后快照 `revoked_devices` 出现该设备；重复投递被拒且快照不变（不重复计入）。
//! 反例：Application 条目带撤销、与成员/策略声明混用、超 64 条、重复、超长单条 → INVALID_INPUT，
//! 且失败后快照逐字节不变。
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, GroupView, PeerEntryInput, PeerEntryOutcome,
    create_group_with_welcome, group_join_from_welcome, group_process_peer_entry, group_snapshot,
};
use chat_crypto::control::{CommitExpectation, WelcomeAcceptance};
use chat_crypto::engine::MlsDevice;
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store};

const MASTER_KEY: [u8; 32] = [0x33; 32];
const REVOKED: &str = "dev:1:bob-device";

struct FixtureDir(PathBuf);

impl FixtureDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "chat-core-revoked-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> PathBuf {
        self.0.join("chat.db")
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn open(dir: &FixtureDir) -> Store {
    open_store(
        StoreOptions::new(dir.database(), KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes(MASTER_KEY),
    )
    .expect("open formal store")
}

struct Fixture {
    store_b: Store,
    alice: MlsDevice,
    bob: MlsDevice,
    b_view: GroupView,
    dir_b: FixtureDir,
}

impl Fixture {
    fn new() -> Self {
        let dir_a = FixtureDir::new();
        let dir_b = FixtureDir::new();
        let mut store_a = open(&dir_a);
        let mut store_b = open(&dir_b);
        let mut alice = MlsDevice::new("alice").expect("alice");
        let mut bob = MlsDevice::new("bob").expect("bob");
        let request = CreateGroupRequest::new("revoke-group", "alice");
        let peer = GroupBootstrapPeer {
            credential: "bob".to_owned(),
            device: &bob,
        };
        let (a_view, welcome) =
            create_group_with_welcome(&mut alice, &peer, &mut store_a, &request).expect("create");
        let acceptance = WelcomeAcceptance {
            group_id: a_view.mls_group_key.clone(),
            expected_epoch: a_view.epoch,
            members: vec!["alice".to_owned(), "bob".to_owned()],
        };
        let b_view = group_join_from_welcome(
            &mut bob,
            &mut store_b,
            "revoke-group",
            &welcome,
            &acceptance,
            "bob",
            "alice",
        )
        .expect("bob joins");
        Self {
            store_b,
            alice,
            bob,
            b_view,
            dir_b,
        }
    }

    fn group_id(&self) -> String {
        self.b_view.group_id.clone()
    }

    /// A 生成一条**只推进 epoch、不带成员变化**的提交字节并投递（staging 不合并）。
    fn staged_commit(&mut self) -> Vec<u8> {
        self.alice
            .stage_self_update_commit()
            .expect("stage self update commit")
    }

    fn input(&self, payload: Vec<u8>) -> PeerEntryInput {
        let mut input = PeerEntryInput::new(
            self.group_id(),
            self.b_view.epoch,
            "alice",
            payload,
        );
        // 纯撤销提交：没有成员变化，但 epoch 前进一步（SelfUpdate 也算一次更新）。
        input.expectation = Some(CommitExpectation {
            added: 0,
            removed: 0,
            updated: 0,
        });
        input
    }
}

#[test]
fn a_revocation_commit_reaches_the_peer_snapshot() {
    let mut fixture = Fixture::new();
    assert_eq!(fixture.b_view.epoch, 1);
    assert!(fixture.b_view.revoked_devices.is_empty());
    let payload = fixture.staged_commit();
    let mut input = fixture.input(payload);
    input.revoked_devices = vec![REVOKED.to_owned()];
    let outcome = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &input)
        .expect("process the revocation commit");
    let view = match outcome {
        PeerEntryOutcome::Commit { new_epoch, view, .. } => {
            assert_eq!(new_epoch, 2, "the commit advances the epoch by one");
            view
        }
        other => panic!("expected a merged commit, got {other:?}"),
    };
    assert_eq!(view.revoked_devices, vec![REVOKED.to_owned()]);
    let snapshot = group_snapshot(&fixture.store_b, &fixture.group_id()).expect("snapshot");
    assert_eq!(snapshot.revoked_devices, vec![REVOKED.to_owned()]);
    assert_eq!(snapshot.epoch, 2);

    // 重复投递：epoch 已落后 → 显式拒绝，快照不变（不会重复计入）。
    let after = group_snapshot(&fixture.store_b, &fixture.group_id()).expect("snapshot");
    let error = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &input)
        .expect_err("duplicate revocation entry");
    assert_eq!(error.code(), "STALE_EPOCH");
    let unchanged = group_snapshot(&fixture.store_b, &fixture.group_id()).expect("snapshot");
    assert_eq!(unchanged, after, "a rejected duplicate must not change the snapshot");
    assert_eq!(unchanged.revoked_devices.len(), 1);

    // 快照随库重启仍然保留。
    let database = fixture.dir_b.database();
    let group_id = fixture.group_id();
    let Fixture { store_b, dir_b, .. } = fixture;
    drop(store_b);
    let _ = &dir_b;
    let reopened = open_store(
        StoreOptions::new(&database, KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes(MASTER_KEY),
    )
    .expect("reopen store b");
    let restored = group_snapshot(&reopened, &group_id).expect("snapshot after reopen");
    assert_eq!(restored.revoked_devices, vec![REVOKED.to_owned()]);
}

#[test]
fn an_application_entry_may_not_carry_revocations() {
    let mut fixture = Fixture::new();
    let before = group_snapshot(&fixture.store_b, &fixture.group_id()).expect("snapshot");
    let ciphertext = fixture.alice.encrypt("hello").expect("encrypt");
    let mut input = fixture.input(ciphertext);
    input.expectation = None;
    input.revoked_devices = vec![REVOKED.to_owned()];
    let error = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &input)
        .expect_err("an application entry may not revoke devices");
    assert_eq!(error.code(), "INVALID_INPUT");
    let after = group_snapshot(&fixture.store_b, &fixture.group_id()).expect("snapshot");
    assert_eq!(after, before, "a rejected entry must not change the snapshot");
}

#[test]
fn malformed_revocation_lists_are_rejected_without_touching_the_snapshot() {
    let mut fixture = Fixture::new();
    let before = group_snapshot(&fixture.store_b, &fixture.group_id()).expect("snapshot");
    let cases: Vec<Vec<String>> = vec![
        // 超过 64 条。
        (0..65).map(|index| format!("dev:1:{index}")).collect(),
        // 同一设备重复两次。
        vec![REVOKED.to_owned(), REVOKED.to_owned()],
        // 单条超过 128 字节。
        vec!["d".repeat(129)],
        // 空串与控制字符。
        vec![String::new()],
        vec!["dev:1:\u{1}".to_owned()],
    ];
    for devices in cases {
        // 校验发生在 MLS 处理之前，因此不需要真实 Commit 字节。
        let mut input = fixture.input(vec![0x01, 0x02, 0x03]);
        input.revoked_devices = devices.clone();
        let error = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &input)
            .expect_err("malformed revocation list");
        assert_eq!(error.code(), "INVALID_INPUT", "devices: {devices:?}");
        let after = group_snapshot(&fixture.store_b, &fixture.group_id()).expect("snapshot");
        assert_eq!(after, before, "snapshot must stay untouched");
    }
}

#[test]
fn a_revocation_mixed_with_member_changes_is_rejected() {
    let mut fixture = Fixture::new();
    let before = group_snapshot(&fixture.store_b, &fixture.group_id()).expect("snapshot");
    let payload = fixture.staged_commit();
    let mut input = fixture.input(payload);
    input.revoked_devices = vec![REVOKED.to_owned()];
    // 混合：撤销 + 声明成员新增 → 不允许（撤销提交必须是纯撤销）。
    input.added_members = vec!["carol".to_owned()];
    input.expectation = Some(CommitExpectation { added: 1, removed: 0, updated: 0 });
    let error = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &input)
        .expect_err("revocation mixed with member changes");
    assert_eq!(error.code(), "INVALID_INPUT");
    let after = group_snapshot(&fixture.store_b, &fixture.group_id()).expect("snapshot");
    assert_eq!(after, before);
    assert!(after.revoked_devices.is_empty());
}

