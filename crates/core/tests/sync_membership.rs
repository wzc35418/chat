//! task-91：控制提交携带成员凭据后，同步 apply 能把 invite/remove 真正写回成员集合。
//!
//! 覆盖：带 added_credentials 的 invite 提交不再计 unsupported，成员集合出现新成员、ApplyReport
//! 记录凭据；随后 remove 提交携带 removed_credentials，成员集合移除该成员、报告记录移除凭据。
//! 不连接真实节点：直接构造已通过外层校验的 LogEntry 调 apply_verified_page（该函数本身不重复验证，
//! 由上游 verify_pull_page 负责），只验证“凭据 → MLS 成员变化”的应用语义。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, GroupMemberKeyPackage, GroupView,
    accept_pending_control, create_group_with_welcome, group_join_from_welcome, group_snapshot,
    remove_member,
};
use chat_core::sync_worker::apply_verified_page;
use chat_crypto::control::WelcomeAcceptance;
use chat_crypto::engine::MlsDevice;
use chat_node_services::canonical::hex_encode;
use chat_node_services::group::{
    AppendResult, CandidateControlMetadata, GroupCandidate, KIND_CONTROL, LogEntry, SignedCandidate,
};
use chat_protocol::DeviceId;
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store};

const NOW_MS: i64 = 1_700_000_000_000;
const MASTER_KEY: [u8; 32] = [0x5a; 32];
const ALICE_SEED: u8 = 42;
const BOB_SEED: u8 = 43;
const CAROL_SEED: u8 = 44;

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
            "chat-core-sync-membership-{}-{nanos}-{counter}",
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

fn credential(seed: u8) -> String {
    DeviceId::from_bytes([seed; 32]).encode()
}

fn log_entry(candidate: GroupCandidate, group_id: &str, seq: u64) -> LogEntry {
    LogEntry {
        candidate: SignedCandidate {
            candidate,
            author_signature: "00".repeat(64),
        },
        receipt: AppendResult {
            group_id: group_id.to_owned(),
            submission_id: "sub:1:0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
            author_device_id: credential(ALICE_SEED),
            seq,
            candidate_hash: "00".repeat(32),
            previous_hash: "00".repeat(32),
            log_hash: "00".repeat(32),
            accepted_at: 0,
            node_id: "node-test".to_owned(),
            node_public_key: "00".repeat(32),
            signature: "00".repeat(64),
        },
        domain_operation: None,
        domain_target_digest: None,
    }
}

fn control_candidate(
    group_id: &str,
    epoch: u64,
    commit: &[u8],
    operation: &str,
    added_credentials: Vec<String>,
    removed_credentials: Vec<String>,
) -> GroupCandidate {
    GroupCandidate {
        protocol_major: 1,
        protocol_minor: 0,
        submission_id: "sub:1:1111111111111111111111111111111111111111111111111111111111111111".to_owned(),
        group_id: group_id.to_owned(),
        author_device_id: credential(ALICE_SEED),
        kind: KIND_CONTROL.to_owned(),
        expected_control_head: "00".repeat(32),
        policy_version: 1,
        mls_epoch: epoch,
        encrypted_payload: hex_encode(commit),
        control_metadata: Some(CandidateControlMetadata {
            operation: operation.to_owned(),
            target_digest: "22".repeat(32),
            added_credentials,
            removed_credentials,
        }),
    }
}

fn members(view: &GroupView) -> Vec<String> {
    let mut values: Vec<String> = view.members.iter().map(|member| member.credential.clone()).collect();
    values.sort();
    values
}

