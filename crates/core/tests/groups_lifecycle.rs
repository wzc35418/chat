//! GROUPS-010/011/012：转让群主 / 退出 / 解散（真 OpenMLS 双设备 + 真临时加密库）。
//!
//! 覆盖：转让后角色互换且策略版本递增、接受后 epoch 前进；非群主/未知目标/转给自己被拒且不留写入；
//! 群主退出被拒、成员退出后本机不再是成员；解散后所有治理显式拒绝且记录保留；关库重开一致。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, GroupView, accept_pending_control, create_group,
    create_group_with_welcome, discard_pending_control, dissolve_group, group_join_from_welcome,
    group_snapshot, leave_group, remove_member, rename_group, set_muted, set_role,
    transfer_ownership,
};
use chat_crypto::control::WelcomeAcceptance;
use chat_crypto::engine::MlsDevice;
use chat_domain::MemberRole;
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store};

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
            "chat-core-lifecycle-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> PathBuf {
        self.0.join("chat.db")
    }

    fn open(&self) -> Store {
        open_store(
            StoreOptions::new(self.database(), KeyProvenance::Raw32),
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
    dir: FixtureDir,
    store: Store,
    alice: MlsDevice,
    bob: MlsDevice,
    view: GroupView,
}

impl Fixture {
    fn new() -> Self {
        let dir = FixtureDir::new();
        let mut store = dir.open();
        let mut alice = MlsDevice::new("alice").expect("alice");
        let bob = MlsDevice::new("bob").expect("bob");
        let request = CreateGroupRequest::new("生命周期群", "alice");
        let peer = GroupBootstrapPeer {
            credential: "bob".to_owned(),
            device: &bob,
        };
        let view = create_group(&mut alice, &peer, &mut store, &request).expect("create group");
        assert_eq!(view.epoch, 1);
        assert_eq!(view.policy_version, 1);
        assert!(view.pending.is_none());
        assert!(!view.dissolved);
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
        group_snapshot(&self.store, &self.group_id()).expect("snapshot")
    }

    fn role_of(&self, view: &GroupView, credential: &str) -> MemberRole {
        view.members
            .iter()
            .find(|member| member.credential == credential)
            .map(|member| member.role)
            .expect("member exists")
    }
}

/// 在投影里查某个成员的角色（不加不依赖 fixture 的借用）。
fn role_in(view: &GroupView, credential: &str) -> MemberRole {
    view.members
        .iter()
        .find(|member| member.credential == credential)
        .map(|member| member.role)
        .expect("member exists")
}

#[test]
fn transfer_swaps_roles_and_advances_policy_version() {
    let mut fixture = Fixture::new();
    let before = fixture.snapshot();
    let group_id = fixture.group_id();

    let transferred = transfer_ownership(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "alice",
        "bob",
    )
    .expect("owner transfers ownership");
    // 角色互换 + 策略版本递增 + 真实控制提交（pending，epoch 不前进）。
    assert_eq!(fixture.role_of(&transferred, "bob"), MemberRole::Owner);
    assert_eq!(fixture.role_of(&transferred, "alice"), MemberRole::Member);
    assert_eq!(transferred.policy_version, before.policy_version + 1);
    assert_ne!(transferred.control_head, before.control_head);
    assert_eq!(transferred.epoch, 1, "transferring does not advance the epoch");
    assert_eq!(transferred.role, MemberRole::Member, "the local device is no longer owner");
    let pending = transferred.pending.as_ref().expect("pending control commit");
    assert_eq!(pending.operation, "self_update");
    assert_eq!(pending.base_epoch, 1);
    assert_eq!(pending.target_epoch, 2);

    // 接受后 epoch 前进，角色保持不变。
    let accepted = accept_pending_control(&mut fixture.alice, &mut fixture.store, &group_id)
        .expect("accept the transfer commit");
    assert_eq!(accepted.epoch, 2);
    assert!(accepted.pending.is_none());
    assert_eq!(fixture.role_of(&accepted, "bob"), MemberRole::Owner);
    assert_eq!(fixture.role_of(&accepted, "alice"), MemberRole::Member);
    assert_eq!(fixture.snapshot(), accepted);

    // 关库重开：转让结果落盘。
    let database = fixture.dir.database();
    drop(fixture.store);
    let reopened = open_store(
        StoreOptions::new(&database, KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes(MASTER_KEY),
    )
    .expect("reopen store");
    let restored = group_snapshot(&reopened, &group_id).expect("snapshot after reopen");
    assert_eq!(restored.epoch, 2);
    assert_eq!(role_in(&restored, "bob"), MemberRole::Owner);
    assert_eq!(restored.policy_version, accepted.policy_version);
    let _ = &fixture.bob;
}

#[test]
fn transfer_rejects_non_owner_unknown_target_and_self_without_writing() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id();
    let before = fixture.snapshot();

    // 非群主转让 → PERMISSION_DENIED（授权层按 actor 凭据判定；本用例用持有该群的设备）。
    let error = transfer_ownership(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "bob",
        "alice",
    )
    .expect_err("member cannot transfer ownership");
    assert_eq!(error.code(), "PERMISSION_DENIED");
    assert_eq!(fixture.snapshot(), before);

    // 目标不是确认成员 → INVALID_TARGET。
    let error = transfer_ownership(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "alice",
        "carol",
    )
    .expect_err("unknown target");
    assert_eq!(error.code(), "INVALID_TARGET");
    assert_eq!(fixture.snapshot(), before);

    // 转给自己 → INVALID_TARGET。
    let error = transfer_ownership(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "alice",
        "alice",
    )
    .expect_err("transfer to self");
    assert_eq!(error.code(), "INVALID_TARGET");
    assert_eq!(fixture.snapshot(), before);

    // 三次失败后本机设备仍无 pending、策略版本未变。
    assert!(fixture.alice.pending_control().expect("pending query").is_none());
    assert_eq!(fixture.snapshot().policy_version, before.policy_version);
}

#[test]
fn owner_cannot_leave_and_member_leave_removes_the_local_device() {
    // 双库双设备：bob 用 Welcome 真正入群，才能在自己的库里退出。
    let dir_a = FixtureDir::new();
    let dir_b = FixtureDir::new();
    let mut store_a = dir_a.open();
    let mut store_b = dir_b.open();
    let mut alice = MlsDevice::new("alice").expect("alice");
    let mut bob = MlsDevice::new("bob").expect("bob");
    let request = CreateGroupRequest::new("退出群", "alice");
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
    let joined = group_join_from_welcome(
        &mut bob,
        &mut store_b,
        "退出群",
        &welcome,
        &acceptance,
        "bob",
        "alice",
    )
    .expect("bob joins");
    let group_id = view_a.group_id.clone();
    assert_eq!(joined.group_id, group_id);
    // 群主必须先转让：直接退出 → PERMISSION_DENIED，状态不变。
    let before = group_snapshot(&store_a, &group_id).expect("snapshot");
    let error = leave_group(&mut alice, &mut store_a, &group_id, "alice")
        .expect_err("the owner must transfer first");
    assert_eq!(error.code(), "PERMISSION_DENIED");
    assert_eq!(group_snapshot(&store_a, &group_id).expect("snapshot"), before);
    assert!(before.left_at_ms.is_none());

    // 普通成员退出（bob 自己的库）：本机从确认成员里移除，记录退出时间。
    let left = leave_group(&mut bob, &mut store_b, &group_id, "bob").expect("member leaves");
    assert_eq!(
        left.members
            .iter()
            .map(|member| member.credential.clone())
            .collect::<Vec<_>>(),
        vec!["alice".to_owned()]
    );
    assert!(left.members.iter().all(|member| !member.is_local));
    assert!(left.left_at_ms.is_some());
    assert_eq!(left.policy_version, before.policy_version + 1);
    assert!(left.pending.is_none(), "leaving is a local policy transition");
    assert_eq!(
        left.pending_removed
            .iter()
            .map(|member| member.credential.clone())
            .collect::<Vec<_>>(),
        vec!["bob".to_owned()],
        "the departed local device is recorded, not silently dropped"
    );

    // 退出后本机不再能治理该群（不是确认成员）。
    let error = set_muted(&mut store_b, &group_id, "bob", "alice", true)
        .expect_err("a departed device is not a member");
    assert_eq!(error.code(), "INVALID_TARGET");
    // 重复退出显式失败。
    let error = leave_group(&mut bob, &mut store_b, &group_id, "bob").expect_err("already left");
    assert_eq!(error.code(), "INVALID_STATE_TRANSITION");
    // 群主那一侧看到的仍是两人（对端还没被协调者移除）。
    assert_eq!(
        group_snapshot(&store_a, &group_id)
            .expect("owner snapshot")
            .members
            .len(),
        2
    );

    // 关库重开：退出状态保留；MLS 侧叶子仍在（epoch 不变），因此协调者仍需移除本机。
    let database = dir_b.database();
    let mls_key = left.mls_group_key.clone();
    drop(store_b);
    let reopened = open_store(
        StoreOptions::new(&database, KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes(MASTER_KEY),
    )
    .expect("reopen store");
    let restored = group_snapshot(&reopened, &group_id).expect("snapshot after reopen");
    assert!(restored.left_at_ms.is_some());
    assert_eq!(restored.members.len(), 1);
    let device = MlsDevice::import_from_store(&reopened, &mls_key).expect("MLS state still there");
    assert_eq!(device.epoch().expect("epoch"), 1, "the MLS leaf is still in the group");
}

#[test]
fn dissolve_blocks_all_governance_and_keeps_the_record() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id();
    let before = fixture.snapshot();

    let dissolved = dissolve_group(&mut fixture.store, &group_id, "alice").expect("dissolve");
    assert!(dissolved.dissolved);
    assert!(dissolved.dissolved_at_ms.is_some());
    assert_eq!(dissolved.policy_version, before.policy_version + 1);
    assert_eq!(dissolved.members.len(), before.members.len(), "records are kept");
    assert!(dissolved.pending.is_none());

    // 所有后续治理都显式拒绝，且状态不变。
    let frozen = fixture.snapshot();
    let attempts: Vec<(&str, String)> = vec![
        (
            "set_role",
            set_role(
                &mut fixture.alice,
                &mut fixture.store,
                &group_id,
                "alice",
                "bob",
                MemberRole::Admin,
            )
            .expect_err("dissolved")
            .code()
            .to_owned(),
        ),
        (
            "set_muted",
            set_muted(&mut fixture.store, &group_id, "alice", "bob", true)
                .expect_err("dissolved")
                .code()
                .to_owned(),
        ),
        (
            "remove_member",
            remove_member(&mut fixture.alice, &mut fixture.store, &group_id, "alice", "bob")
                .expect_err("dissolved")
                .code()
                .to_owned(),
        ),
        (
            "rename_group",
            rename_group(&mut fixture.store, &group_id, "alice", "新名字")
                .expect_err("dissolved")
                .code()
                .to_owned(),
        ),
        (
            "transfer_ownership",
            transfer_ownership(
                &mut fixture.alice,
                &mut fixture.store,
                &group_id,
                "alice",
                "bob",
            )
            .expect_err("dissolved")
            .code()
            .to_owned(),
        ),
        (
            "leave_group",
            leave_group(&mut fixture.bob, &mut fixture.store, &group_id, "bob")
                .expect_err("dissolved")
                .code()
                .to_owned(),
        ),
        (
            "dissolve_again",
            dissolve_group(&mut fixture.store, &group_id, "alice")
                .expect_err("dissolved")
                .code()
                .to_owned(),
        ),
        (
            "accept_pending",
            accept_pending_control(&mut fixture.alice, &mut fixture.store, &group_id)
                .expect_err("dissolved")
                .code()
                .to_owned(),
        ),
        (
            "discard_pending",
            discard_pending_control(&mut fixture.store, &group_id, "node_rejected")
                .expect_err("dissolved")
                .code()
                .to_owned(),
        ),
    ];
    for (operation, code) in &attempts {
        assert_eq!(code, "INVALID_STATE_TRANSITION", "{operation} must be rejected");
    }
    assert_eq!(fixture.snapshot(), frozen);

    // 记录保留：快照仍可读，关库重开一致。
    let database = fixture.dir.database();
    drop(fixture.store);
    let reopened = open_store(
        StoreOptions::new(&database, KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes(MASTER_KEY),
    )
    .expect("reopen store");
    let restored = group_snapshot(&reopened, &group_id).expect("snapshot survives");
    assert!(restored.dissolved);
    assert_eq!(restored, frozen);
}

#[test]
fn dissolve_requires_the_owner_and_no_pending_commit() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id();
    let before = fixture.snapshot();

    // 非群主解散 → PERMISSION_DENIED，状态不变。
    let error = dissolve_group(&mut fixture.store, &group_id, "bob")
        .expect_err("only the owner dissolves");
    assert_eq!(error.code(), "PERMISSION_DENIED");
    assert_eq!(fixture.snapshot(), before);

    // 有未决控制提交时解散 → INVALID_STATE_TRANSITION（先收口）。
    let pending_view = set_role(
        &mut fixture.alice,
        &mut fixture.store,
        &group_id,
        "alice",
        "bob",
        MemberRole::Admin,
    )
    .expect("prepare a role change");
    assert!(pending_view.pending.is_some());
    let error = dissolve_group(&mut fixture.store, &group_id, "alice")
        .expect_err("pending commit blocks dissolve");
    assert_eq!(error.code(), "INVALID_STATE_TRANSITION");
    assert_eq!(fixture.snapshot(), pending_view);

    // 收口之后可以解散。
    let accepted = accept_pending_control(&mut fixture.alice, &mut fixture.store, &group_id)
        .expect("accept the pending commit");
    assert!(accepted.pending.is_none());
    let dissolved = dissolve_group(&mut fixture.store, &group_id, "alice").expect("dissolve now");
    assert!(dissolved.dissolved);
    assert_eq!(dissolved.epoch, 2);
}
