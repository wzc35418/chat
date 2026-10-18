//! GROUPS-003/004：成员治理（角色变更 / 禁言）的端到端用例。
//!
//! 覆盖：群主改角色成功且 policy_version 递增（DOMAIN-004 计算）、真实 MLS 控制提交进 pending、
//! 非群主拒绝、改群主拒绝、禁言往返、未知成员拒绝、非法迁移拒绝且不留写入、未决提交阻塞后续写入、
//! 治理后快照重开一致。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, GroupView, create_group, group_snapshot, set_muted,
    set_role,
};
use chat_crypto::engine::MlsDevice;
use chat_domain::MemberRole;
use chat_storage::formal::{
    KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store,
};

const MASTER_KEY: [u8; 32] = [0x5a; 32];

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
            "chat-core-governance-{}-{nanos}-{counter}",
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

/// 治理夹具：alice 建群（owner）、bob 是引导成员（member），没有追加成员 → 建群后无 pending。
struct Fixture {
    dir: FixtureDir,
    store: Store,
    alice: MlsDevice,
    bob: MlsDevice,
    view: GroupView,
}

impl Fixture {
    fn new() -> Self {
        let dir = FixtureDir::new();
        let mut store = open(&dir);
        let mut alice = MlsDevice::new("alice").expect("alice");
        let bob = MlsDevice::new("bob").expect("bob");
        let request = CreateGroupRequest::new("治理测试群", "alice");
        let peer = GroupBootstrapPeer {
            credential: "bob".to_owned(),
            device: &bob,
        };
        let view = create_group(&mut alice, &peer, &mut store, &request).expect("create group");
        Self {
            dir,
            store,
            alice,
            bob,
            view,
        }
    }

    fn member(&self, view: &GroupView, credential: &str) -> chat_core::groups::GroupMemberView {
        view.members
            .iter()
            .find(|member| member.credential == credential)
            .cloned()
            .expect("member exists")
    }

    fn snapshot(&self) -> GroupView {
        group_snapshot(&self.store, &self.view.group_id).expect("snapshot")
    }
}

#[test]
fn owner_changes_a_role_and_policy_version_advances() {
    let mut fixture = Fixture::new();
    let before = fixture.snapshot();
    assert_eq!(before.policy_version, 1);
    assert_eq!(
        before.control_head,
        "0".repeat(64),
        "new groups start from the frozen zero control head"
    );
    assert!(before.pending.is_none());
    assert_eq!(fixture.member(&before, "bob").role, MemberRole::Member);
    assert!(!fixture.member(&before, "bob").muted);

    let after = set_role(
        &mut fixture.alice,
        &mut fixture.store,
        &fixture.view.group_id,
        "alice",
        "bob",
        MemberRole::Admin,
    )
    .expect("owner changes bob's role");

    assert_eq!(after.policy_version, 2, "DOMAIN-004 advanced the policy version");
    assert_ne!(after.control_head, before.control_head, "control head was derived");
    assert_eq!(after.control_head.len(), 64);
    assert_eq!(fixture.member(&after, "bob").role, MemberRole::Admin);
    assert_eq!(fixture.member(&after, "alice").role, MemberRole::Owner);
    assert_eq!(after.role, MemberRole::Owner);
    assert_eq!(after.epoch, 1, "the control commit is not merged yet");

    // 真实 MLS 控制提交（SelfUpdate）已经进入 pending。
    let pending = after.pending.as_ref().expect("pending control commit");
    assert_eq!(pending.operation, "self_update");
    assert_eq!(pending.base_epoch, 1);
    assert_eq!(pending.target_epoch, 2);
    assert_eq!(pending.commit_digest.len(), 64);
    let device_pending = fixture
        .alice
        .pending_control()
        .expect("pending query")
        .expect("pending commit exists");
    assert_eq!(device_pending.operation.code(), "self_update");

    // 快照（正式库）与返回值一致。
    let snapshot = fixture.snapshot();
    assert_eq!(snapshot.policy_version, 2);
    assert_eq!(snapshot.control_head, after.control_head);
    assert_eq!(fixture.member(&snapshot, "bob").role, MemberRole::Admin);
}