#[test]
fn invite_and_remove_commits_with_credentials_change_membership() {
    let dir_a = FixtureDir::new();
    let dir_b = FixtureDir::new();
    let mut store_a = dir_a.open();
    let mut store_b = dir_b.open();
    let mut alice = MlsDevice::new(&credential(ALICE_SEED)).expect("alice device");
    let mut bob = MlsDevice::new(&credential(BOB_SEED)).expect("bob device");
    let carol = MlsDevice::new(&credential(CAROL_SEED)).expect("carol device");

    // alice 建群：bob 走 Welcome，carol 走 Add（pending，等 bob 侧 apply 后正式入群）。
    let mut request = CreateGroupRequest::new("凭据成员群", credential(ALICE_SEED));
    request.members = vec![GroupMemberKeyPackage {
        credential: credential(CAROL_SEED),
        key_package: carol.key_package().expect("carol key package"),
    }];
    let peer = GroupBootstrapPeer {
        credential: credential(BOB_SEED),
        device: &bob,
    };
    let (a_view, welcome) =
        create_group_with_welcome(&mut alice, &peer, &mut store_a, &request).expect("create group");
    let acceptance = WelcomeAcceptance {
        group_id: a_view.mls_group_key.clone(),
        expected_epoch: a_view.epoch,
        members: vec![credential(ALICE_SEED), credential(BOB_SEED)],
    };
    let b_view = group_join_from_welcome(
        &mut bob,
        &mut store_b,
        "凭据成员群",
        &welcome,
        &acceptance,
        &credential(BOB_SEED),
        &credential(ALICE_SEED),
    )
    .expect("bob joins");
    let group_id = b_view.group_id.clone();
    assert_eq!(b_view.epoch, 1);
    assert_eq!(members(&b_view), vec![credential(ALICE_SEED), credential(BOB_SEED)]);

    // invite：候选携带 carol 凭据 → bob 侧 apply 后 carol 成为正式成员。
    let pending_add = alice
        .pending_control()
        .expect("pending query")
        .expect("pending add commit");
    let invite = control_candidate(
        &group_id,
        1,
        &pending_add.commit,
        "invite",
        vec![credential(CAROL_SEED)],
        Vec::new(),
    );
    let report = apply_verified_page(
        &mut store_b,
        &group_id,
        &[log_entry(invite, &group_id, 1)],
        NOW_MS,
    )
    .expect("invite applies");
    assert_eq!(report.applied, 1, "invite with credentials must apply");
    assert_eq!(report.unsupported, 0);
    assert_eq!(report.added_credentials, vec![credential(CAROL_SEED)]);
    assert!(report.removed_credentials.is_empty());
    assert_eq!(report.pending_controls, 0);
    let after_invite = group_snapshot(&store_b, &group_id).expect("snapshot");
    assert_eq!(after_invite.epoch, 2);
    assert_eq!(
        members(&after_invite),
        vec![
            credential(ALICE_SEED),
            credential(BOB_SEED),
            credential(CAROL_SEED)
        ]
    );

    // alice 接受自己的 pending，使本地 epoch 与 bob 对齐后移除 carol。
    let accepted = accept_pending_control(&mut alice, &mut store_a, &group_id).expect("accept add");
    assert_eq!(accepted.epoch, 2);
    let removed_view = remove_member(
        &mut alice,
        &mut store_a,
        &group_id,
        &credential(ALICE_SEED),
        &credential(CAROL_SEED),
    )
    .expect("remove carol");
    assert_eq!(removed_view.epoch, 2, "pending remove does not advance epoch");
    let pending_remove = alice
        .pending_control()
        .expect("pending query")
        .expect("pending remove commit");
    let removal = control_candidate(
        &group_id,
        2,
        &pending_remove.commit,
        "remove",
        Vec::new(),
        vec![credential(CAROL_SEED)],
    );
    let report = apply_verified_page(
        &mut store_b,
        &group_id,
        &[log_entry(removal, &group_id, 2)],
        NOW_MS,
    )
    .expect("removal applies");
    assert_eq!(report.applied, 1, "remove with credentials must apply");
    assert_eq!(report.unsupported, 0);
    assert!(report.added_credentials.is_empty());
    assert_eq!(report.removed_credentials, vec![credential(CAROL_SEED)]);
    let after_remove = group_snapshot(&store_b, &group_id).expect("snapshot");
    assert_eq!(after_remove.epoch, 3);
    assert_eq!(
        members(&after_remove),
        vec![credential(ALICE_SEED), credential(BOB_SEED)]
    );
}
