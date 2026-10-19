//! GROUPS：禁言与群名随控制提交同步到对端（真双设备两库）。
//!
//! 覆盖：A 禁言 B → B 处理带策略声明的 Commit 后自己 muted；A 改名 → B 名字更新；
//! 普通成员越权（改名/禁言他人）被拒且快照不变；群主禁言自己被拒；不带声明时行为不变。
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, GroupView, PeerEntryInput, PeerEntryOutcome,
    PolicySyncIntent, accept_pending_control, create_group_with_welcome, group_join_from_welcome,
    group_process_peer_entry, group_snapshot, rename_group, set_muted,
};
use chat_crypto::control::WelcomeAcceptance;
use chat_crypto::engine::MlsDevice;
use chat_domain::MemberRole;
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
            "chat-core-polsync-{tag}-{}-{nanos}-{counter}",
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
    view_b: GroupView,
}

impl Fixture {
    fn new() -> Self {
        let dir_a = FixtureDir::new("a");
        let dir_b = FixtureDir::new("b");
        let mut store_a = dir_a.open();
        let mut store_b = dir_b.open();
        let mut alice = MlsDevice::new("alice").expect("alice");
        let mut bob = MlsDevice::new("bob").expect("bob");
        let request = CreateGroupRequest::new("策略同步群", "alice");
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
            "策略同步群",
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
            view_b,
        }
    }

    /// 纯策略变更（禁言/改名）本身不产生 MLS 提交：这里显式准备一个 SelfUpdate 提交并接受，
    /// 返回对端要处理的提交字节（真实双端流程：策略先本地生效，提交负责把 epoch 推进给对方）。
    fn prepare_and_accept_self_update(&mut self) -> Vec<u8> {
        let pending = self
            .alice
            .prepare_control(
                chat_crypto::control::ControlOperation::SelfUpdate,
                &mut self.store_a,
            )
            .expect("prepare a self update commit");
        let commit = pending.commit;
        let accepted = accept_pending_control(&mut self.alice, &mut self.store_a, &self.group_id)
            .expect("accept the local commit");
        assert_eq!(accepted.epoch, 2);
        commit
    }
}

/// 让 alice 准备一个 SelfUpdate 提交（不接受到本地），返回提交字节。
fn join_muted_commit(fixture: &mut Fixture) -> Vec<u8> {
    let pending = fixture
        .alice
        .prepare_control(
            chat_crypto::control::ControlOperation::SelfUpdate,
            &mut fixture.store_a,
        )
        .expect("prepare a self update commit");
    pending.commit
}

fn muted_of(view: &GroupView, credential: &str) -> bool {
    view.members
        .iter()
        .find(|member| member.credential == credential)
        .map(|member| member.muted)
        .expect("member present")
}

#[test]
fn mute_syncs_to_the_peer_through_the_control_commit() {
    let mut fixture = Fixture::new();
    // A：禁言 bob（真实 MLS SelfUpdate 提交，本地先挂 pending）。
    set_muted(&mut fixture.store_a, &fixture.group_id, "alice", "bob", true)
        .expect("owner mutes bob");
    let commit = fixture.prepare_and_accept_self_update();

    // B：处理带策略声明的提交 → 自己 muted，且 epoch 恰好 +1。
    let mut input = PeerEntryInput::new(fixture.group_id.clone(), 1, "alice", commit)
        .with_policy_sync(PolicySyncIntent::SetMuted {
            credential: "bob".to_owned(),
            muted: true,
        });
    input.expectation = None;
    let outcome = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &input)
        .expect("bob processes the mute commit");
    let view = match outcome {
        PeerEntryOutcome::Commit { new_epoch, view, .. } => {
            assert_eq!(new_epoch, 2);
            view
        }
        other => panic!("expected a commit outcome, got {other:?}"),
    };
    assert!(muted_of(&view, "bob"), "bob must be muted on the peer");
    assert!(!muted_of(&view, "alice"));
    // 落库后的快照同样成立（不是只有返回值）。
    let snapshot = group_snapshot(&fixture.store_b, &fixture.group_id).expect("snapshot");
    assert!(muted_of(&snapshot, "bob"));
}

#[test]
fn rename_syncs_to_the_peer_through_the_control_commit() {
    let mut fixture = Fixture::new();
    rename_group(
        &mut fixture.store_a,
        &fixture.group_id,
        "alice",
        "改名后的群",
    )
    .expect("owner renames the group");
    let commit = fixture.prepare_and_accept_self_update();
    let input = PeerEntryInput::new(fixture.group_id.clone(), 1, "alice", commit)
        .with_policy_sync(PolicySyncIntent::RenameGroup {
            name: "改名后的群".to_owned(),
        });
    let outcome = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &input)
        .expect("bob processes the rename commit");
    let view = match outcome {
        PeerEntryOutcome::Commit { new_epoch, view, .. } => {
            assert_eq!(new_epoch, 2);
            view
        }
        other => panic!("expected a commit outcome, got {other:?}"),
    };
    assert_eq!(view.name, "改名后的群");
    let snapshot = group_snapshot(&fixture.store_b, &fixture.group_id).expect("snapshot");
    assert_eq!(snapshot.name, "改名后的群");
}

