//! GROUPS-013/004 对端条目与 Welcome 加入（真 OpenMLS 双设备 + 两个独立临时库）。
//!
//! 覆盖：A 建群 → B 用 Welcome 加入（成员 2、epoch 1）→ B 处理 A 的 Add Commit（epoch 恰 +1、成员含 carol）→
//! 快照重开一致；重复/乱序/epoch 不符/作者不符/成员计数不符全部显式拒绝且不留半状态；
//! 未知群拒绝；Welcome 绑定不符不写快照、重复加入拒绝。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, GroupMemberKeyPackage, GroupView,
    PeerEntryInput, PeerEntryOutcome, create_group_with_welcome, group_join_from_welcome,
    group_process_peer_entry, group_snapshot,
};
use chat_crypto::control::{CommitExpectation, WelcomeAcceptance};
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
            "chat-core-peer-{}-{nanos}-{counter}",
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

fn credentials(view: &GroupView) -> Vec<String> {
    let mut names: Vec<String> = view
        .members
        .iter()
        .map(|member| member.credential.clone())
        .collect();
    names.sort();
    names
}

/// 双设备夹具：alice 在 A 库建群（追加 carol → pending Add 提交），bob 在 B 库用 Welcome 加入。
struct Fixture {
    dir_b: FixtureDir,
    store_b: Store,
    alice: MlsDevice,
    bob: MlsDevice,
    a_view: GroupView,
    b_view: GroupView,
}

impl Fixture {
    fn new() -> Self {
        let dir_a = FixtureDir::new();
        let dir_b = FixtureDir::new();
        let mut store_a = open(&dir_a);
        let mut store_b = open(&dir_b);
        let mut alice = MlsDevice::new("alice").expect("alice");
        let mut bob = MlsDevice::new("bob").expect("bob");
        let carol = MlsDevice::new("carol").expect("carol");
        let mut request = CreateGroupRequest::new("跨端群", "alice");
        request.members = vec![GroupMemberKeyPackage {
            credential: "carol".to_owned(),
            key_package: carol.key_package().expect("carol key package"),
        }];
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
            "跨端群",
            &welcome,
            &acceptance,
            "bob",
            "alice",
        )
        .expect("bob joins with welcome");
        // A 侧的库与 Welcome 在加入完成后不再需要（数据已提交；alice 设备仍在内存里）。
        drop(store_a);
        drop(dir_a);
        drop(welcome);
        Self {
            dir_b,
            store_b,
            alice,
            bob,
            a_view,
            b_view,
        }
    }

    fn group_id(&self) -> String {
        self.a_view.group_id.clone()
    }

    /// alice 的 pending Add 提交（追加 carol）字节。
    fn pending_commit(&self) -> Vec<u8> {
        self.alice
            .pending_control()
            .expect("pending query")
            .expect("pending add commit")
            .commit
    }

    fn peer_entry(&self) -> PeerEntryInput {
        let mut input = PeerEntryInput::new(
            self.group_id(),
            self.a_view.epoch,
            "alice",
            self.pending_commit(),
        );
        input.expectation = Some(CommitExpectation {
            added: 1,
            removed: 0,
            updated: 0,
        });
        input.added_members = vec!["carol".to_owned()];
        input
    }
}

#[test]
fn welcome_join_then_peer_add_commit_advances_the_epoch() {
    let mut fixture = Fixture::new();

    // 加入侧：真实 Welcome → 成员 2、epoch 1、本机不是群主。
    assert_eq!(fixture.b_view.group_id, fixture.a_view.group_id);
    assert_eq!(fixture.b_view.epoch, 1);
    assert_eq!(credentials(&fixture.b_view), vec!["alice", "bob"]);
    assert_eq!(fixture.b_view.role, MemberRole::Member);
    assert_eq!(fixture.b_view.mls_group_key, fixture.a_view.mls_group_key);
    let owner = fixture
        .b_view
        .members
        .iter()
        .find(|member| member.role == MemberRole::Owner)
        .expect("owner member")
        .credential
        .clone();
    assert_eq!(owner, "alice");
    assert!(fixture.b_view.pending.is_none());

    // 发侧：A 的快照里 carol 仍是 pending（Add 提交尚未被接受）。
    assert_eq!(credentials(&fixture.a_view), vec!["alice", "bob"]);
    assert_eq!(
        fixture
            .a_view
            .pending_members
            .iter()
            .map(|member| member.credential.clone())
            .collect::<Vec<_>>(),
        vec!["carol"]
    );

    // 收侧：B 处理 A 的真实 Add Commit。
    let input = fixture.peer_entry();
    let outcome = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &input)
        .expect("process peer commit");
    let view = match outcome {
        PeerEntryOutcome::Commit {
            old_epoch,
            new_epoch,
            added,
            removed,
            updated,
            self_removed,
            view,
        } => {
            assert_eq!(old_epoch, 1);
            assert_eq!(new_epoch, 2, "epoch advances by exactly one");
            assert_eq!(added, 1);
            assert_eq!(removed, 0);
            assert_eq!(updated, 0);
            assert!(!self_removed);
            view
        }
        other => panic!("expected a merged commit, got {other:?}"),
    };
    assert_eq!(view.epoch, 2);
    assert_eq!(credentials(&view), vec!["alice", "bob", "carol"]);
    assert!(
        view.members
            .iter()
            .find(|member| member.credential == "carol")
            .map(|member| member.role)
            == Some(MemberRole::Member)
    );
    assert_eq!(view.role, MemberRole::Member);

    // 正式库快照与返回值一致，并且重开库后仍然一致。
    let snapshot = group_snapshot(&fixture.store_b, &fixture.group_id()).expect("snapshot");
    assert_eq!(snapshot.epoch, 2);
    assert_eq!(credentials(&snapshot), vec!["alice", "bob", "carol"]);
    let database = fixture.dir_b.database();
    let group_id = fixture.group_id();
    drop(fixture.store_b);
    let reopened = open_store(
        StoreOptions::new(&database, KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes(MASTER_KEY),
    )
    .expect("reopen store b");
    let restored = group_snapshot(&reopened, &group_id).expect("snapshot after reopen");
    assert_eq!(restored.epoch, 2);
    assert_eq!(credentials(&restored), vec!["alice", "bob", "carol"]);
}

