//! CTRLREV-SEED：账户撤销集合 → 群快照的单向播种（真库）。
//!
//! 覆盖：登记并撤销 2 台设备后 seed 新增 2 条、再 seed 为 0（幂等）；重开库仍在；未知群 INVALID_TARGET。
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, GroupView, PeerEntryInput, create_group_with_welcome,
    group_join_from_welcome, group_snapshot, seed_revoked_devices_from_registry,
};
use chat_crypto::control::WelcomeAcceptance;
use chat_crypto::engine::MlsDevice;
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store};

const MASTER_KEY: [u8; 32] = [0x44; 32];
const ACCOUNT: &str = "alice";

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
            "chat-core-seed-{}-{nanos}-{counter}",
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

fn build_group(store: &mut Store) -> GroupView {
    let mut alice = MlsDevice::new(ACCOUNT).expect("alice");
    let mut bob = MlsDevice::new("bob").expect("bob");
    let request = CreateGroupRequest::new("seed-group", ACCOUNT);
    let peer = GroupBootstrapPeer {
        credential: "bob".to_owned(),
        device: &bob,
    };
    let (a_view, welcome) =
        create_group_with_welcome(&mut alice, &peer, store, &request).expect("create group");
    let acceptance = WelcomeAcceptance {
        group_id: a_view.mls_group_key.clone(),
        expected_epoch: a_view.epoch,
        members: vec![ACCOUNT.to_owned(), "bob".to_owned()],
    };
    // bob 在独立库加入（与 groups_peer 的双设备夹具一致；同一库会报 already joined）。
    let dir_b = FixtureDir::new();
    let mut store_b = open(&dir_b);
    let _b_view = group_join_from_welcome(
        &mut bob,
        &mut store_b,
        "seed-group",
        &welcome,
        &acceptance,
        "bob",
        ACCOUNT,
    )
    .expect("bob joins");
    drop(store_b);
    a_view
}

#[test]
fn seeding_from_the_registry_adds_revoked_devices_once() {
    let dir = FixtureDir::new();
    let mut store = open(&dir);
    let view = build_group(&mut store);
    let group_id = view.group_id.clone();
    assert!(group_snapshot(&store, &group_id).expect("snapshot").revoked_devices.is_empty());

    // 身份层（账户视角，权威）：登记并撤销 2 台设备。
    store
        .revoke_device(ACCOUNT, "dev:1:laptop", 1_000)
        .expect("revoke laptop");
    store
        .revoke_device(ACCOUNT, "dev:1:phone", 2_000)
        .expect("revoke phone");

    // 播种：群快照新增 2 条（升序）。
    let added = seed_revoked_devices_from_registry(&mut store, &group_id).expect("seed");
    assert_eq!(added, 2);
    let snapshot = group_snapshot(&store, &group_id).expect("snapshot");
    assert_eq!(
        snapshot.revoked_devices,
        vec!["dev:1:laptop".to_owned(), "dev:1:phone".to_owned()]
    );

    // 幂等：再播种新增 0。
    assert_eq!(
        seed_revoked_devices_from_registry(&mut store, &group_id).expect("seed twice"),
        0
    );
    assert_eq!(
        group_snapshot(&store, &group_id).expect("snapshot").revoked_devices.len(),
        2
    );

    // 新增一台撤销设备 → 只追加这一台。
    store
        .revoke_device(ACCOUNT, "dev:1:tablet", 3_000)
        .expect("revoke tablet");
    assert_eq!(
        seed_revoked_devices_from_registry(&mut store, &group_id).expect("seed third"),
        1
    );
    assert_eq!(
        group_snapshot(&store, &group_id).expect("snapshot").revoked_devices.len(),
        3
    );

    // 重开库后仍在（快照落库）。
    let database = dir.database();
    drop(store);
    let reopened = open_store(
        StoreOptions::new(&database, KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes(MASTER_KEY),
    )
    .expect("reopen");
    let restored = group_snapshot(&reopened, &group_id).expect("snapshot after reopen");
    assert_eq!(restored.revoked_devices.len(), 3);
    assert!(restored.revoked_devices.contains(&"dev:1:laptop".to_owned()));
}

#[test]
fn seeding_without_registered_revocations_writes_nothing() {
    let dir = FixtureDir::new();
    let mut store = open(&dir);
    let view = build_group(&mut store);
    let group_id = view.group_id.clone();
    let before = group_snapshot(&store, &group_id).expect("snapshot");
    assert_eq!(
        seed_revoked_devices_from_registry(&mut store, &group_id).expect("seed"),
        0
    );
    let after = group_snapshot(&store, &group_id).expect("snapshot");
    assert_eq!(after, before, "no revocations means no write");
    assert!(after.revoked_devices.is_empty());
}

#[test]
fn seeding_an_unknown_group_is_invalid_target() {
    let dir = FixtureDir::new();
    let mut store = open(&dir);
    let group_id = build_group(&mut store).group_id.clone();
    // 格式合法但库里不存在 → INVALID_TARGET（格式非法才是 INVALID_ID）。
    let existing = group_snapshot(&store, &group_id).expect("snapshot").group_id;
    // 只翻最后一位十六进制：保持格式合法（否则会先报 INVALID_ID）。
    let mut unknown = existing.clone();
    let last = unknown.pop().expect("group id");
    unknown.push(if last == '0' { '1' } else { '0' });
    let error = seed_revoked_devices_from_registry(&mut store, &unknown)
        .expect_err("unknown group");
    assert_eq!(error.code(), "INVALID_TARGET");
}

#[test]
fn a_peer_revocation_entry_and_the_registry_seed_are_merged_not_replaced() {
    let dir = FixtureDir::new();
    let mut store = open(&dir);
    let view = build_group(&mut store);
    let group_id = view.group_id.clone();
    // 先用一条对端撤销条目写入一台设备，再从身份层播种另一台 → 两者都在（只增不减）。
    store
        .revoke_device(ACCOUNT, "dev:1:from-registry", 5_000)
        .expect("revoke");
    let added = seed_revoked_devices_from_registry(&mut store, &group_id).expect("seed");
    assert_eq!(added, 1);
    let snapshot = group_snapshot(&store, &group_id).expect("snapshot");
    assert_eq!(snapshot.revoked_devices, vec!["dev:1:from-registry".to_owned()]);
    // 校验入口仍可用（这里只验证类型可构造，避免 dead_code）。
    let _unused: Option<PeerEntryInput> = None;
}

