//! GROUPS-013：自移除提案编排（真双设备 + 两个独立临时库）。
//!
//! 覆盖：群主拒绝发起自移除 → 转让后普通成员发起 → 对端处理提案（epoch 不变）→ 对端打包成
//! pending 提交（epoch 不推进）→ 本机合并后 self_removed 且无法加解密；重复发起/乱序/作者不符拒绝；
//! 已解散的群全部拒绝。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, GroupView, PeerEntryInput, PeerEntryOutcome,
    ProposalEntryInput, create_group_with_welcome, dissolve_group, group_commit_proposals,
    group_join_from_welcome, group_list, group_prepare_leave, group_process_peer_entry,
    group_process_proposal,
    group_snapshot,
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
            "chat-core-proposal-{}-{nanos}-{counter}",
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

/// 双设备双库：alice 建群（bob 为引导成员），bob 用 Welcome 真正入群。
struct Fixture {
    dir_a: FixtureDir,
    dir_b: FixtureDir,
    store_a: Store,
    store_b: Store,
    alice: MlsDevice,
    bob: MlsDevice,
    group_id: String,
    mls_key: String,
    view_a: GroupView,
    view_b: GroupView,
}

impl Fixture {
    fn new() -> Self {
        let dir_a = FixtureDir::new();
        let dir_b = FixtureDir::new();
        let mut store_a = dir_a.open();
        let mut store_b = dir_b.open();
        let mut alice = MlsDevice::new("alice").expect("alice");
        let mut bob = MlsDevice::new("bob").expect("bob");
        let request = CreateGroupRequest::new("提案群", "alice");
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
            "提案群",
            &welcome,
            &acceptance,
            "bob",
            "alice",
        )
        .expect("bob joins");
        assert_eq!(view_b.group_id, view_a.group_id);
        Self {
            dir_a,
            dir_b,
            store_a,
            store_b,
            alice,
            bob,
            group_id: view_a.group_id.clone(),
            mls_key: view_a.mls_group_key.clone(),
            view_a,
            view_b,
        }
    }

    /// bob 是建群者（群主），alice 是 Welcome 加入的成员——两侧记录里的角色都准确。
    fn assert_roles(&self) {
        assert_eq!(self.view_b.role, MemberRole::Owner);
        assert_eq!(self.view_a.role, MemberRole::Member);
    }
}

/// bob 建群（群主）+ alice 用 Welcome 加入（成员）：提交侧权限用例需要 B 侧角色准确。
fn bob_owned_fixture() -> Fixture {
    let dir_a = FixtureDir::new();
    let dir_b = FixtureDir::new();
    let mut store_a = dir_a.open();
    let mut store_b = dir_b.open();
    let mut alice = MlsDevice::new("alice").expect("alice");
    let mut bob = MlsDevice::new("bob").expect("bob");
    let request = CreateGroupRequest::new("提案群（bob 建）", "bob");
    let peer = GroupBootstrapPeer {
        credential: "alice".to_owned(),
        device: &alice,
    };
    let (view_b, welcome) =
        create_group_with_welcome(&mut bob, &peer, &mut store_b, &request).expect("create");
    let acceptance = WelcomeAcceptance {
        group_id: view_b.mls_group_key.clone(),
        expected_epoch: view_b.epoch,
        members: vec!["alice".to_owned(), "bob".to_owned()],
    };
    let view_a = group_join_from_welcome(
        &mut alice,
        &mut store_a,
        "提案群（bob 建）",
        &welcome,
        &acceptance,
        "alice",
        "bob",
    )
    .expect("alice joins");
    Fixture {
        dir_a,
        dir_b,
        store_a,
        store_b,
        alice,
        bob,
        group_id: view_b.group_id.clone(),
        mls_key: view_b.mls_group_key.clone(),
        view_a,
        view_b,
    }
}

#[test]
fn owner_cannot_prepare_leave() {
    let mut fixture = Fixture::new();
    let before = group_snapshot(&fixture.store_a, &fixture.group_id).expect("snapshot");
    let error = group_prepare_leave(&mut fixture.alice, &mut fixture.store_a, &fixture.group_id)
        .expect_err("the owner must transfer ownership first");
    assert_eq!(error.code(), "PERMISSION_DENIED");
    let after = group_snapshot(&fixture.store_a, &fixture.group_id).expect("snapshot");
    assert_eq!(after, before);
    assert!(after.leaving_at_ms.is_none());
}

