//! GROUPS-017：接受入群邀请（真双设备两库）。
//!
//! 覆盖：正常接受（role=member、epoch/成员/群名与邀请一致、凭据目录写入）；元数据与 Welcome 不一致
//! （epoch / 成员 / 群 ID）→ STATE_CORRUPTED 且零写入；重复接受 → INVALID_STATE_TRANSITION；
//! 未知群格式 → INVALID_ID。
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, GroupInviteMeta, accept_group_invite,
    create_group_with_welcome, group_snapshot,
};
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
            "chat-core-invite-{tag}-{}-{nanos}-{counter}",
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

/// 邀请方：alice 建群并给出 Welcome + 邀请元数据。
struct Inviter {
    _dir: FixtureDir,
    welcome: Vec<u8>,
    invite: GroupInviteMeta,
}

impl Inviter {
    /// 返回邀请方数据 + 被邀请者的**同一台**设备（Welcome 绑定它的 KeyPackage）。
    fn new() -> (Self, MlsDevice) {
        let dir = FixtureDir::new("inviter");
        let mut store = dir.open();
        let mut alice = MlsDevice::new("alice").expect("alice");
        let bob = MlsDevice::new("bob").expect("bob");
        let request = CreateGroupRequest::new("邀请群", "alice");
        let peer = GroupBootstrapPeer {
            credential: "bob".to_owned(),
            device: &bob,
        };
        let (view, welcome) =
            create_group_with_welcome(&mut alice, &peer, &mut store, &request).expect("create");
        let invite = GroupInviteMeta {
            group_id: view.group_id.clone(),
            mls_group_id: view.mls_group_key.clone(),
            name: view.name.clone(),
            inviter_credential: "alice".to_owned(),
            owner_credential: "alice".to_owned(),
            invitee_credential: "bob".to_owned(),
            members: vec!["alice".to_owned(), "bob".to_owned()],
            epoch: view.epoch,
        };
        (
            Self {
                _dir: dir,
                welcome,
                invite,
            },
            bob,
        )
    }
}

#[test]
fn accepting_an_invite_joins_as_a_member_and_writes_the_directory() {
    let (inviter, mut bob) = Inviter::new();
    let receiver_dir = FixtureDir::new("receiver");
    let mut store = receiver_dir.open();
    let view = accept_group_invite(&mut bob, &mut store, &inviter.welcome, &inviter.invite)
        .expect("accept the invite");
    assert_eq!(view.group_id, inviter.invite.group_id);
    assert_eq!(view.name, "邀请群");
    assert_eq!(view.epoch, inviter.invite.epoch);
    assert_eq!(view.role, MemberRole::Member, "an invite joins as member");
    let mut members: Vec<String> = view.members.iter().map(|m| m.credential.clone()).collect();
    members.sort();
    assert_eq!(members, vec!["alice", "bob"]);
    // 快照已落库，本机是本地成员、群主是 alice。
    let snapshot = group_snapshot(&store, &inviter.invite.group_id).expect("snapshot");
    assert_eq!(snapshot.role, MemberRole::Member);
    let owner = snapshot
        .members
        .iter()
        .find(|member| member.role == MemberRole::Owner)
        .expect("owner member");
    assert_eq!(owner.credential, "alice");
    // 凭据目录已写入（本机 + 已知成员）。
    let directory = store.load_all_credential_directory().expect("load directory");
    let mut credentials: Vec<String> = directory.iter().map(|e| e.credential.clone()).collect();
    credentials.sort();
    assert_eq!(credentials, vec!["alice", "bob"]);
}

