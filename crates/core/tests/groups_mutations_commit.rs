//! GROUPS-016：禁言 / 改名自动准备真实 MLS 控制提交（真双设备两库）。
//!
//! 覆盖：改名/禁言默认准备 pending 提交（epoch 不推进、GroupView 如实报告）；接受后 epoch +1 且策略保持；
//! local_only=true 不产生 pending；失败（越权 / 自我禁言 / 无设备 / 已有 pending）不留半状态。
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, GroupView, PeerEntryInput, PeerEntryOutcome,
    PolicySyncIntent, accept_pending_control, create_group_with_welcome, group_join_from_welcome,
    group_process_peer_entry, group_snapshot, rename_group_with_commit, set_muted_with_commit,
};
use chat_crypto::control::WelcomeAcceptance;
use chat_crypto::engine::MlsDevice;
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store};

const MASTER_KEY: [u8; 32] = [0x5a; 32];

struct FixtureDir(PathBuf);

impl FixtureDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "chat-core-mutcommit-{tag}-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn open(&self) -> Store {
        open_store(
            StoreOptions::new(self.0.join("chat.db"), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(MASTER_KEY),
        )
        .expect("open formal store")
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    _dir_a: FixtureDir,
    _dir_b: FixtureDir,
    store_a: Store,
    store_b: Store,
    alice: MlsDevice,
    bob: MlsDevice,
    group_id: String,
    view_a: GroupView,
}

impl Fixture {
    fn new() -> Self {
        let dir_a = FixtureDir::new("a");
        let dir_b = FixtureDir::new("b");
        let mut store_a = dir_a.open();
        let mut store_b = dir_b.open();
        let mut alice = MlsDevice::new("alice").expect("alice");
        let mut bob = MlsDevice::new("bob").expect("bob");
        let request = CreateGroupRequest::new("提交化群", "alice");
        let peer = GroupBootstrapPeer {
            credential: "bob".to_owned(),
            device: &bob,
        };
        let (view_a, welcome) =
            create_group_with_welcome(&mut alice, &peer, &mut store_a, &request).expect("create");
        let acceptance = WelcomeAcceptance {
            group_id: view_a.mls_group_key.clone(),
            expected_epoch: view_a.epoch,
            members: vec!["alice".to_owned(), "bob".to_owned()],
        };
        let view_b = group_join_from_welcome(
            &mut bob,
            &mut store_b,
            "提交化群",
            &welcome,
            &acceptance,
            "bob",
            "alice",
        )
        .expect("bob joins");
        assert_eq!(view_b.group_id, view_a.group_id);
        Self {
            _dir_a: dir_a,
            _dir_b: dir_b,
            store_a,
            store_b,
            alice,
            bob,
            group_id: view_a.group_id.clone(),
            view_a,
        }
    }

    fn pending_commit(&self) -> Vec<u8> {
        self.alice
            .pending_control()
            .expect("pending query")
            .expect("pending commit")
            .commit
    }
}

fn muted_of(view: &GroupView, credential: &str) -> bool {
    view.members
        .iter()
        .find(|member| member.credential == credential)
        .map(|member| member.muted)
        .expect("member present")
}

#[test]
fn rename_prepares_a_pending_commit_and_accept_keeps_the_name() {
    let mut fixture = Fixture::new();
    let before = group_snapshot(&fixture.store_a, &fixture.group_id).expect("snapshot");
    let view = rename_group_with_commit(
        &mut fixture.alice,
        &mut fixture.store_a,
        &fixture.group_id,
        "alice",
        "提交化的名字",
        false,
    )
    .expect("rename with commit");
    // 默认路径：pending 提交存在、epoch 不推进、名字与策略版本已更新。
    assert!(view.pending.is_some(), "a pending control commit is prepared");
    assert_eq!(view.epoch, before.epoch);
    assert_eq!(view.name, "提交化的名字");
    assert!(view.policy_version > before.policy_version);
    assert!(fixture.alice.pending_control().expect("query").is_some());

    // 接受后 epoch +1，名字保持。
    let accepted = accept_pending_control(&mut fixture.alice, &mut fixture.store_a, &fixture.group_id)
        .expect("accept the prepared commit");
    assert_eq!(accepted.epoch, before.epoch + 1);
    assert_eq!(accepted.name, "提交化的名字");
    assert!(accepted.pending.is_none());
    assert!(fixture.alice.pending_control().expect("query").is_none());
}