#[test]
fn non_owner_cannot_govern() {
    let mut fixture = Fixture::new();
    let group_id = fixture.view.group_id.clone();

    // bob 只是普通成员：改角色需要群主。
    let error = set_role(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "bob",
        "bob",
        MemberRole::Admin,
    )
    .expect_err("a plain member cannot change roles");
    assert_eq!(error.code(), "PERMISSION_DENIED");

    // bob 也不能禁言别人。
    let error = set_muted(&mut fixture.store, &group_id, "bob", "alice", true)
        .expect_err("a plain member cannot mute others");
    assert_eq!(error.code(), "PERMISSION_DENIED");

    // 拒绝没有留下任何写入。
    let snapshot = fixture.snapshot();
    assert_eq!(snapshot.policy_version, 1);
    assert_eq!(snapshot.control_head, "0".repeat(64));
    assert!(snapshot.pending.is_none());
}

#[test]
fn changing_the_owner_or_self_is_rejected() {
    let mut fixture = Fixture::new();
    let group_id = fixture.view.group_id.clone();

    // 不能把自己的群主角色改掉（必须先转让）。
    let error = set_role(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "alice",
        "alice",
        MemberRole::Admin,
    )
    .expect_err("the owner cannot change its own role");
    assert_eq!(error.code(), "PERMISSION_DENIED");

    // 不能直接把别人提升为群主（只能 TransferOwnership）。
    let error = set_role(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "alice",
        "bob",
        MemberRole::Owner,
    )
    .expect_err("ownership requires a transfer");
    assert_eq!(error.code(), "PERMISSION_DENIED");

    // 也不能禁言自己。
    let error = set_muted(&mut fixture.store, &group_id, "alice", "alice", true)
        .expect_err("cannot mute self");
    assert_eq!(error.code(), "PERMISSION_DENIED");

    let snapshot = fixture.snapshot();
    assert_eq!(snapshot.policy_version, 1);
    assert_eq!(fixture.member(&snapshot, "alice").role, MemberRole::Owner);
    assert_eq!(fixture.member(&snapshot, "bob").role, MemberRole::Member);
}

#[test]
fn mute_roundtrip_updates_the_policy_snapshot() {
    let mut fixture = Fixture::new();
    let group_id = fixture.view.group_id.clone();

    let muted = set_muted(&mut fixture.store, &group_id, "alice", "bob", true)
        .expect("owner mutes bob");
    assert!(fixture.member(&muted, "bob").muted);
    assert_eq!(muted.policy_version, 2);
    assert!(muted.pending.is_none(), "mute is a policy-only operation");

    let unmuted = set_muted(&mut fixture.store, &group_id, "alice", "bob", false)
        .expect("owner unmutes bob");
    assert!(!fixture.member(&unmuted, "bob").muted);
    assert_eq!(unmuted.policy_version, 3);
    assert_ne!(unmuted.control_head, muted.control_head);

    // 正式库里的快照与返回值一致。
    let snapshot = fixture.snapshot();
    assert_eq!(snapshot.policy_version, 3);
    assert!(!fixture.member(&snapshot, "bob").muted);
    assert_eq!(snapshot.control_head, unmuted.control_head);

    // 禁言不需要 MLS 提交：设备侧没有 pending。
    assert!(fixture.alice.pending_control().expect("pending query").is_none());
}