#[test]
fn metadata_mismatches_are_rejected_without_writing_anything() {
    let (inviter, mut bob) = Inviter::new();
    let receiver_dir = FixtureDir::new("mismatch");
    let mut store = receiver_dir.open();

    // epoch 不符：crypto 在写入正式库之前拒绝（并删掉刚写入 provider 的群状态）。
    let mut wrong_epoch = inviter.invite.clone();
    wrong_epoch.epoch = inviter.invite.epoch + 1;
    let error = accept_group_invite(&mut bob, &mut store, &inviter.welcome, &wrong_epoch)
        .expect_err("the epoch must match the welcome");
    assert_eq!(error.code(), "STATE_CORRUPTED");
    assert!(group_snapshot(&store, &inviter.invite.group_id).is_err());
    assert!(store.load_all_credential_directory().expect("directory").is_empty());
    assert!(bob.group_key().is_err(), "no MLS group was joined");

    // 成员集合不符。
    let mut wrong_members = inviter.invite.clone();
    wrong_members.members = vec!["alice".to_owned(), "bob".to_owned(), "carol".to_owned()];
    let error = accept_group_invite(&mut bob, &mut store, &inviter.welcome, &wrong_members)
        .expect_err("the member set must match the welcome");
    assert_eq!(error.code(), "STATE_CORRUPTED");
    assert!(group_snapshot(&store, &inviter.invite.group_id).is_err());

    // 邀请自身不自洽：群文本与 MLS 群 ID 推不出来。
    let mut inconsistent = inviter.invite.clone();
    inconsistent.group_id = format!("{}", inviter.invite.group_id);
    inconsistent.mls_group_id = "00".repeat(16);
    let error = accept_group_invite(&mut bob, &mut store, &inviter.welcome, &inconsistent)
        .expect_err("the group id must match its MLS group id");
    assert!(
        matches!(error.code(), "STATE_CORRUPTED" | "WELCOME_NOT_AUTHORIZED"),
        "unexpected code {}",
        error.code()
    );
    assert!(group_snapshot(&store, &inviter.invite.group_id).is_err());
    assert!(bob.group_key().is_err());
}

#[test]
fn accepting_the_same_invite_twice_is_an_invalid_transition() {
    let (inviter, mut bob) = Inviter::new();
    let receiver_dir = FixtureDir::new("duplicate");
    let mut store = receiver_dir.open();
    let first = accept_group_invite(&mut bob, &mut store, &inviter.welcome, &inviter.invite)
        .expect("first accept");
    let error = accept_group_invite(&mut bob, &mut store, &inviter.welcome, &inviter.invite)
        .expect_err("the second accept must be rejected");
    assert_eq!(error.code(), "INVALID_STATE_TRANSITION");
    // 快照没有被第二次接受破坏。
    let snapshot = group_snapshot(&store, &inviter.invite.group_id).expect("snapshot");
    assert_eq!(snapshot.state_version, first.state_version);
    assert_eq!(snapshot.epoch, first.epoch);
}

#[test]
fn malformed_invite_metadata_is_rejected() {
    let (inviter, mut bob) = Inviter::new();
    let receiver_dir = FixtureDir::new("malformed");
    let mut store = receiver_dir.open();

    // 未知群格式。
    let mut bad_group = inviter.invite.clone();
    bad_group.group_id = "not-a-group".to_owned();
    let error = accept_group_invite(&mut bob, &mut store, &inviter.welcome, &bad_group)
        .expect_err("a malformed group id is rejected");
    assert_eq!(error.code(), "INVALID_ID");

    // 群名非法（控制字符）。
    let mut bad_name = inviter.invite.clone();
    bad_name.name = "bad\u{7}name".to_owned();
    let error = accept_group_invite(&mut bob, &mut store, &inviter.welcome, &bad_name)
        .expect_err("a malformed name is rejected");
    assert_eq!(error.code(), "INVALID_INPUT");

    // 邀请者不是成员。
    let mut bad_inviter = inviter.invite.clone();
    bad_inviter.inviter_credential = "stranger".to_owned();
    let error = accept_group_invite(&mut bob, &mut store, &inviter.welcome, &bad_inviter)
        .expect_err("the inviter must be a member");
    assert_eq!(error.code(), "INVALID_INPUT");

    // 被邀请者不在成员集合里。
    let mut missing_invitee = inviter.invite.clone();
    missing_invitee.invitee_credential = "carol".to_owned();
    let error = accept_group_invite(&mut bob, &mut store, &inviter.welcome, &missing_invitee)
        .expect_err("the invitee must be in the member set");
    assert_eq!(error.code(), "INVALID_INPUT");

    // 以上全部失败都没有留下快照。
    assert!(group_snapshot(&store, &inviter.invite.group_id).is_err());
    assert!(bob.group_key().is_err());
}
