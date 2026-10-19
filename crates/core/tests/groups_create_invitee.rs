//! GROUPS-004：用真实被邀请者的 KeyPackage 建群，Welcome 真的可用于入群。
//!
//! 核心证据：B 生成 key_package → A 用它建群并拿到 Welcome → **B 用该 Welcome 成功入群**。
//! 反例：凭据不匹配 / KeyPackage 损坏 → 显式拒绝且**无写入**。
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::groups::{
    CreateGroupRequest, GroupView, create_group_with_invitee_key_package, group_join_from_welcome,
    group_snapshot,
};
use chat_crypto::control::WelcomeAcceptance;
use chat_crypto::engine::MlsDevice;
use chat_domain::MemberRole;
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store};

const MASTER_KEY: [u8; 32] = [0x6b; 32];

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
            "chat-core-group4-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("fixture dir");
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

#[test]
fn an_external_invitee_joins_with_the_welcome_from_the_real_key_package() {
    let dir_a = FixtureDir::new();
    let dir_b = FixtureDir::new();
    let mut store_a = dir_a.open();
    let mut store_b = dir_b.open();
    let mut alice = MlsDevice::new("alice").expect("alice");
    let mut bob = MlsDevice::new("bob").expect("bob");
    // B 侧：真实设备导出 KeyPackage（不在 A 进程里）。
    let key_package = bob.key_package().expect("bob key package");
    let request = CreateGroupRequest::new("external-invite", "alice");
    let (view, welcome) = create_group_with_invitee_key_package(
        &mut alice,
        "bob",
        &key_package,
        &mut store_a,
        &request,
    )
    .expect("group created with the external key package");
    assert_eq!(view.epoch, 1);
    assert_eq!(view.role, MemberRole::Owner);
    let mut members: Vec<String> = view.members.iter().map(|m| m.credential.clone()).collect();
    members.sort();
    assert_eq!(members, vec!["alice".to_owned(), "bob".to_owned()]);
    assert!(!welcome.is_empty(), "a real welcome is produced");

    // 核心证据：B 用这个 Welcome 真的入群。
    let acceptance = WelcomeAcceptance {
        group_id: view.mls_group_key.clone(),
        expected_epoch: view.epoch,
        members: vec!["alice".to_owned(), "bob".to_owned()],
    };
    let b_view = group_join_from_welcome(
        &mut bob,
        &mut store_b,
        "external-invite",
        &welcome,
        &acceptance,
        "bob",
        "alice",
    )
    .expect("bob joins with the real welcome");
    assert_eq!(b_view.group_id, view.group_id);
    assert_eq!(b_view.epoch, view.epoch);
    assert_eq!(b_view.role, MemberRole::Member);
    let mut b_members: Vec<String> = b_view.members.iter().map(|m| m.credential.clone()).collect();
    b_members.sort();
    assert_eq!(b_members, vec!["alice".to_owned(), "bob".to_owned()]);
    // 两侧快照都落库。
    assert_eq!(group_snapshot(&store_a, &view.group_id).expect("a").mls_group_key, view.mls_group_key);
    assert_eq!(group_snapshot(&store_b, &view.group_id).expect("b").mls_group_key, view.mls_group_key);
}

#[test]
fn a_credential_mismatch_is_rejected_without_any_write() {
    let dir_a = FixtureDir::new();
    let mut store_a = dir_a.open();
    let mut alice = MlsDevice::new("alice").expect("alice");
    let bob = MlsDevice::new("bob").expect("bob");
    let key_package = bob.key_package().expect("bob key package");
    let request = CreateGroupRequest::new("external-invite", "alice");
    // 期望凭据写错（carol）→ 拒绝。
    let error = create_group_with_invitee_key_package(
        &mut alice,
        "carol",
        &key_package,
        &mut store_a,
        &request,
    )
    .expect_err("credential mismatch");
    assert_eq!(error.code(), "INVALID_CANDIDATE");
    // 没有任何群被写入（库里的群列表为空）。
    let groups = chat_core::groups::group_list(&store_a).expect("group list");
    assert!(groups.groups.is_empty(), "a rejected creation must not write a snapshot");
}

#[test]
fn a_corrupted_key_package_is_rejected_without_any_write() {
    let dir_a = FixtureDir::new();
    let mut store_a = dir_a.open();
    let mut alice = MlsDevice::new("alice").expect("alice");
    let request = CreateGroupRequest::new("external-invite", "alice");
    for bad in [vec![0u8; 8], vec![0xffu8; 200], vec![1u8; 1]] {
        let error = create_group_with_invitee_key_package(
            &mut alice,
            "bob",
            &bad,
            &mut store_a,
            &request,
        )
        .expect_err("corrupted key package");
        assert_eq!(error.code(), "INVALID_CANDIDATE");
    }
    let groups = chat_core::groups::group_list(&store_a).expect("group list");
    assert!(groups.groups.is_empty());
    // 空包同样拒绝。
    let error = create_group_with_invitee_key_package(&mut alice, "bob", &[], &mut store_a, &request)
        .expect_err("empty key package");
    assert_eq!(error.code(), "INVALID_CANDIDATE");
}

#[allow(dead_code)]
fn _types(_: GroupView) {}

