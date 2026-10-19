//! ATTACHMENTS-008：附件两阶段回收（计划/执行/崩溃恢复），真正式库。
//!
//! 覆盖：有消息引用保留、宽限期内保留、超期回收计数正确（blob+密钥+进度行）、
//! 崩溃恢复（只计划不执行 → 重新计划仍包含 → 执行 → 再计划为空）、幂等、上限截断。
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::attachments::{
    execute_attachment_gc, pending_attachment_gc, plan_attachment_gc,
};
use chat_storage::formal::{
    AttachmentProgressInput, AttachmentRecordInput, AttachmentSecretInput, KeyProvenance,
    ProtectedStorageKey, Store, StoreOptions, VerifiedEntryInput, open_store,
};

const MASTER_KEY: [u8; 32] = [0x5a; 32];
const NOW: i64 = 1_800_000_000_000;
const CREATED: i64 = 1_700_000_000_000;

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
            "chat-core-gc-{tag}-{}-{nanos}-{counter}",
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
        display_name: "会议纪要.pdf".to_owned(),
        created_at_ms,
    }
}

fn secret(seed: u8) -> AttachmentSecretInput {
    AttachmentSecretInput {
        attachment_id: attachment_id(seed),
        file_id: [seed; 16],
        file_key: vec![seed ^ 0x33; 32],
        created_at_ms: CREATED,
    }
}

fn progress(seed: u8) -> AttachmentProgressInput {
    AttachmentProgressInput {
        attachment_id: attachment_id(seed),
        direction: "upload".to_owned(),
        chunk_count: 1,
        confirmed: vec![true],
        session_id: Some("sess-1".to_owned()),
        blob_id: Some("obj:1:demo".to_owned()),
        prefix_digest: None,
    }
}

/// 建一条正式消息（用于制造“附件被消息引用”）。
fn message(store: &mut Store, message_id: &str) {
    store
        .apply_verified_entry(VerifiedEntryInput {
            conversation_id: "conv:1:attachment-gc".to_owned(),
            title: String::new(),
            message_id: message_id.to_owned(),
            author_account: chat_protocol::AccountId::from_bytes([1u8; 32]).encode(),
            author_device: chat_protocol::DeviceId::from_bytes([2u8; 32]).encode(),
            body: "带附件的消息".to_owned(),
            mine: false,
            created_at_ms: CREATED,
        })
        .expect("apply message");
}

#[test]
fn referenced_attachments_are_kept_and_expired_unreferenced_ones_are_reclaimed() {
    let dir = FixtureDir::new("referenced");
    let mut store = dir.open();
    // ① 被消息引用（宽限期已过）→ 保留。
    store.save_attachment(record(1, CREATED)).expect("save referenced");
    store.save_attachment_secret(secret(1)).expect("secret");
    message(&mut store, "msg:1:gc-referenced");
    store
        .attach_message_attachment("msg:1:gc-referenced", &attachment_id(1))
        .expect("link message attachment");
    // ② 无引用且超期 → 回收。
    store.save_attachment(record(2, CREATED)).expect("save orphan");
    store.save_attachment_secret(secret(2)).expect("secret");
    store.save_attachment_progress(progress(2)).expect("progress");

    let plan = plan_attachment_gc(&store, NOW, 60_000).expect("plan");
    assert_eq!(plan.blobs, 1, "only the orphan is a candidate");
    assert_eq!(plan.candidates.len(), 1);
    assert_eq!(plan.candidates[0].attachment_id, attachment_id(2));
    assert_eq!(plan.secrets, 1);
    assert_eq!(plan.progress_rows, 1);
    assert_eq!(plan.skipped_referenced, 1);

    let report = execute_attachment_gc(&mut store, &plan).expect("execute");
    assert_eq!(report.marked, 1);
    assert_eq!(report.removed, 1);
    assert_eq!(report.skipped, 0);
    // 孤儿记录/密钥/进度都没了；被引用的记录还在。
    assert!(store.load_attachment(&attachment_id(2)).expect("load").is_none());
    assert!(store.load_attachment_secret(&attachment_id(2)).expect("secret").is_none());
    assert!(store.load_attachment_progress(&attachment_id(2), "upload").expect("progress").is_none());
    assert!(store.load_attachment(&attachment_id(1)).expect("load").is_some());
    assert!(store.load_attachment_secret(&attachment_id(1)).expect("secret").is_some());
}

