//! GROUPS-005/006：移除成员与改名的端到端用例（真实 OpenMLS + 临时正式加密库）。
//!
//! 覆盖：移除成功且 policy_version 递增、被移除者不在 members、非治理者/移除群主/移除自己/未知成员拒绝、
//! 改名成功、同名幂等无操作、非法名拒绝、治理后快照重开一致、pending 期间第二次治理拒绝。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, GroupView, create_group, group_snapshot, remove_member,
    rename_group, set_role,
};
use chat_crypto::engine::MlsDevice;
use chat_domain::MemberRole;
use chat_protocol::GroupId;
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
            "chat-core-membership-{}-{nanos}-{counter}",
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

/// 夹具：alice 建群（owner）、bob 是引导成员（member）；没有追加成员 → 建群后无 pending。
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
        let request = CreateGroupRequest::new("原始群名", "alice");
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

    fn group_id(&self) -> String {
        self.view.group_id.clone()
    }

    fn snapshot(&self) -> GroupView {
        group_snapshot(&self.store, &self.view.group_id).expect("snapshot")
    }

    fn credentials(view: &GroupView) -> Vec<String> {
        view.members
            .iter()
            .map(|member| member.credential.clone())
            .collect()
    }
}

#[test]
fn remove_member_succeeds_and_policy_version_advances() {
    let mut fixture = Fixture::new();
    let before = fixture.snapshot();
    assert_eq!(before.policy_version, 1);
    assert_eq!(Fixture::credentials(&before), vec!["alice", "bob"]);
    assert!(before.pending.is_none());
    let group_id = fixture.group_id();

    let after = remove_member(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "alice",
        "bob",
    )
    .expect("owner removes a member");

    assert_eq!(after.policy_version, 2, "policy version advanced by DOMAIN-004");
    assert_ne!(after.control_head, before.control_head);
    assert_eq!(Fixture::credentials(&after), vec!["alice"], "bob is gone");
    assert_eq!(after.epoch, 1, "the removal commit is not merged yet");
    let pending = after.pending.as_ref().expect("pending removal commit");
    assert_eq!(pending.operation, "remove_members");
    assert_eq!(pending.removed, 1, "one member is staged for removal");
    assert_eq!(pending.added, 0);
    assert_eq!(pending.base_epoch, 1);
    assert_eq!(pending.target_epoch, 2);
    let device_pending = fixture
        .alice
        .pending_control()
        .expect("pending query")
        .expect("pending commit exists");
    assert_eq!(device_pending.operation.code(), "remove_members");
    assert_eq!(device_pending.removed, 1);

    // 正式库快照与返回值一致。
    let snapshot = fixture.snapshot();
    assert_eq!(snapshot.policy_version, 2);
    assert_eq!(Fixture::credentials(&snapshot), vec!["alice"]);
    assert_eq!(snapshot.pending.as_ref().expect("pending").operation, "remove_members");
    assert_eq!(snapshot.control_head, after.control_head);
}

#[test]
fn non_governor_cannot_remove_members() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id();
    // bob 只是普通成员：移除别人需要群主或管理员。
    let error = remove_member(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "bob",
        "alice",
    )
    .expect_err("a plain member cannot remove anyone");
    assert_eq!(error.code(), "PERMISSION_DENIED");

    // 拒绝没有留下任何写入。
    let snapshot = fixture.snapshot();
    assert_eq!(snapshot.policy_version, 1);
    assert_eq!(snapshot.control_head, "0".repeat(64));
    assert!(snapshot.pending.is_none());
    assert_eq!(Fixture::credentials(&snapshot), vec!["alice", "bob"]);
}

#[test]
fn cannot_remove_owner_self_or_unknown_member() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id();

    // 未知成员：写入前拒绝。
    let error = remove_member(&mut fixture.alice, &mut fixture.store, &group_id, "alice", "carol")
        .expect_err("unknown member");
    assert_eq!(error.code(), "INVALID_TARGET");

    // 移除自己：必须走退出流程。
    let error = remove_member(&mut fixture.alice, &mut fixture.store, &group_id, "alice", "alice")
        .expect_err("cannot remove self");
    assert_eq!(error.code(), "PERMISSION_DENIED");

    // 把 bob 升成管理员后，bob 仍不能移除群主（授权先于 MLS 提交，因此即使有 pending 也是权限错）。
    let promoted = set_role(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "alice",
        "bob",
        MemberRole::Admin,
    )
    .expect("promote bob to admin");
    assert_eq!(promoted.policy_version, 2);
    let error = remove_member(&mut fixture.alice, &mut fixture.store, &group_id, "bob", "alice")
        .expect_err("the owner cannot be removed");
    assert_eq!(error.code(), "PERMISSION_DENIED");

    // 群成员与策略版本保持第一次成功操作后的状态。
    let snapshot = fixture.snapshot();
    assert_eq!(snapshot.policy_version, 2);
    assert_eq!(Fixture::credentials(&snapshot), vec!["alice", "bob"]);
    assert_eq!(
        snapshot.members.iter().find(|m| m.credential == "bob").map(|m| m.role),
        Some(MemberRole::Admin)
    );
}

