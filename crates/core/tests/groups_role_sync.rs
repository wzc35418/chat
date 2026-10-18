//! GROUPS-014：策略角色随控制提交同步到对端（真双设备两库）。
//!
//! 覆盖：A 转让给 B → B 处理带策略声明的 Commit 后变 owner、A 变 admin → B 能打包 A 的离开提案
//! （端到端）→ 无声明时角色不变（向后兼容）→ 声明与真实成员变化不一致时拒绝且快照不变。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::group_node::ControlIntent;
use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, GroupView, PeerEntryInput, PeerEntryOutcome,
    ProposalEntryInput, accept_pending_control, create_group_with_welcome, group_commit_proposals,
    group_join_from_welcome, group_prepare_leave, group_process_peer_entry, group_process_proposal,
    group_snapshot, transfer_ownership,
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
            "chat-core-rolesync-{}-{nanos}-{counter}",
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

/// alice 建群（群主）+ bob 用 Welcome 加入（成员）。
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
        let dir_a = FixtureDir::new();
        let dir_b = FixtureDir::new();
        let mut store_a = dir_a.open();
        let mut store_b = dir_b.open();
        let mut alice = MlsDevice::new("alice").expect("alice");
        let mut bob = MlsDevice::new("bob").expect("bob");
        let request = CreateGroupRequest::new("角色同步群", "alice");
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
            "角色同步群",
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

    /// alice 转让群主给 bob：返回 bob 侧要处理的 (提交字节, 策略声明)。
    fn transfer_to_bob(&mut self) -> (Vec<u8>, PeerEntryInput) {
        let pending = transfer_ownership(
            &mut self.alice,
            &mut self.store_a,
            &self.group_id,
            "alice",
            "bob",
        )
        .expect("transfer ownership");
        assert!(pending.pending.is_some());
        let commit = self
            .alice
            .pending_control()
            .expect("pending query")
            .expect("pending commit")
            .commit;
        let accepted = accept_pending_control(&mut self.alice, &mut self.store_a, &self.group_id)
            .expect("accept the transfer");
        assert_eq!(accepted.epoch, 2);
        assert_eq!(accepted.role, MemberRole::Member);
        let mut input = PeerEntryInput::new(self.group_id.clone(), 1, "alice", commit.clone());
        input.policy_intent = Some(ControlIntent::TransferOwnership {
            target_credential: "bob".to_owned(),
        });
        (commit, input)
    }
}

fn role_of(view: &GroupView, credential: &str) -> MemberRole {
    view.members
        .iter()
        .find(|member| member.credential == credential)
        .map(|member| member.role)
        .expect("member present")
}

#[test]
fn transfer_with_policy_intent_syncs_peer_roles_and_unlocks_committing() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id.clone();
    let before_peer = group_snapshot(&fixture.store_b, &group_id).expect("snapshot");
    assert_eq!(role_of(&before_peer, "alice"), MemberRole::Owner);
    assert_eq!(role_of(&before_peer, "bob"), MemberRole::Member);

    // 1) A 转让给 B（带策略声明的对端提交）。
    let (_commit, input) = fixture.transfer_to_bob();
    let outcome = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &input)
        .expect("bob merges the transfer with the declaration");
    let view = match outcome {
        PeerEntryOutcome::Commit { new_epoch, view, .. } => {
            assert_eq!(new_epoch, 2);
            view
        }
        other => panic!("expected a merged commit, got {other:?}"),
    };
    // 2) B 侧角色已同步：bob 变 owner、alice 降为 admin（公开元数据语义），策略版本前进。
    assert_eq!(role_of(&view, "bob"), MemberRole::Owner);
    assert_eq!(role_of(&view, "alice"), MemberRole::Admin);
    assert_eq!(view.role, MemberRole::Owner, "the local device is now the owner");
    assert_eq!(
        view.policy_version,
        before_peer.policy_version + 1,
        "policy version advances exactly once"
    );
    assert_ne!(view.control_head, before_peer.control_head, "control head advances");

    // 3) 端到端解锁：bob（现在是对端记录的群主）能打包 alice 的离开提案。
    let proposal = group_prepare_leave(&mut fixture.alice, &mut fixture.store_a, &group_id)
        .expect("alice prepares self-removal");
    let processed = group_process_proposal(
        &mut fixture.bob,
        &mut fixture.store_b,
        &ProposalEntryInput::new(group_id.clone(), 2, "alice", proposal.proposal.clone()),
    )
    .expect("bob processes the proposal");
    assert_eq!(processed.proposals_pending, 1);
    let committed = group_commit_proposals(&mut fixture.bob, &mut fixture.store_b, &group_id)
        .expect("the new owner can commit another member's proposal");
    let pending = committed.pending.as_ref().expect("pending control commit");
    assert_eq!(pending.operation, "remove_members");
    assert_eq!(pending.base_epoch, 2);
    assert_eq!(pending.target_epoch, 3);

    // 4) alice 合并移除提交后离群。
    let commit = fixture
        .bob
        .pending_control()
        .expect("pending query")
        .expect("pending commit")
        .commit;
    let mut removal = PeerEntryInput::new(group_id.clone(), 2, "bob", commit);
    removal.removed_members = vec!["alice".to_owned()];
    removal.expectation = Some(chat_crypto::control::CommitExpectation {
        added: 0,
        removed: 1,
        updated: 0,
    });
    let outcome = group_process_peer_entry(&mut fixture.alice, &mut fixture.store_a, &removal)
        .expect("alice merges the removal commit");
    match outcome {
        PeerEntryOutcome::Commit {
            self_removed, view, ..
        } => {
            assert!(self_removed);
            assert!(view.members.iter().all(|member| !member.is_local));
        }
        other => panic!("expected a merged commit, got {other:?}"),
    }
    assert!(fixture.alice.encrypt("gone").is_err());
}