#[test]
fn attachments_within_the_grace_window_are_kept() {
    let dir = FixtureDir::new("grace");
    let mut store = dir.open();
    store.save_attachment(record(3, CREATED)).expect("save");
    // now - created = 1000ms，宽限期 60s → 仍在窗口内。
    let plan = plan_attachment_gc(&store, CREATED + 1_000, 60_000).expect("plan");
    assert!(plan.candidates.is_empty(), "within grace: nothing to collect");
    assert_eq!(plan.skipped_within_grace, 1);
    assert!(store.load_attachment(&attachment_id(3)).expect("load").is_some());
    // 同一个库把 now 推过宽限期后就能回收。
    let plan = plan_attachment_gc(&store, CREATED + 60_001, 60_000).expect("plan later");
    assert_eq!(plan.blobs, 1);
    let report = execute_attachment_gc(&mut store, &plan).expect("execute");
    assert_eq!(report.removed, 1);
}

#[test]
fn a_planned_but_unexecuted_gc_is_replanned_and_then_completes() {
    let dir = FixtureDir::new("crash");
    let mut store = dir.open();
    store.save_attachment(record(4, CREATED)).expect("save");
    store.save_attachment_secret(secret(4)).expect("secret");
    // 第一阶段：只计划（模拟“标记前崩溃”）。
    let plan = plan_attachment_gc(&store, NOW, 0).expect("plan");
    assert_eq!(plan.blobs, 1);
    assert!(pending_attachment_gc(&store).expect("pending").is_empty());
    // 重新计划：仍然包含同一个候选（计划是只读的）。
    let replanned = plan_attachment_gc(&store, NOW, 0).expect("replan");
    assert_eq!(replanned.candidates, plan.candidates);
    // 第二阶段：执行成功 → 再计划为空。
    let report = execute_attachment_gc(&mut store, &replanned).expect("execute");
    assert_eq!(report.removed, 1);
    let after = plan_attachment_gc(&store, NOW, 0).expect("plan after");
    assert!(after.candidates.is_empty(), "nothing left to plan");
    assert!(pending_attachment_gc(&store).expect("pending").is_empty());
}

#[test]
fn executing_the_same_plan_twice_is_idempotent() {
    let dir = FixtureDir::new("idempotent");
    let mut store = dir.open();
    store.save_attachment(record(5, CREATED)).expect("save");
    let plan = plan_attachment_gc(&store, NOW, 0).expect("plan");
    let first = execute_attachment_gc(&mut store, &plan).expect("first execute");
    assert_eq!(first.marked, 1);
    assert_eq!(first.removed, 1);
    let second = execute_attachment_gc(&mut store, &plan).expect("second execute");
    assert_eq!(second.marked, 0, "already deleted rows are not marked again");
    assert_eq!(second.removed, 0, "no double delete");
    assert_eq!(second.skipped, 1);
}

#[test]
fn the_plan_is_bounded_and_truncation_is_reported() {
    let dir = FixtureDir::new("limit");
    let mut store = dir.open();
    for seed in 10..16u8 {
        store.save_attachment(record(seed, CREATED + i64::from(seed))).expect("save");
    }
    let plan = plan_attachment_gc(&store, NOW, 0).expect("plan") ;
    assert_eq!(plan.blobs, 6);
    assert!(!plan.truncated);
    // chat-core 的薄封装固定用 MAX_ATTACHMENT_GC_BATCH=256，所以这里直接用 storage 的小上限验证截断。
    let small = store.plan_attachment_gc(NOW, 0, 2).expect("small plan");
    assert_eq!(small.candidates.len(), 2);
    assert_eq!(small.blobs, 2);
    assert!(small.truncated, "more candidates remain beyond the limit");
    let report = execute_attachment_gc(&mut store, &small).expect("execute small");
    assert_eq!(report.removed, 2);
    let rest = store.plan_attachment_gc(NOW, 0, 16).expect("rest plan");
    assert_eq!(rest.candidates.len(), 4);
}