#[test]
fn duplicate_and_out_of_order_entries_are_rejected() {
    let mut fixture = Fixture::new();
    let input = fixture.peer_entry();
    group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &input).expect("first merge");
    let after = group_snapshot(&fixture.store_b, &fixture.group_id()).expect("snapshot");
    assert_eq!(after.epoch, 2);

    // 重复处理同一份条目：epoch 落后 → 显式拒绝。
    let error = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &input)
        .expect_err("duplicate entry");
    assert_eq!(error.code(), "STALE_EPOCH");
    assert_eq!(
        group_snapshot(&fixture.store_b, &fixture.group_id()).expect("snapshot"),
        after
    );

    // 乱序（声称 epoch 3）：缺中间条目 → 显式拒绝。
    let mut ahead = input.clone();
    ahead.epoch = 3;
    let error = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &ahead)
        .expect_err("out of order entry");
    assert_eq!(error.code(), "MISSING_LOG_ENTRIES");
    assert_eq!(
        group_snapshot(&fixture.store_b, &fixture.group_id()).expect("snapshot"),
        after
    );

    // epoch 声明与消息内 epoch 不符（payload 仍是 epoch 1 的提交）：显式拒绝。
    let mut mismatched = input.clone();
    mismatched.epoch = 2;
    let error = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &mismatched)
        .expect_err("epoch mismatch");
    assert_eq!(error.code(), "INVALID_CIPHERTEXT");
    assert_eq!(
        group_snapshot(&fixture.store_b, &fixture.group_id()).expect("snapshot"),
        after
    );
}

#[test]
fn wrong_sender_and_declared_member_mismatch_are_rejected() {
    let mut fixture = Fixture::new();
    let before = group_snapshot(&fixture.store_b, &fixture.group_id()).expect("snapshot");
    assert_eq!(before.epoch, 1);

    // 作者不符：外层声明的作者与 MLS 认证作者不一致 → 显式拒绝，库内状态不变。
    let mut wrong_sender = fixture.peer_entry();
    wrong_sender.sender_identity = "mallory".to_owned();
    let error = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &wrong_sender)
        .expect_err("wrong sender");
    assert_eq!(error.code(), "INVALID_CIPHERTEXT");
    assert_eq!(
        group_snapshot(&fixture.store_b, &fixture.group_id()).expect("snapshot"),
        before
    );

    // 声明的成员身份数量与预期计数不符：在合并**之前**就拒绝（不会产生半状态）。
    let mut mismatch = fixture.peer_entry();
    mismatch.added_members.clear();
    let error = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &mismatch)
        .expect_err("declared members do not match");
    assert_eq!(error.code(), "INVALID_CONTROL_RESULT");
    assert_eq!(
        group_snapshot(&fixture.store_b, &fixture.group_id()).expect("snapshot"),
        before
    );
}

#[test]
fn unknown_group_and_empty_payload_are_rejected() {
    let mut fixture = Fixture::new();
    let unknown = "grp:1:0000000000000000000000000000000000000000000000000000000000000007";
    let mut input = PeerEntryInput::new(unknown, 1, "alice", fixture.pending_commit());
    input.expectation = Some(CommitExpectation {
        added: 1,
        removed: 0,
        updated: 0,
    });
    input.added_members = vec!["carol".to_owned()];
    let error = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &input)
        .expect_err("unknown group");
    assert_eq!(error.code(), "INVALID_TARGET");

    let mut empty = fixture.peer_entry();
    empty.payload.clear();
    let error = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &empty)
        .expect_err("empty payload");
    assert_eq!(error.code(), "INVALID_CANDIDATE");
}