#[test]
fn rename_group_succeeds_and_same_name_is_idempotent() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id();
    let before = fixture.snapshot();

    let renamed = rename_group(&mut fixture.store, &group_id, "alice", "新的群名")
        .expect("owner renames the group");
    assert_eq!(renamed.name, "新的群名");
    assert_eq!(renamed.policy_version, 2);
    assert_ne!(renamed.control_head, before.control_head);
    assert!(renamed.pending.is_none(), "rename is a policy-only operation");
    assert_eq!(renamed.epoch, before.epoch);

    // 同名再次调用：幂等无操作（不推进策略版本、不改记录版本）。
    let again = rename_group(&mut fixture.store, &group_id, "alice", "新的群名")
        .expect("same-name rename is a no-op");
    assert_eq!(again.policy_version, renamed.policy_version);
    assert_eq!(again.state_version, renamed.state_version);
    assert_eq!(again.control_head, renamed.control_head);

    // 正式库快照与返回值一致。
    let snapshot = fixture.snapshot();
    assert_eq!(snapshot.name, "新的群名");
    assert_eq!(snapshot.policy_version, 2);
}

#[test]
fn invalid_names_and_non_governor_renames_are_rejected() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id();
    let before = fixture.snapshot();

    for name in [
        String::new(),
        " 前导空格".to_owned(),
        "尾随空格 ".to_owned(),
        "含\u{0007}控制字符".to_owned(),
        "x".repeat(65),
    ] {
        let error = rename_group(&mut fixture.store, &group_id, "alice", &name)
            .expect_err("invalid name");
        assert_eq!(error.code(), "INVALID_INPUT", "name: {name:?}");
    }

    // 普通成员不能改名。
    let error = rename_group(&mut fixture.store, &group_id, "bob", "bob 想改名")
        .expect_err("a plain member cannot rename");
    assert_eq!(error.code(), "PERMISSION_DENIED");

    // 未知群 ID 显式失败。
    let unknown = GroupId::from_bytes([7_u8; 32]).encode();
    let error = rename_group(&mut fixture.store, &unknown, "alice", "另一个群")
        .expect_err("unknown group");
    assert_eq!(error.code(), "INVALID_TARGET");

    // 全部拒绝都没有写入。
    let snapshot = fixture.snapshot();
    assert_eq!(snapshot.name, before.name);
    assert_eq!(snapshot.policy_version, before.policy_version);
    assert_eq!(snapshot.control_head, before.control_head);
}

#[test]
fn membership_snapshot_survives_reopen() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id();
    // 先改名（策略层，无 pending），再移除成员（产生 pending 提交）。
    let renamed = rename_group(&mut fixture.store, &group_id, "alice", "重开后一致")
        .expect("rename");
    assert_eq!(renamed.policy_version, 2);
    let removed = remove_member(&mut fixture.alice, &mut fixture.store, &group_id, "alice", "bob")
        .expect("remove");
    assert_eq!(removed.policy_version, 3);
    let database = fixture.dir.database();

    // 关闭库后重新打开同一库文件：改名、移除结果、pending 都还在。
    drop(fixture.store);
    let reopened = open_store(
        StoreOptions::new(&database, KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes(MASTER_KEY),
    )
    .expect("reopen formal store");
    let snapshot = group_snapshot(&reopened, &group_id).expect("snapshot after reopen");
    assert_eq!(snapshot.name, "重开后一致");
    assert_eq!(snapshot.policy_version, removed.policy_version);
    assert_eq!(snapshot.control_head, removed.control_head);
    assert_eq!(Fixture::credentials(&snapshot), vec!["alice"]);
    assert_eq!(snapshot.epoch, removed.epoch);
    assert_eq!(
        snapshot.pending.as_ref().expect("pending").operation,
        "remove_members"
    );
    assert_eq!(snapshot.pending.as_ref().expect("pending").removed, 1);
    assert_eq!(snapshot.role, MemberRole::Owner);
    let _ = fixture.bob;
}

#[test]
fn pending_control_commit_blocks_further_governance() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id();
    let removed = remove_member(&mut fixture.alice, &mut fixture.store, &group_id, "alice", "bob")
        .expect("remove bob");
    assert_eq!(removed.policy_version, 2);

    // 有未决移除提交时：改名被策略层显式拒绝。
    let error = rename_group(&mut fixture.store, &group_id, "alice", "改不了")
        .expect_err("policy write while a control commit is pending");
    assert_eq!(error.code(), "INVALID_STATE_TRANSITION");

    // 第二次移除提交同样被拒绝（目标已不在成员里 → INVALID_TARGET；移除自己 → PERMISSION_DENIED）。
    let error = remove_member(&mut fixture.alice, &mut fixture.store, &group_id, "alice", "bob")
        .expect_err("bob is no longer a confirmed member");
    assert_eq!(error.code(), "INVALID_TARGET");

    // 第一次的结果保持一致（没有半更新）。
    let snapshot = fixture.snapshot();
    assert_eq!(snapshot.policy_version, 2);
    assert_eq!(snapshot.name, "原始群名");
    assert_eq!(Fixture::credentials(&snapshot), vec!["alice"]);
    assert_eq!(
        snapshot.pending.as_ref().expect("pending").operation,
        "remove_members"
    );
}
