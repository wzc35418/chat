//! GROUPS 控制提交的接受与合并 / 丢弃回滚（CRYPTO-007/008 接线）端到端用例。
//!
//! 覆盖：改角色(pending) → 接受后 epoch 前进、pending 清空、快照一致；第二次接受显式拒绝；
//! 添加成员(pending) → 接受后成员正式入群；丢弃后 epoch 不变、pending 清空、成员回滚、仍可继续治理；
//! 没有 pending 时接受/丢弃显式失败且不留半状态；非法设备接受被拒且 pending 保留。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, GroupMemberKeyPackage, GroupView,
    accept_pending_control, create_group, discard_pending_control, group_snapshot,
    load_group_device, remove_member, set_role,
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
            "chat-core-commit-{}-{nanos}-{counter}",
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

/// 夹具：alice 建群（owner）、bob 是引导成员；可选追加一名成员（carol，建成后是 pending）。
struct Fixture {
    dir: FixtureDir,
    store: Store,
    alice: MlsDevice,
    view: GroupView,
}

impl Fixture {
    fn new() -> Self {
        Self::build(Vec::new())
    }

    fn with_pending_member() -> Self {
        let carol = MlsDevice::new("carol").expect("carol");
        let member = GroupMemberKeyPackage {
            credential: "carol".to_owned(),
            key_package: carol.key_package().expect("carol key package"),
        };
        Self::build(vec![member])
    }

