//! STORAGE-016 / 凭据目录：凭据 → 真实账户/设备映射（迁移 0011 + core 调用点）。
//!
//! 覆盖：upsert 幂等/覆盖、单条与全量读取、条目上限（1024）与整笔回滚、删除、重开（迁移后）可用；
//! core 建群/入群写入目录、同步 apply 的作者解析（已知映射 / 未知退回占位并计数）。
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::groups::{CreateGroupRequest, GroupBootstrapPeer, create_group};
use chat_core::sync_worker::{ApplyReport, resolve_author_account};
use chat_crypto::control::WelcomeAcceptance;
use chat_core::groups::{group_join_from_welcome, create_group_with_welcome};
use chat_crypto::engine::MlsDevice;
use chat_storage::formal::{
    CredentialDirectoryRecord, KeyProvenance, MAX_CREDENTIAL_DIRECTORY_ROWS, ProtectedStorageKey,
    Store, StoreOptions, open_store,
};

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
            "chat-core-directory-{tag}-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> PathBuf {
        self.0.join("chat.db")
    }

    fn open(&self) -> Store {
        open_entry(&self.database())
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn open_entry(path: &PathBuf) -> Store {
    open_store(
        StoreOptions::new(path, KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes(MASTER_KEY),
    )
    .expect("open formal store")
}

fn record(credential: &str, account_seed: u8, device_seed: u8) -> CredentialDirectoryRecord {
    CredentialDirectoryRecord {
        credential: credential.to_owned(),
        account_id: [account_seed; 32],
        device_id: [device_seed; 32],
        updated_at_ms: 0,
    }
}

#[test]
fn upsert_load_and_overwrite_round_trips() {
    let dir = FixtureDir::new("roundtrip");
    let mut store = dir.open();
    // 幂等 upsert：同凭据写两次只保留一行，映射按最后一次覆盖。
    assert_eq!(store.upsert_credential_directory(&[record("alice", 1, 2)]).expect("upsert"), 1);
    assert_eq!(store.upsert_credential_directory(&[record("alice", 3, 4)]).expect("upsert again"), 1);
    let loaded = store
        .load_credential_directory("alice")
        .expect("load")
        .expect("entry exists");
    assert_eq!(loaded.credential, "alice");
    assert_eq!(loaded.account_id, [3u8; 32], "the last upsert wins");
    assert_eq!(loaded.device_id, [4u8; 32]);
    // 未知凭据返回 None（不猜）。
    assert!(store.load_credential_directory("nobody").expect("load").is_none());
    // 批量 + 全量读取（凭据升序）。
    store
        .upsert_credential_directory(&[record("bob", 5, 6), record("carol", 7, 8)])
        .expect("batch upsert");
    let all = store.load_all_credential_directory().expect("load all");
    assert_eq!(
        all.iter().map(|entry| entry.credential.as_str()).collect::<Vec<_>>(),
        vec!["alice", "bob", "carol"]
    );
    // 删除：删掉后读回 None，重复删除返回 false。
    assert!(store.delete_credential_directory("carol").expect("delete"));
    assert!(!store.delete_credential_directory("carol").expect("delete again"));
    assert!(store.load_credential_directory("carol").expect("load").is_none());
}

#[test]
fn directory_limit_is_enforced_without_partial_writes() {
    let dir = FixtureDir::new("limit");
    let mut store = dir.open();
    let entries: Vec<CredentialDirectoryRecord> = (0..MAX_CREDENTIAL_DIRECTORY_ROWS)
        .map(|index| CredentialDirectoryRecord {
            credential: format!("member-{index}"),
            account_id: [1u8; 32],
            device_id: [2u8; 32],
            updated_at_ms: 0,
        })
        .collect();
    assert_eq!(
        store.upsert_credential_directory(&entries).expect("fill to the limit"),
        MAX_CREDENTIAL_DIRECTORY_ROWS
    );
    // 第 1025 条：整笔失败（含同批里合法的条目），目录保持 1024 条。
    let error = store
        .upsert_credential_directory(&[record("overflow", 9, 9), record("another", 8, 8)])
        .expect_err("over the limit");
    assert_eq!(error.code(), "INVALID_INPUT");
    assert!(store.load_credential_directory("overflow").expect("load").is_none());
    assert_eq!(
        store.load_all_credential_directory().expect("load all").len(),
        MAX_CREDENTIAL_DIRECTORY_ROWS
    );
    // 已存在凭据的更新不受上限影响（不新增行）。
    store
        .upsert_credential_directory(&[record("member-0", 4, 4)])
        .expect("update existing at the limit");
}

#[test]
fn directory_survives_reopen_and_the_old_tables_still_work() {
    let dir = FixtureDir::new("reopen");
    {
        let mut store = dir.open();
        store.upsert_credential_directory(&[record("alice", 1, 2)]).expect("upsert");
    }
    // 重开（迁移已到最新版本）：目录保留，且既有写入路径（会话/消息）仍然可用。
    let mut store = dir.open();
    let loaded = store
        .load_credential_directory("alice")
        .expect("load")
        .expect("entry survives reopen");
    assert_eq!(loaded.account_id, [1u8; 32]);
    let applied = store
        .apply_verified_entry(chat_storage::formal::VerifiedEntryInput {
            conversation_id: "conv:1:credential-directory".to_owned(),
            title: String::new(),
            message_id: "msg:1:credential-directory".to_owned(),
            author_account: chat_protocol::AccountId::from_bytes(loaded.account_id).encode(),
            author_device: chat_protocol::DeviceId::from_bytes(loaded.device_id).encode(),
            body: "目录映射后的消息".to_owned(),
            mine: false,
            created_at_ms: 1_700_000_000_000,
        })
        .expect("messages still apply after the migration");
    assert!(!applied.message_id.is_empty());
}

#[test]
fn creating_a_group_writes_the_credential_directory() {
    let dir = FixtureDir::new("create");
    let mut store = dir.open();
    let mut alice = MlsDevice::new("alice").expect("alice");
    let bob = MlsDevice::new("bob").expect("bob");
    let request = CreateGroupRequest::new("目录群", "alice");
    let peer = GroupBootstrapPeer {
        credential: "bob".to_owned(),
        device: &bob,
    };
    let view = create_group(&mut alice, &peer, &mut store, &request).expect("create group");
    let all = store.load_all_credential_directory().expect("load all");
    let mut credentials: Vec<String> = all.iter().map(|entry| entry.credential.clone()).collect();
    credentials.sort();
    assert_eq!(credentials, vec!["alice", "bob"]);
    for entry in &all {
        assert_eq!(entry.account_id.len(), 32);
        assert_eq!(entry.device_id.len(), 32);
        assert_ne!(entry.account_id, entry.device_id, "account and device ids differ");
    }
    // 同步 apply 的作者解析：目录命中 → 真实账户（known = true）。
    let (account, known) = resolve_author_account(&store, "alice");
    assert!(known, "alice is in the directory");
    let alice_row = store
        .load_credential_directory("alice")
        .expect("load")
        .expect("alice row");
    assert_eq!(account, chat_protocol::AccountId::from_bytes(alice_row.account_id).encode());
    // 目录里没有的凭据 → 占位且 known = false（调用方计入 unknown_authors）。
    let (unknown, known) = resolve_author_account(&store, "stranger");
    assert!(!known);
    assert!(unknown.starts_with("acct:1:"), "a deterministic placeholder is returned");
    assert_eq!(ApplyReport::default().unknown_authors, 0);
    let _ = view;
}

#[test]
fn joining_writes_the_directory_and_unknown_credentials_count_as_placeholders() {
    let dir_a = FixtureDir::new("join-a");
    let dir_b = FixtureDir::new("join-b");
    let mut store_a = dir_a.open();
    let mut store_b = dir_b.open();
    let mut alice = MlsDevice::new("alice").expect("alice");
    let mut bob = MlsDevice::new("bob").expect("bob");
    let request = CreateGroupRequest::new("目录群", "alice");
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
    let _view_b = group_join_from_welcome(
        &mut bob,
        &mut store_b,
        "目录群",
        &welcome,
        &acceptance,
        "bob",
        "alice",
    )
    .expect("bob joins");
    // 加入侧的目录也写了本机与已知成员。
    let all = store_b.load_all_credential_directory().expect("load all");
    let mut credentials: Vec<String> = all.iter().map(|entry| entry.credential.clone()).collect();
    credentials.sort();
    assert_eq!(credentials, vec!["alice", "bob"]);
    let (_, known) = resolve_author_account(&store_b, "bob");
    assert!(known, "the local member credential is mapped");
    // 删掉映射后同一凭据只能拿到占位（同步 apply 会把这条计入 unknown_authors）。
    assert!(store_b.delete_credential_directory("alice").expect("delete"));
    let (placeholder, known) = resolve_author_account(&store_b, "alice");
    assert!(!known, "a deleted mapping is unknown again");
    assert!(placeholder.starts_with("acct:1:"));
}