#[test]
fn unknown_members_and_illegal_transitions_are_rejected() {
    let mut fixture = Fixture::new();
    let group_id = fixture.view.group_id.clone();

    let error = set_role(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "alice",
        "carol",
        MemberRole::Admin,
    )
    .expect_err("unknown target");
    assert_eq!(error.code(), "INVALID_TARGET");

    let error = set_muted(&mut fixture.store, &group_id, "alice", "nobody", true)
        .expect_err("unknown mute target");
    assert_eq!(error.code(), "INVALID_TARGET");

    let error = set_role(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "mallory",
        "bob",
        MemberRole::Admin,
    )
    .expect_err("unknown actor");
    assert_eq!(error.code(), "INVALID_TARGET");

    // 非法群 ID 也是显式失败。
    let error = set_role(
        &mut fixture.alice,
        &mut fixture.store,
        "not-a-group",
        "alice",
        "bob",
        MemberRole::Admin,
    )
    .expect_err("malformed group id");
    assert_eq!(error.code(), "INVALID_ID");

    let snapshot = fixture.snapshot();
    assert_eq!(snapshot.policy_version, 1);
    assert!(snapshot.pending.is_none());
}

#[test]
fn pending_control_commit_blocks_further_governance() {
    let mut fixture = Fixture::new();
    let group_id = fixture.view.group_id.clone();
    set_role(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "alice",
        "bob",
        MemberRole::Admin,
    )
    .expect("first role change prepares a control commit");

    // 有未决控制提交时，策略层写入被显式拒绝（避免策略头与 commit 顺序不一致）。
    let error = set_muted(&mut fixture.store, &group_id, "alice", "bob", true)
        .expect_err("policy write while a control commit is pending");
    assert_eq!(error.code(), "INVALID_STATE_TRANSITION");

    // 第二次控制提交同样被 crypto 拒绝。
    let error = set_role(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "alice",
        "bob",
        MemberRole::Member,
    )
    .expect_err("second control commit while pending");
    assert_eq!(error.code(), "INVALID_STATE_TRANSITION");

    // 第一次的结果仍然一致（没有半更新）。
    let snapshot = fixture.snapshot();
    assert_eq!(snapshot.policy_version, 2);
    assert_eq!(fixture.member(&snapshot, "bob").role, MemberRole::Admin);
    assert!(!fixture.member(&snapshot, "bob").muted);
    assert_eq!(snapshot.pending.as_ref().expect("pending").operation, "self_update");
}

#[test]
fn governance_snapshot_survives_reopen() {
    let mut fixture = Fixture::new();
    let group_id = fixture.view.group_id.clone();
    set_muted(&mut fixture.store, &group_id, "alice", "bob", true).expect("mute");
    set_muted(&mut fixture.store, &group_id, "alice", "bob", false).expect("unmute");
    let after_role = set_role(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "alice",
        "bob",
        MemberRole::Admin,
    )
    .expect("role change");
    let before_reopen = fixture.snapshot();
    let database = fixture.dir.database();

    // 关闭库后重新打开同一库文件：治理结果与 pending 都还在。
    drop(fixture.store);
    let reopened = open_store(
        StoreOptions::new(&database, KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes(MASTER_KEY),
    )
    .expect("reopen formal store");
    let snapshot = group_snapshot(&reopened, &group_id).expect("snapshot after reopen");
    assert_eq!(snapshot.policy_version, before_reopen.policy_version);
    assert_eq!(snapshot.policy_version, after_role.policy_version);
    assert_eq!(snapshot.control_head, after_role.control_head);
    assert_eq!(snapshot.name, after_role.name);
    assert_eq!(snapshot.epoch, after_role.epoch);
    assert_eq!(snapshot.members, before_reopen.members);
    assert_eq!(
        snapshot.members.iter().find(|m| m.credential == "bob").map(|m| m.role),
        Some(MemberRole::Admin)
    );
    assert!(!snapshot.members.iter().find(|m| m.credential == "bob").expect("bob").muted);
    assert_eq!(snapshot.pending.as_ref().expect("pending").operation, "self_update");
    assert_eq!(snapshot.role, MemberRole::Owner);
    let _ = fixture.bob;
}