#[test]
fn member_cannot_rename_or_mute_others_and_the_snapshot_is_unchanged() {
    let mut fixture = Fixture::new();
    // bob（普通成员）自己准备一个 SelfUpdate 提交。
    let pending = fixture
        .bob
        .prepare_control(chat_crypto::control::ControlOperation::SelfUpdate, &mut fixture.store_b)
        .expect("bob prepares a self update");
    let commit = pending.commit;
    let before = group_snapshot(&fixture.store_a, &fixture.group_id).expect("snapshot a");

    // A 侧处理 bob 的提交：声明改名 → 普通成员无权重命名 → PERMISSION_DENIED，快照逐字段不变。
    let input = PeerEntryInput::new(
        fixture.group_id.clone(),
        before.epoch,
        "bob",
        commit.clone(),
    )
    .with_policy_sync(PolicySyncIntent::RenameGroup {
        name: "成员改的名字".to_owned(),
    });
    let error = group_process_peer_entry(&mut fixture.alice, &mut fixture.store_a, &input)
        .expect_err("a plain member cannot rename the group");
    assert_eq!(error.code(), "PERMISSION_DENIED");
    let after = group_snapshot(&fixture.store_a, &fixture.group_id).expect("snapshot a again");
    assert_eq!(before, after, "a rejected policy intent must not touch the snapshot");

    // 同一提交声明“禁言 alice”（他人）→ 同样 PERMISSION_DENIED 且不快照变化。
    let input = PeerEntryInput::new(
        fixture.group_id.clone(),
        before.epoch,
        "bob",
        commit.clone(),
    )
    .with_policy_sync(PolicySyncIntent::SetMuted {
        credential: "alice".to_owned(),
        muted: true,
    });
    let error = group_process_peer_entry(&mut fixture.alice, &mut fixture.store_a, &input)
        .expect_err("a plain member cannot mute the owner");
    assert_eq!(error.code(), "PERMISSION_DENIED");
    let after = group_snapshot(&fixture.store_a, &fixture.group_id).expect("snapshot third");
    assert_eq!(before, after);

    // 群主不能禁言自己：alice 的提交 + 目标 alice。
    let self_commit = join_muted_commit(&mut fixture);
    let snapshot_before = group_snapshot(&fixture.store_a, &fixture.group_id).expect("snapshot");
    let input = PeerEntryInput::new(
        fixture.group_id.clone(),
        snapshot_before.epoch,
        "alice",
        self_commit.clone(),
    )
    .with_policy_sync(PolicySyncIntent::SetMuted {
        credential: "alice".to_owned(),
        muted: true,
    });
    let error = group_process_peer_entry(&mut fixture.alice, &mut fixture.store_a, &input)
        .expect_err("the owner cannot mute itself");
    assert_eq!(error.code(), "PERMISSION_DENIED");
    assert_eq!(
        group_snapshot(&fixture.store_a, &fixture.group_id).expect("snapshot"),
        snapshot_before
    );
    // 名字校验：控制字符/过长一律 INVALID_INPUT，且不快照变化（复用上面已准备的提交）。
    let snapshot_before = group_snapshot(&fixture.store_a, &fixture.group_id).expect("snapshot");
    for invalid in ["bad\u{7}name", &"x".repeat(65)] {
        let input = PeerEntryInput::new(
            fixture.group_id.clone(),
            snapshot_before.epoch,
            "alice",
            self_commit.clone(),
        )
        .with_policy_sync(PolicySyncIntent::RenameGroup {
            name: invalid.to_owned(),
        });
        let error = group_process_peer_entry(&mut fixture.alice, &mut fixture.store_a, &input)
            .expect_err("an invalid group name is rejected");
        assert_eq!(error.code(), "INVALID_INPUT", "name {invalid:?}");
        assert_eq!(
            group_snapshot(&fixture.store_a, &fixture.group_id).expect("snapshot"),
            snapshot_before
        );
    }
    let _ = MemberRole::Owner;
}

#[test]
fn without_a_policy_declaration_the_peer_behaviour_is_unchanged() {
    let mut fixture = Fixture::new();
    rename_group(
        &mut fixture.store_a,
        &fixture.group_id,
        "alice",
        "只在本机生效",
    )
    .expect("owner renames");
    let commit = fixture.prepare_and_accept_self_update();
    // 不带任何声明：合并照旧（epoch +1），但名字/禁言保持对端原值（向后兼容）。
    let input = PeerEntryInput::new(fixture.group_id.clone(), 1, "alice", commit);
    let outcome = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &input)
        .expect("commit merges without a declaration");
    let view = match outcome {
        PeerEntryOutcome::Commit { new_epoch, view, .. } => {
            assert_eq!(new_epoch, 2);
            view
        }
        other => panic!("expected a commit outcome, got {other:?}"),
    };
    assert_eq!(view.name, "策略同步群", "no declaration = no name change");
    assert!(!muted_of(&view, "bob"));
    assert_eq!(fixture.view_b.name, "策略同步群");
    let _ = &fixture.view_a;
}