#[test]
fn self_removal_proposal_end_to_end() {
    // bob 建群（群主）+ alice 加入（成员）：提交侧权限要求打包者是群主/管理员。
    let mut fixture = bob_owned_fixture();
    fixture.assert_roles();
    let group_id = fixture.group_id.clone();

    // 1) 普通成员 alice 发起自移除提案：epoch 与策略版本都不变，无 pending 控制提交。
    let before = group_snapshot(&fixture.store_a, &group_id).expect("snapshot");
    let proposal = group_prepare_leave(&mut fixture.alice, &mut fixture.store_a, &group_id)
        .expect("prepare self-removal");
    assert_eq!(proposal.base_epoch, 1);
    assert_eq!(proposal.epoch, 1);
    assert_eq!(proposal.removed, 1);
    assert!(!proposal.has_welcome);
    assert_eq!(proposal.proposal_digest.len(), 64);
    assert!(!proposal.proposal.is_empty());
    let after = group_snapshot(&fixture.store_a, &group_id).expect("snapshot");
    assert_eq!(after.epoch, 1, "preparing a proposal does not advance the epoch");
    assert_eq!(after.policy_version, before.policy_version);
    assert!(after.pending.is_none(), "no pending control commit is created");
    assert!(after.leaving_at_ms.is_some());
    assert_eq!(after.members.len(), 2, "the device is still a member until committed");

    // 2) 对端（bob，此时是群主）处理提案：epoch 不变、提案入本地 store。
    let outcome = group_process_proposal(
        &mut fixture.bob,
        &mut fixture.store_b,
        &ProposalEntryInput::new(group_id.clone(), 1, "alice", proposal.proposal.clone()),
    )
    .expect("process the proposal");
    assert_eq!(outcome.kind, "remove");
    assert_eq!(outcome.by, "alice");
    assert_eq!(outcome.epoch, 1);
    assert_eq!(outcome.targets, vec!["alice".to_owned()]);
    assert_eq!(outcome.proposals_pending, 1);
    assert_eq!(outcome.view.epoch, 1);

    // 3) 对端把提案打包成 pending 控制提交：epoch 不推进，快照如实报告 pending。
    let committed = group_commit_proposals(&mut fixture.bob, &mut fixture.store_b, &group_id)
        .expect("commit pending proposals");
    assert_eq!(committed.epoch, 1, "committing does not advance the epoch");
    assert_eq!(committed.members.len(), 2, "members change only after merge");
    let pending = committed.pending.as_ref().expect("pending control commit");
    assert_eq!(pending.operation, "remove_members");
    assert_eq!(pending.base_epoch, 1);
    assert_eq!(pending.target_epoch, 2);
    assert_eq!(pending.removed, 1);

    // 4) 本机合并对端的提交：self_removed，epoch 前进，本机不再是成员。
    let commit = fixture
        .bob
        .pending_control()
        .expect("pending query")
        .expect("pending commit")
        .commit;
    let mut input = PeerEntryInput::new(group_id.clone(), 1, "bob", commit);
    input.expectation = Some(CommitExpectation {
        added: 0,
        removed: 1,
        updated: 0,
    });
    input.removed_members = vec!["alice".to_owned()];
    let outcome = group_process_peer_entry(&mut fixture.alice, &mut fixture.store_a, &input)
        .expect("merge the removal commit");
    let view = match outcome {
        PeerEntryOutcome::Commit {
            old_epoch,
            new_epoch,
            removed,
            self_removed,
            view,
            ..
        } => {
            assert_eq!(old_epoch, 1);
            assert_eq!(new_epoch, 2);
            assert_eq!(removed, 1);
            assert!(self_removed, "this commit removes the local device");
            view
        }
        other => panic!("expected a merged commit, got {other:?}"),
    };
    assert_eq!(view.epoch, 2);
    assert_eq!(
        view.members
            .iter()
            .map(|member| member.credential.clone())
            .collect::<Vec<_>>(),
        vec!["bob".to_owned()]
    );
    assert!(view.members.iter().all(|member| !member.is_local));

    // 5) 真正离群后无法再加解密；对端仍然正常。
    assert!(
        fixture.alice.encrypt("after removal").is_err(),
        "a removed device must not be able to encrypt"
    );
    // bob 自己还有 pending 提交（它打包的那条）：本地按 crypto 语义先落 pending，因此不能加密，
    // 但它的群状态是健康的（epoch 仍 1、pending 仍在）。
    assert!(
        fixture.bob.encrypt("still here").is_err(),
        "a device with a pending commit cannot encrypt before merging it"
    );
    assert!(
        fixture
            .bob
            .pending_control()
            .expect("pending query")
            .is_some()
    );
    let _ = (&fixture.dir_a, &fixture.dir_b, &fixture.mls_key, fixture.view_a.clone(), fixture.view_b.clone());
}

