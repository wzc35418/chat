//! 常驻运行时的附件回收阶段（真库；无用例依赖真实节点）。
//!
//! 覆盖：超期无引用对象在若干轮后被回收且计数增长、宽限期内不回收、关闭 GC 计数恒 0、
//! GC 失败只写自己的状态且不阻塞拉取、stop 在任务边界生效。
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chat_core::sync_runtime::{SyncRuntime, SyncRuntimeConfig, SyncRuntimePolicy};
use chat_core::sync_worker::initialize_pull_state;
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_protocol::{ControlHead, GroupId, VerifiedHead};
use chat_storage::formal::{
    AttachmentRecordInput, AttachmentSecretInput, KeyProvenance, ProtectedStorageKey, Store,
    StoreOptions, VerifiedEntryInput, open_store,
};
use ed25519_dalek::SigningKey;

const MASTER_KEY: [u8; 32] = [0x5a; 32];
const DEVICE_SEED: u8 = 41;
const GROUP_SEED: u8 = 61;
const WAIT_TIMEOUT: Duration = Duration::from_secs(20);

fn master_key_hex() -> String {
    MASTER_KEY.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn group_id() -> String {
    GroupId::from_bytes([GROUP_SEED; 32]).encode()
}

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
            "chat-core-runtime-gc-{tag}-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn path(&self) -> PathBuf {
        self.0.join("chat.db")
    }

    fn open(&self) -> Store {
        open_store(
            StoreOptions::new(self.path(), KeyProvenance::Raw32),
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

fn attachment_id(seed: u8) -> String {
    format!("att-{}", format!("{seed:02x}").repeat(16))
}

fn record(seed: u8, created_at_ms: i64) -> AttachmentRecordInput {
    AttachmentRecordInput {
        attachment_id: attachment_id(seed),
        state: "available".to_owned(),
        plain_size: 4096,
        chunk_size: 262_144,
        chunk_count: 1,
        content_digest: "ab".repeat(32),
        ciphertext_digest: "cd".repeat(32),
        display_name: "纪要.pdf".to_owned(),
        created_at_ms,
    }
}

fn secret(seed: u8) -> AttachmentSecretInput {
    AttachmentSecretInput {
        attachment_id: attachment_id(seed),
        file_id: [seed; 16],
        file_key: vec![seed ^ 0x55; 32],
        created_at_ms: 1,
    }
}

/// 初始化游标并准备一个“必然不可达”的客户端：拉取会失败（可重试），
/// 但正是这样能证明回收阶段与拉取互相独立。
fn config(dir: &FixtureDir, interval_ms: u64, grace_ms: i64) -> SyncRuntimeConfig {
    let endpoint = NodeEndpoint::new("http://127.0.0.1:9", "node-gc-test")
        .expect("endpoint")
        .with_node_public_key([7u8; 32]);
    let client = NodeClient::new(
        endpoint,
        chat_protocol::DeviceId::from_bytes([DEVICE_SEED; 32]).encode(),
        SigningKey::from_bytes(&[DEVICE_SEED; 32]),
    )
    .expect("client");
    let policy = SyncRuntimePolicy {
        round_wait_min_ms: 5,
        round_wait_max_ms: 10,
        // 拉取一直失败也不让它耗尽预算：本用例只关心回收阶段与拉取互不影响。
        max_failures: 1_000_000,
    };
    SyncRuntimeConfig::new(dir.path(), &master_key_hex(), client, group_id())
        .expect("config")
        .with_policy(policy)
        .with_attachment_gc(interval_ms, grace_ms)
}

fn init_pull_state(store: &mut Store) {
    let id = group_id();
    initialize_pull_state(
        store,
        &id,
        VerifiedHead::genesis(
            GroupId::parse(&id).expect("group id"),
            ControlHead::ZERO,
            1,
            0,
        ),
    )
    .expect("pull state");
}

fn wait_for(what: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

/// 建一条带附件引用的消息（让目标附件不可回收）。
fn referenced_attachment(store: &mut Store, seed: u8, message_id: &str) {
    store.save_attachment(record(seed, 1)).expect("save");
    store
        .apply_verified_entry(VerifiedEntryInput {
            conversation_id: "conv:1:runtime-gc".to_owned(),
            title: String::new(),
            message_id: message_id.to_owned(),
            author_account: chat_protocol::AccountId::from_bytes([1u8; 32]).encode(),
            author_device: chat_protocol::DeviceId::from_bytes([2u8; 32]).encode(),
            body: "带附件的消息".to_owned(),
            mine: false,
            created_at_ms: 1,
        })
        .expect("apply message");
    store
        .attach_message_attachment(message_id, &attachment_id(seed))
        .expect("link");
}

#[test]
fn expired_unreferenced_attachments_are_reclaimed_and_counted() {
    let dir = FixtureDir::new("reclaim");
    {
        let mut store = dir.open();
        init_pull_state(&mut store);
        // 两个超期无引用（会被回收）+ 一个被消息引用（保留）+ 一个新的（宽限期内）。
        for seed in [1u8, 2] {
            store.save_attachment(record(seed, 1)).expect("save");
            store.save_attachment_secret(secret(seed)).expect("secret");
        }
        referenced_attachment(&mut store, 3, "msg:1:gc-referenced");
        // 刚创建（created_at = 现在）→ 落在 60s 宽限期内，必须保留。
        let fresh_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        store.save_attachment(record(4, fresh_ms)).expect("save fresh");
    }
    let runtime = SyncRuntime::start(config(&dir, 5, 60_000)).expect("runtime starts");
    let handle = runtime.status_handle();
    wait_for("gc_removed_total == 2", || handle.view().gc_removed_total >= 2);
    let status = handle.view();
    assert_eq!(status.gc_removed_total, 2, "both expired orphans are reclaimed");
    assert!(status.gc_last_error_code.is_none(), "unexpected gc error: {status:?}");
    assert!(status.gc_next_attempt_at_ms > 0);
    let store = runtime.stop().expect("clean stop");
    assert!(store.load_attachment(&attachment_id(1)).expect("load").is_none());
    assert!(store.load_attachment(&attachment_id(2)).expect("load").is_none());
    assert!(store.load_attachment_secret(&attachment_id(1)).expect("secret").is_none());
    assert!(
        store.load_attachment(&attachment_id(3)).expect("load").is_some(),
        "referenced attachments are kept"
    );
    assert!(
        store.load_attachment(&attachment_id(4)).expect("load").is_some(),
        "attachments within the grace window are kept"
    );
}

#[test]
fn gc_disabled_keeps_the_counter_at_zero() {
    let dir = FixtureDir::new("disabled");
    {
        let mut store = dir.open();
        init_pull_state(&mut store);
        store.save_attachment(record(5, 1)).expect("save");
    }
    let runtime = SyncRuntime::start(config(&dir, 0, 0)).expect("runtime starts");
    let handle = runtime.status_handle();
    wait_for("rounds >= 3", || handle.view().rounds >= 3);
    let status = handle.view();
    assert_eq!(status.gc_removed_total, 0, "gc is disabled");
    assert!(status.gc_last_error_code.is_none());
    let store = runtime.stop().expect("stop");
    assert!(
        store.load_attachment(&attachment_id(5)).expect("load").is_some(),
        "nothing is reclaimed when gc is off"
    );
}

#[test]
fn a_failing_gc_never_blocks_the_pull_loop() {
    let dir = FixtureDir::new("failing");
    {
        let mut store = dir.open();
        init_pull_state(&mut store);
        store.save_attachment(record(6, 1)).expect("save");
    }
    // 负宽限期是非法配置：回收阶段必然失败（INVALID_INPUT），但拉取阶段照常推进。
    let runtime = SyncRuntime::start(config(&dir, 5, -1)).expect("runtime starts");
    let handle = runtime.status_handle();
    wait_for("gc error recorded", || {
        handle.view().gc_last_error_code.is_some()
    });
    let status = handle.view();
    assert_eq!(status.gc_last_error_code, Some("INVALID_INPUT"));
    assert_eq!(status.gc_removed_total, 0);
    // 拉取阶段仍在跑：轮数在增长（失败只记账，不阻塞下一轮）。
    let rounds = status.rounds;
    wait_for("rounds keep growing", || handle.view().rounds > rounds);
    assert!(!handle.view().is_stopped(), "the loop is still running");
    let store = runtime.stop().expect("stop");
    assert!(
        store.load_attachment(&attachment_id(6)).expect("load").is_some(),
        "a failing gc touches nothing"
    );
}
