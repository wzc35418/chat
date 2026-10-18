//! GROUPS-013 自移除提案编排的 FFI 用例（真双设备 + 两个独立临时库，全程经 FFI 调三个新方法）。
//!
//! 覆盖：成员发起自移除提案（返回可转发字节、epoch/策略版本不变、无 pending）→ 重复发起被拒 →
//! 对端处理提案（epoch 不变）→ 对端打包 pending 提交（epoch 不推进）；群主发起被拒；乱序 epoch 被拒。
//! 端到端的“合并后 self_removed”由 core 侧 groups_proposal 用例覆盖（同一真实链路）。

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, PeerEntryInput, accept_pending_control,
    create_group_with_welcome, group_join_from_welcome, group_process_peer_entry,
    transfer_ownership,
};
use chat_crypto::control::WelcomeAcceptance;
use chat_crypto::engine::MlsDevice;
use chat_ffi::groups_ffi::{GroupFfi, GroupViewFfi};
use chat_ffi::local_store::FfiError;
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
            "chat-ffi-proposal-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> PathBuf {
        self.0.join("chat.db")
    }

    fn database_text(&self) -> String {
        self.database().to_string_lossy().into_owned()
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

fn master_key_hex() -> String {
    MASTER_KEY.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn code(error: FfiError) -> String {
    match error {
        FfiError::Store { code, .. } => code,
    }
}

fn member_credentials(view: &GroupViewFfi) -> Vec<String> {
    view.members
        .iter()
        .map(|member| member.credential.clone())
        .collect()
}

struct Harness {
    _dir_a: FixtureDir,
    _dir_b: FixtureDir,
    alice: Arc<GroupFfi>,
    bob: Arc<GroupFfi>,
    group_id: String,
}

impl Harness {
    /// transfer 为真时先把群主转让给 bob（两侧都到 epoch 2），否则 alice 仍是群主。
    fn new(transfer: bool) -> Self {
        let dir_a = FixtureDir::new();
        let dir_b = FixtureDir::new();
        let group_id = {
            let mut store_a = dir_a.open();
            let mut store_b = dir_b.open();
            let mut alice = MlsDevice::new("alice").expect("alice");
            let mut bob = MlsDevice::new("bob").expect("bob");
            let request = CreateGroupRequest::new("FFI 提案群", "alice");
            let peer = GroupBootstrapPeer {
                credential: "bob".to_owned(),
                device: &bob,
            };
            let (view_a, welcome) =
                create_group_with_welcome(&mut alice, &peer, &mut store_a, &request)
                    .expect("create group");
            let acceptance = WelcomeAcceptance {
                group_id: view_a.mls_group_key.clone(),
                expected_epoch: view_a.epoch,
                members: vec!["alice".to_owned(), "bob".to_owned()],
            };
            group_join_from_welcome(
                &mut bob,
                &mut store_b,
                "FFI 提案群",
                &welcome,
                &acceptance,
                "bob",
                "alice",
            )
            .expect("bob joins");
            let group_id = view_a.group_id.clone();
            if transfer {
                let pending =
                    transfer_ownership(&mut alice, &mut store_a, &group_id, "alice", "bob")
                        .expect("transfer ownership");
                assert!(pending.pending.is_some());
                let commit = alice
                    .pending_control()
                    .expect("pending query")
                    .expect("pending commit")
                    .commit;
                accept_pending_control(&mut alice, &mut store_a, &group_id)
                    .expect("accept the transfer");
                let input = PeerEntryInput::new(group_id.clone(), 1, "alice", commit);
                group_process_peer_entry(&mut bob, &mut store_b, &input)
                    .expect("bob merges the transfer");
            }
            group_id
        };
        let alice = GroupFfi::open(dir_a.database_text(), master_key_hex(), None)
            .expect("open alice group ffi");
        let bob = GroupFfi::open(dir_b.database_text(), master_key_hex(), None)
            .expect("open bob group ffi");
        Self {
            _dir_a: dir_a,
            _dir_b: dir_b,
            alice,
            bob,
            group_id,
        }
    }

    fn snapshot(&self) -> GroupViewFfi {
        self.alice
            .group_snapshot(self.group_id.clone())
            .expect("snapshot")
    }
}

#[test]
fn owner_prepare_leave_is_rejected_through_ffi() {
    let harness = Harness::new(false);
    let before = harness.snapshot();
    assert_eq!(before.role, "owner");
    let error = harness
        .alice
        .group_prepare_leave(harness.group_id.clone())
        .expect_err("the owner must transfer ownership first");
    assert_eq!(code(error), "PERMISSION_DENIED");
    assert_eq!(harness.snapshot(), before);
}

#[test]
fn member_prepare_leave_process_and_commit_through_ffi() {
    let harness = Harness::new(true);
    let group_id = harness.group_id.clone();
    let before = harness.snapshot();
    assert_eq!(before.epoch, 2);
    assert_eq!(before.role, "member", "alice transferred ownership to bob");

    // 1) 成员发起自移除提案：返回可转发字节，epoch/策略版本不变，无 pending 控制提交。
    let proposal = harness
        .alice
        .group_prepare_leave(group_id.clone())
        .expect("prepare self-removal");
    assert_eq!(proposal.base_epoch, 2);
    assert_eq!(proposal.epoch, 2);
    assert_eq!(proposal.removed, 1);
    assert!(!proposal.has_welcome);
    assert_eq!(proposal.proposal_digest.len(), 64);
    assert!(!proposal.proposal.is_empty());
    assert!(proposal.leaving_at_ms > 0);
    let after_prepare = harness.snapshot();
    assert_eq!(after_prepare.epoch, 2);
    assert_eq!(after_prepare.policy_version, before.policy_version);
    assert!(after_prepare.pending.is_none());
    assert!(after_prepare.leaving_at_ms.is_some());
    assert_eq!(member_credentials(&after_prepare), vec!["alice", "bob"]);

    // 2) 重复发起 → 显式拒绝。
    let error = harness
        .alice
        .group_prepare_leave(group_id.clone())
        .expect_err("duplicate prepare");
    assert_eq!(code(error), "INVALID_STATE_TRANSITION");

    // 3) 对端处理提案：epoch 不变、提案入本地 store。
    let outcome = harness
        .bob
        .group_process_proposal(
            group_id.clone(),
            2,
            "alice".to_owned(),
            proposal.proposal.clone(),
        )
        .expect("process proposal");
    assert_eq!(outcome.kind, "remove");
    assert_eq!(outcome.by, "alice");
    assert_eq!(outcome.epoch, 2);
    assert_eq!(outcome.targets, vec!["alice".to_owned()]);
    assert_eq!(outcome.proposals_pending, 1);

    // 4) 对端打包成 pending 控制提交：epoch 不推进、成员不变、快照如实报告 pending。
    let committed = harness
        .bob
        .group_commit_proposals(group_id.clone())
        .expect("commit proposals");
    assert_eq!(committed.epoch, 2);
    assert_eq!(member_credentials(&committed), vec!["alice", "bob"]);
    let pending = committed.pending.as_ref().expect("pending control commit");
    assert_eq!(pending.operation, "remove_members");
    assert_eq!(pending.base_epoch, 2);
    assert_eq!(pending.target_epoch, 3);
    assert_eq!(pending.removed, 1);

    // 5) 已有 pending 提交时再次打包 → 显式拒绝。
    let error = harness
        .bob
        .group_commit_proposals(group_id.clone())
        .expect_err("a control commit is already pending");
    assert!(!code(error).is_empty());
}

#[test]
fn process_proposal_rejects_wrong_epoch_through_ffi() {
    let harness = Harness::new(false);
    let error = harness
        .bob
        .group_process_proposal(harness.group_id.clone(), 5, "alice".to_owned(), vec![1, 2, 3])
        .expect_err("future epoch");
    assert_eq!(code(error), "MISSING_LOG_ENTRIES");
    let error = harness
        .bob
        .group_process_proposal(harness.group_id.clone(), 0, "alice".to_owned(), vec![1, 2, 3])
        .expect_err("stale epoch");
    assert_eq!(code(error), "STALE_EPOCH");
    // 空 payload 显式拒绝。
    let error = harness
        .bob
        .group_process_proposal(harness.group_id.clone(), 1, "alice".to_owned(), Vec::new())
        .expect_err("empty payload");
    assert_eq!(code(error), "INVALID_CANDIDATE");
}