#[test]
fn mute_prepares_a_pending_commit_and_syncs_to_the_peer_after_acceptance() {
    let mut fixture = Fixture::new();
    let view = set_muted_with_commit(
        &mut fixture.alice,
        &mut fixture.store_a,
        &fixture.group_id,
        "alice",
        "bob",
        true,
        false,
    )
    .expect("mute with commit");
    assert!(view.pending.is_some());
    assert_eq!(view.epoch, 1);
    assert!(muted_of(&view, "bob"));
    let commit = fixture.pending_commit();
    accept_pending_control(&mut fixture.alice, &mut fixture.store_a, &fixture.group_id)
        .expect("accept the mute commit");

    // 端到端：对端用同一条控制提交 + 策略声明即可看到自己被禁言。
    let input = PeerEntryInput::new(fixture.group_id.clone(), 1, "alice", commit)
        .with_policy_sync(PolicySyncIntent::SetMuted {
            credential: "bob".to_owned(),
            muted: true,
        });
    let outcome = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &input)
        .expect("peer processes the mute commit");
    let peer_view = match outcome {
        PeerEntryOutcome::Commit { new_epoch, view, .. } => {
            assert_eq!(new_epoch, 2);
            view
        }
        other => panic!("expected a commit outcome, got {other:?}"),
    };
    assert!(muted_of(&peer_view, "bob"));
}

#[test]
fn local_only_does_not_prepare_a_control_commit() {
    let mut fixture = Fixture::new();
    let view = rename_group_with_commit(
        &mut fixture.alice,
        &mut fixture.store_a,
        &fixture.group_id,
        "alice",
        "只在本机",
        true,
    )
    .expect("local only rename");
    assert!(view.pending.is_none(), "local_only must not prepare a commit");
    assert_eq!(view.epoch, 1);
    assert_eq!(view.name, "只在本机");
    assert!(fixture.alice.pending_control().expect("query").is_none());

    // 禁言同理（local_only 只改策略，不产生 pending）。
    let view = set_muted_with_commit(
        &mut fixture.alice,
        &mut fixture.store_a,
        &fixture.group_id,
        "alice",
        "bob",
        true,
        true,
    )
    .expect("local only mute");
    assert!(view.pending.is_none());
    assert!(muted_of(&view, "bob"));
    assert!(fixture.alice.pending_control().expect("query").is_none());
}

#[test]
fn failures_leave_no_half_state() {
    let mut fixture = Fixture::new();
    let before = group_snapshot(&fixture.store_a, &fixture.group_id).expect("snapshot");
    let bob_before = group_snapshot(&fixture.store_b, &fixture.group_id).expect("snapshot b");

    // ① 普通成员越权改名 → PERMISSION_DENIED，且没有准备出 pending。
    let error = rename_group_with_commit(
        &mut fixture.bob,
        &mut fixture.store_b,
        &fixture.group_id,
        "bob",
        "成员改的名字",
        false,
    )
    .expect_err("a plain member cannot rename");
    assert_eq!(error.code(), "PERMISSION_DENIED");
    assert!(fixture.bob.pending_control().expect("query").is_none());
    assert_eq!(
        group_snapshot(&fixture.store_b, &fixture.group_id).expect("snapshot b"),
        bob_before,
        "a rejected rename must not touch the local snapshot"
    );

    // ② 群主禁言自己 → PERMISSION_DENIED，不准备 pending（DOMAIN-002 拒绝）。
    let error = set_muted_with_commit(
        &mut fixture.alice,
        &mut fixture.store_a,
        &fixture.group_id,
        "alice",
        "alice",
        true,
        false,
    )
    .expect_err("the owner cannot mute itself");
    assert_eq!(error.code(), "PERMISSION_DENIED");
    assert!(fixture.alice.pending_control().expect("query").is_none());

    // ③ 没有任何 MLS 群的设备 → 显式 INVALID_TARGET（绝不静默降级成本地变更）。
    let mut ghost = MlsDevice::new("ghost").expect("ghost device");
    let error = rename_group_with_commit(
        &mut ghost,
        &mut fixture.store_a,
        &fixture.group_id,
        "alice",
        "幽灵改的名字",
        false,
    )
    .expect_err("a device without the group cannot commit");
    assert_eq!(error.code(), "INVALID_TARGET");
    assert_eq!(
        group_snapshot(&fixture.store_a, &fixture.group_id).expect("snapshot"),
        before
    );

    // ④ 已有 pending 时再次变更 → INVALID_STATE_TRANSITION，且第一次的 pending 不被覆盖。
    rename_group_with_commit(
        &mut fixture.alice,
        &mut fixture.store_a,
        &fixture.group_id,
        "alice",
        "第一份提交",
        false,
    )
    .expect("first rename prepares a commit");
    let first_commit = fixture.pending_commit();
    let error = set_muted_with_commit(
        &mut fixture.alice,
        &mut fixture.store_a,
        &fixture.group_id,
        "alice",
        "bob",
        true,
        false,
    )
    .expect_err("a second change requires accepting or discarding the first commit");
    assert_eq!(error.code(), "INVALID_STATE_TRANSITION");
    assert_eq!(fixture.pending_commit(), first_commit, "the first commit is intact");
    let _ = &fixture.view_a;
}