#[test]
fn without_a_declaration_peer_roles_stay_unchanged() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id.clone();
    let before = group_snapshot(&fixture.store_b, &group_id).expect("snapshot");

    // 不带声明：只合并提交，不动角色（向后兼容）。
    let (_commit, mut input) = fixture.transfer_to_bob();
    input.policy_intent = None;
    let outcome = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &input)
        .expect("merge without a declaration");
    let view = match outcome {
        PeerEntryOutcome::Commit { view, .. } => view,
        other => panic!("expected a merged commit, got {other:?}"),
    };
    assert_eq!(view.epoch, 2);
    assert_eq!(role_of(&view, "alice"), MemberRole::Owner, "unchanged");
    assert_eq!(role_of(&view, "bob"), MemberRole::Member, "unchanged");
    assert_eq!(view.policy_version, before.policy_version, "unchanged");
    assert_eq!(view.control_head, before.control_head, "unchanged");
    // 正因为角色没同步，bob 不能打包他人提案（旧限制仍在，必须显式声明才能解锁）。
    let proposal = group_prepare_leave(&mut fixture.alice, &mut fixture.store_a, &group_id)
        .expect("alice prepares self-removal");
    group_process_proposal(
        &mut fixture.bob,
        &mut fixture.store_b,
        &ProposalEntryInput::new(group_id.clone(), 2, "alice", proposal.proposal.clone()),
    )
    .expect("process the proposal");
    let error = group_commit_proposals(&mut fixture.bob, &mut fixture.store_b, &group_id)
        .expect_err("a member cannot commit another member's proposal");
    assert_eq!(error.code(), "PERMISSION_DENIED");
}

#[test]
fn inconsistent_declarations_are_rejected_without_changing_the_snapshot() {
    // 每个子用例都用**全新夹具**（转让只能发起一次：之后本机已不是群主）。
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id.clone();
    let before = group_snapshot(&fixture.store_b, &group_id).expect("snapshot");

    // 声明是转让但提交同时增删成员（声明与真实形态不一致）。
    let (_commit, mut input) = fixture.transfer_to_bob();
    input.added_members = vec!["carol".to_owned()];
    input.expectation = Some(chat_crypto::control::CommitExpectation {
        added: 1,
        removed: 0,
        updated: 0,
    });
    let error = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &input)
        .expect_err("declaration does not match the real commit shape");
    assert!(!error.code().is_empty());
    assert_eq!(
        group_snapshot(&fixture.store_b, &group_id).expect("snapshot"),
        before
    );

    // 声明 set_role 但目标不是确认成员 → 显式拒绝且快照不变（新夹具）。
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id.clone();
    let before = group_snapshot(&fixture.store_b, &group_id).expect("snapshot");
    let (_commit, mut input) = fixture.transfer_to_bob();
    input.policy_intent = Some(ControlIntent::SetRole {
        target_credential: Some("carol".to_owned()),
    });
    input.policy_role = Some(MemberRole::Admin);
    let error = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &input)
        .expect_err("unknown set_role target");
    assert_eq!(error.code(), "INVALID_TARGET");
    assert_eq!(
        group_snapshot(&fixture.store_b, &group_id).expect("snapshot"),
        before
    );

    // 声明 set_role 却没给角色 → INVALID_INPUT；同样不改快照（新夹具）。
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id.clone();
    let before = group_snapshot(&fixture.store_b, &group_id).expect("snapshot");
    let (_commit, mut input) = fixture.transfer_to_bob();
    input.policy_intent = Some(ControlIntent::SetRole {
        target_credential: Some("bob".to_owned()),
    });
    input.policy_role = None;
    let error = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &input)
        .expect_err("missing role");
    assert_eq!(error.code(), "INVALID_INPUT");
    assert_eq!(
        group_snapshot(&fixture.store_b, &group_id).expect("snapshot"),
        before
    );
    let _ = (&fixture.view_a, &fixture.view_b);
}

#[test]
fn set_role_declaration_syncs_the_target_role() {
    let mut fixture = Fixture::new();
    let group_id = fixture.group_id.clone();
    let before = group_snapshot(&fixture.store_b, &group_id).expect("snapshot");
    // alice（群主）把 bob 设为管理员。
    let pending = chat_core::groups::set_role(
        &mut fixture.alice,
        &mut fixture.store_a,
        &group_id,
        "alice",
        "bob",
        MemberRole::Admin,
    )
    .expect("set role");
    assert!(pending.pending.is_some());
    let commit = fixture
        .alice
        .pending_control()
        .expect("pending query")
        .expect("pending commit")
        .commit;
    accept_pending_control(&mut fixture.alice, &mut fixture.store_a, &group_id)
        .expect("accept the role change");
    let mut input = PeerEntryInput::new(group_id.clone(), 1, "alice", commit);
    input.policy_intent = Some(ControlIntent::SetRole {
        target_credential: Some("bob".to_owned()),
    });
    input.policy_role = Some(MemberRole::Admin);
    let outcome = group_process_peer_entry(&mut fixture.bob, &mut fixture.store_b, &input)
        .expect("bob merges the role change");
    let view = match outcome {
        PeerEntryOutcome::Commit { view, .. } => view,
        other => panic!("expected a merged commit, got {other:?}"),
    };
    assert_eq!(role_of(&view, "bob"), MemberRole::Admin);
    assert_eq!(role_of(&view, "alice"), MemberRole::Owner, "owner stays owner");
    assert_eq!(view.policy_version, before.policy_version + 1);
}