    fn build(extra: Vec<GroupMemberKeyPackage>) -> Self {
        let dir = FixtureDir::new();
        let mut store = open(&dir);
        let mut alice = MlsDevice::new("alice").expect("alice");
        let bob = MlsDevice::new("bob").expect("bob");
        let mut request = CreateGroupRequest::new("提交合并测试群", "alice");
        request.members = extra;
        let peer = GroupBootstrapPeer {
            credential: "bob".to_owned(),
            device: &bob,
        };
        let view = create_group(&mut alice, &peer, &mut store, &request).expect("create group");
        Self {
            dir,
            store,
            alice,
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
fn accept_merges_the_commit_and_advances_the_epoch() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id();
    let prepared = set_role(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "alice",
        "bob",
        MemberRole::Admin,
    )
    .expect("role change prepares a control commit");
    assert_eq!(prepared.policy_version, 2);
    assert_eq!(prepared.epoch, 1);
    assert_eq!(
        prepared.pending.as_ref().expect("pending").operation,
        "self_update"
    );

    let accepted = accept_pending_control(&mut fixture.alice, &mut fixture.store, &group_id)
        .expect("local acceptance merges the commit");
    assert_eq!(accepted.epoch, 2, "epoch advanced to the pending target");
    assert!(accepted.pending.is_none(), "pending cleared after merge");
    assert_eq!(accepted.policy_version, 2, "policy version is not rolled back");
    assert_eq!(
        accepted.members.iter().find(|m| m.credential == "bob").map(|m| m.role),
        Some(MemberRole::Admin)
    );
    assert!(accepted.state_version > prepared.state_version);

    // 设备侧也到新 epoch、没有 pending。
    assert_eq!(fixture.alice.epoch().expect("epoch"), 2);
    assert!(fixture.alice.pending_control().expect("pending").is_none());

    // 正式库快照与返回值一致。
    let snapshot = fixture.snapshot();
    assert_eq!(snapshot.epoch, 2);
    assert!(snapshot.pending.is_none());
    assert_eq!(snapshot.policy_version, 2);
    assert_eq!(snapshot.control_head, accepted.control_head);

    // 第二次接受：没有 pending → 显式拒绝，且状态不变。
    let error = accept_pending_control(&mut fixture.alice, &mut fixture.store, &group_id)
        .expect_err("second accept must be rejected");
    assert_eq!(error.code(), "PENDING_MISMATCH");
    let after = fixture.snapshot();
    assert_eq!(after.epoch, 2);
    assert_eq!(after.state_version, snapshot.state_version);

    // 合并后可以继续治理（新的提交基于新 epoch）。
    let next = set_role(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "alice",
        "bob",
        MemberRole::Member,
    )
    .expect("governance continues after the merge");
    assert_eq!(next.policy_version, 3);
    assert_eq!(
        next.pending.as_ref().expect("pending").base_epoch,
        2,
        "the new commit is based on the merged epoch"
    );
}

#[test]
fn accept_without_pending_is_rejected_and_leaves_state() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id();
    let before = fixture.snapshot();
    assert!(before.pending.is_none());

    let error = accept_pending_control(&mut fixture.alice, &mut fixture.store, &group_id)
        .expect_err("nothing to accept");
    assert_eq!(error.code(), "PENDING_MISMATCH");

    let after = fixture.snapshot();
    assert_eq!(after, before, "a failed accept must not change anything");

    // 不属于该群的设备：显式拒绝，且原群状态完全不变。
    let mut stranger = MlsDevice::new("mallory").expect("mallory");
    let error = accept_pending_control(&mut stranger, &mut fixture.store, &group_id)
        .expect_err("a stranger cannot merge this group's commit");
    assert_eq!(error.code(), "INVALID_TARGET");
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn accept_promotes_pending_members_after_an_add_commit() {
    let mut fixture = Fixture::with_pending_member();
    let group_id = fixture.group_id();
    let pending = fixture.snapshot();
    assert_eq!(Fixture::credentials(&pending), vec!["alice", "bob"]);
    assert_eq!(
        pending
            .pending_members
            .iter()
            .map(|member| member.credential.clone())
            .collect::<Vec<_>>(),
        vec!["carol"],
        "carol is pending, not confirmed"
    );
    assert_eq!(
        pending.pending.as_ref().expect("pending").operation,
        "add_members"
    );

    let accepted =
        accept_pending_control(&mut fixture.alice, &mut fixture.store, &group_id).expect("accept");
    assert_eq!(accepted.epoch, 2);
    assert!(accepted.pending.is_none());
    assert!(accepted.pending_members.is_empty(), "pending list is drained");
    assert_eq!(
        Fixture::credentials(&accepted),
        vec!["alice", "bob", "carol"],
        "carol is a confirmed member after the merge"
    );

    let snapshot = fixture.snapshot();
    assert_eq!(Fixture::credentials(&snapshot), vec!["alice", "bob", "carol"]);
    assert_eq!(snapshot.epoch, 2);
}

#[test]
fn discard_rolls_back_without_advancing_the_epoch() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id();
    let removed = remove_member(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "alice",
        "bob",
    )
    .expect("remove bob locally");
    assert_eq!(removed.policy_version, 2);
    assert_eq!(Fixture::credentials(&removed), vec!["alice"]);
    assert_eq!(
        removed
            .pending_removed
            .iter()
            .map(|member| member.credential.clone())
            .collect::<Vec<_>>(),
        vec!["bob"]
    );

    let discarded = discard_pending_control(&mut fixture.store, &group_id, "node_rejected")
        .expect("discard the rejected commit");
    assert_eq!(discarded.epoch, 1, "the epoch does not advance on discard");
    assert!(discarded.pending.is_none(), "pending cleared");
    assert_eq!(discarded.policy_version, 2, "policy version never rolls back");
    let cleared = discarded
        .last_control_cleared
        .as_ref()
        .expect("clear note is recorded");
    assert_eq!(cleared.operation, "remove_members");
    assert_eq!(cleared.reason_code, "node_rejected");
    assert_eq!(cleared.base_epoch, 1);
    assert_eq!(cleared.target_epoch, 2);
    assert_eq!(cleared.commit_digest.len(), 64);
    assert!(cleared.detail.contains("node_rejected"));
    // 回滚：被移除的成员恢复为正式成员。
    assert_eq!(Fixture::credentials(&discarded), vec!["alice", "bob"]);
    assert!(discarded.pending_removed.is_empty());
    // 丢弃只改库：手持的旧设备已过期，继续治理前必须重新从库装载。
    fixture.alice = load_group_device(&fixture.store, &group_id).expect("reload group device");
    assert_eq!(fixture.alice.epoch().expect("epoch"), 1);
    assert!(fixture.alice.pending_control().expect("pending").is_none());

    // 丢弃后仍可继续治理：再移除一次并接受。
    let removed_again = remove_member(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "alice",
        "bob",
    )
    .expect("remove again after discard");
    assert_eq!(removed_again.policy_version, 3);
    assert_eq!(
        removed_again.pending.as_ref().expect("pending").base_epoch,
        1,
        "still based on epoch 1"
    );
    let accepted = accept_pending_control(&mut fixture.alice, &mut fixture.store, &group_id)
        .expect("accept the second attempt");
    assert_eq!(accepted.epoch, 2);
    assert!(accepted.pending.is_none());
    assert_eq!(Fixture::credentials(&accepted), vec!["alice"]);
    assert!(accepted.pending_removed.is_empty());
}

#[test]
fn discard_of_an_add_commit_drops_the_pending_members() {
    let mut fixture = Fixture::with_pending_member();
    let group_id = fixture.group_id();
    let discarded = discard_pending_control(&mut fixture.store, &group_id, "coordinator_timeout")
        .expect("discard the add commit");
    assert_eq!(discarded.epoch, 1);
    assert!(discarded.pending.is_none());
    assert!(discarded.pending_members.is_empty(), "carol never joined");
    assert_eq!(Fixture::credentials(&discarded), vec!["alice", "bob"]);
    assert_eq!(
        discarded
            .last_control_cleared
            .as_ref()
            .expect("clear note")
            .reason_code,
        "coordinator_timeout"
    );
    assert_eq!(
        discarded
            .last_control_cleared
            .as_ref()
            .expect("clear note")
            .operation,
        "add_members"
    );
}

#[test]
fn discard_without_pending_is_rejected_and_leaves_state() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id();
    let before = fixture.snapshot();