#[test]
fn duplicate_and_invalid_proposals_are_rejected() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id.clone();

    // 成员（bob）发起自移除提案成功；再发一次显式拒绝。
    let proposal = group_prepare_leave(&mut fixture.bob, &mut fixture.store_b, &group_id)
        .expect("prepare self-removal");
    let error = group_prepare_leave(&mut fixture.bob, &mut fixture.store_b, &group_id)
        .expect_err("duplicate prepare");
    assert_eq!(error.code(), "INVALID_STATE_TRANSITION");

    // 乱序 epoch：超前 / 落后都被拒（epoch 检查在解密之前，不消耗消息）。
    let before = group_snapshot(&fixture.store_a, &group_id).expect("snapshot");
    let error = group_process_proposal(
        &mut fixture.alice,
        &mut fixture.store_a,
        &ProposalEntryInput::new(group_id.clone(), 5, "bob", proposal.proposal.clone()),
    )
    .expect_err("future epoch");
    assert_eq!(error.code(), "MISSING_LOG_ENTRIES");
    let error = group_process_proposal(
        &mut fixture.alice,
        &mut fixture.store_a,
        &ProposalEntryInput::new(group_id.clone(), 0, "bob", proposal.proposal.clone()),
    )
    .expect_err("stale epoch");
    assert_eq!(error.code(), "STALE_EPOCH");
    assert_eq!(
        group_snapshot(&fixture.store_a, &group_id).expect("snapshot"),
        before
    );

    // 作者不符：外层声明的作者与 MLS 认证作者不一致。
    let error = group_process_proposal(
        &mut fixture.alice,
        &mut fixture.store_a,
        &ProposalEntryInput::new(group_id.clone(), 1, "mallory", proposal.proposal.clone()),
    )
    .expect_err("wrong sender");
    assert_eq!(error.code(), "INVALID_CIPHERTEXT");

    // 没有待提交提案时打包 → NO_PENDING_PROPOSALS；未知群 → INVALID_TARGET。
    let error = group_commit_proposals(&mut fixture.alice, &mut fixture.store_a, &group_id)
        .expect_err("no pending proposals");
    assert_eq!(error.code(), "NO_PENDING_PROPOSALS");
    let error = group_process_proposal(
        &mut fixture.alice,
        &mut fixture.store_a,
        &ProposalEntryInput::new(
            "grp:1:0000000000000000000000000000000000000000000000000000000000000009",
            1,
            "bob",
            proposal.proposal.clone(),
        ),
    )
    .expect_err("unknown group");
    assert_eq!(error.code(), "INVALID_TARGET");
}

#[test]
fn dissolved_group_rejects_the_proposal_flow() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id.clone();
    dissolve_group(&mut fixture.store_a, &group_id, "alice").expect("dissolve");
    let frozen = group_snapshot(&fixture.store_a, &group_id).expect("snapshot");

    let error = group_prepare_leave(&mut fixture.alice, &mut fixture.store_a, &group_id)
        .expect_err("dissolved");
    assert_eq!(error.code(), "INVALID_STATE_TRANSITION");
    let error = group_process_proposal(
        &mut fixture.alice,
        &mut fixture.store_a,
        &ProposalEntryInput::new(group_id.clone(), 1, "bob", vec![1, 2, 3]),
    )
    .expect_err("dissolved");
    assert_eq!(error.code(), "INVALID_STATE_TRANSITION");
    let error = group_commit_proposals(&mut fixture.alice, &mut fixture.store_a, &group_id)
        .expect_err("dissolved");
    assert_eq!(error.code(), "INVALID_STATE_TRANSITION");
    assert_eq!(
        group_snapshot(&fixture.store_a, &group_id).expect("snapshot"),
        frozen
    );
}

/// 列表摘要：pending（待收口线索）与 pending_proposal（已收下待打包的提案）可区分。
#[test]
fn group_list_separates_pending_proposal_from_the_leaving_hint() {
    let mut fixture = bob_owned_fixture();
    let group_id = fixture.group_id.clone();
    // 初始：两者都是 false。
    let list = group_list(&fixture.store_a).expect("list a");
    let summary = by_group(&list, &group_id);
    assert!(!summary.pending_proposal);
    assert!(!summary.pending);

    // alice 发起自移除提案：本机只是「待收口线索」（leaving），没有已收下的提案。
    let proposal = group_prepare_leave(&mut fixture.alice, &mut fixture.store_a, &group_id)
        .expect("prepare self-removal");
    let list = group_list(&fixture.store_a).expect("list a after prepare");
    let summary = by_group(&list, &group_id);
    assert!(summary.pending, "the local leaving hint is pending");
    assert!(
        !summary.pending_proposal,
        "no proposal has been received for committing yet"
    );

    // bob（群主）收下提案：他那边 pending_proposal = true（有待打包的提案）。
    group_process_proposal(
        &mut fixture.bob,
        &mut fixture.store_b,
        &ProposalEntryInput::new(group_id.clone(), 1, "alice", proposal.proposal.clone()),
    )
    .expect("bob processes the proposal");
    let list = group_list(&fixture.store_b).expect("list b");
    let summary = by_group(&list, &group_id);
    assert!(
        summary.pending_proposal,
        "a received proposal awaiting commit is reported"
    );
    assert!(summary.pending, "pending is the union of the two hints");
}

/// 按群 ID 取摘要（列表顺序由 storage 决定）。
fn by_group(
    list: &chat_core::groups::GroupListView,
    group_id: &str,
) -> chat_core::groups::GroupSummaryView {
    list.groups
        .iter()
        .find(|summary| summary.group_id == group_id)
        .cloned()
        .expect("summary present")
}