#[test]
fn welcome_join_rejects_wrong_binding() {
    // 绑定不符的加入：另起一对设备，避免消耗已用过的 KeyPackage。
    let dir_a = FixtureDir::new();
    let dir_b = FixtureDir::new();
    let mut store_a = open(&dir_a);
    let mut store_b = open(&dir_b);
    let mut alice = MlsDevice::new("alice").expect("alice");
    let mut bob = MlsDevice::new("bob").expect("bob");
    let request = CreateGroupRequest::new("绑定校验群", "alice");
    let peer = GroupBootstrapPeer {
        credential: "bob".to_owned(),
        device: &bob,
    };
    let (a_view, welcome) =
        create_group_with_welcome(&mut alice, &peer, &mut store_a, &request).expect("create");
    let group_id = a_view.group_id.clone();
    let wrong = WelcomeAcceptance {
        group_id: a_view.mls_group_key.clone(),
        expected_epoch: a_view.epoch + 1,
        members: vec!["alice".to_owned(), "bob".to_owned()],
    };
    let error = group_join_from_welcome(
        &mut bob,
        &mut store_b,
        "绑定校验群",
        &welcome,
        &wrong,
        "bob",
        "alice",
    )
    .expect_err("wrong epoch binding");
    assert_eq!(error.code(), "WELCOME_NOT_AUTHORIZED");
    // 没有写入任何快照（不留半加入状态）。
    assert_eq!(
        group_snapshot(&store_b, &group_id).expect_err("no snapshot written").code(),
        "INVALID_TARGET"
    );
    // 被拒绝的 Welcome 已消耗本机 KeyPackage：即使改用正确绑定也必须显式失败（crypto 语义）。
    let mut bob2 = MlsDevice::new("bob").expect("bob2");
    let correct = WelcomeAcceptance {
        group_id: a_view.mls_group_key.clone(),
        expected_epoch: a_view.epoch,
        members: vec!["alice".to_owned(), "bob".to_owned()],
    };
    let error = group_join_from_welcome(
        &mut bob2,
        &mut store_b,
        "绑定校验群",
        &welcome,
        &correct,
        "bob",
        "alice",
    )
    .expect_err("a foreign device cannot use this welcome");
    assert_eq!(error.code(), "WELCOME_NOT_AUTHORIZED");
}

#[test]
fn duplicate_join_is_rejected_and_snapshot_is_unchanged() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id();
    let before = group_snapshot(&fixture.store_b, &group_id).expect("snapshot");
    // bob 已经入群：再次加入（同一设备）由 crypto 的“群已初始化”显式拒绝。
    let acceptance = WelcomeAcceptance {
        group_id: before.mls_group_key.clone(),
        expected_epoch: before.epoch,
        members: vec!["alice".to_owned(), "bob".to_owned()],
    };
    let error = group_join_from_welcome(
        &mut fixture.bob,
        &mut fixture.store_b,
        "跨端群",
        &[],
        &acceptance,
        "bob",
        "alice",
    )
    .expect_err("already joined");
    assert_eq!(error.code(), "INVALID_STATE_TRANSITION");
    assert_eq!(
        group_snapshot(&fixture.store_b, &group_id).expect("snapshot"),
        before
    );
}

#[test]
fn join_rejects_declared_owner_outside_the_group() {
    let dir_a = FixtureDir::new();
    let dir_b = FixtureDir::new();
    let mut store_a = open(&dir_a);
    let mut store_b = open(&dir_b);
    let mut alice = MlsDevice::new("alice").expect("alice");
    let mut bob = MlsDevice::new("bob").expect("bob");
    let request = CreateGroupRequest::new("群主校验群", "alice");
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
    // 声明的群主不在 MLS 成员里 → 显式拒绝（在写快照之前）。
    let error = group_join_from_welcome(
        &mut bob,
        &mut store_b,
        "群主校验群",
        &welcome,
        &acceptance,
        "bob",
        "mallory",
    )
    .expect_err("owner is not a member");
    assert_eq!(error.code(), "INVALID_CONTROL_RESULT");
    assert_eq!(
        group_snapshot(&store_b, &a_view.group_id)
            .expect_err("no snapshot written")
            .code(),
        "INVALID_TARGET"
    );
    // 入群本身已成功（MLS 状态已落库），因此同一设备的第二次加入应显式拒绝。
    let error = group_join_from_welcome(
        &mut bob,
        &mut store_b,
        "群主校验群",
        &welcome,
        &acceptance,
        "bob",
        "alice",
    )
    .expect_err("device already joined the MLS group");
    assert_eq!(error.code(), "INVALID_STATE_TRANSITION");
}