    let error = discard_pending_control(&mut fixture.store, &group_id, "node_rejected")
        .expect_err("nothing to discard");
    assert_eq!(error.code(), "PENDING_MISMATCH");
    assert_eq!(fixture.snapshot(), before);

    // 空/超长/控制字符原因显式拒绝（写入前校验）。
    for reason in ["", "   ", "带\u{0007}控制字符"] {
        let error = discard_pending_control(&mut fixture.store, &group_id, reason)
            .expect_err("invalid reason");
        assert_eq!(error.code(), "INVALID_INPUT", "reason: {reason:?}");
    }

    // 失败后仍可正常准备提交并接受。
    let prepared = set_role(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "alice",
        "bob",
        MemberRole::Admin,
    )
    .expect("prepare after failures");
    assert!(prepared.pending.is_some());
    let accepted = accept_pending_control(&mut fixture.alice, &mut fixture.store, &group_id)
        .expect("accept after failures");
    assert_eq!(accepted.epoch, 2);
}

#[test]
fn accepted_state_survives_reopen() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id();
    set_role(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "alice",
        "bob",
        MemberRole::Admin,
    )
    .expect("prepare");
    let accepted =
        accept_pending_control(&mut fixture.alice, &mut fixture.store, &group_id).expect("accept");
    let database = fixture.dir.database();

    drop(fixture.store);
    let reopened = open_store(
        StoreOptions::new(&database, KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes(MASTER_KEY),
    )
    .expect("reopen formal store");
    let snapshot = group_snapshot(&reopened, &group_id).expect("snapshot after reopen");
    assert_eq!(snapshot.epoch, accepted.epoch);
    assert_eq!(snapshot.epoch, 2);
    assert!(snapshot.pending.is_none());
    assert_eq!(snapshot.policy_version, accepted.policy_version);
    assert_eq!(snapshot.control_head, accepted.control_head);
    assert_eq!(snapshot.members, accepted.members);
}
