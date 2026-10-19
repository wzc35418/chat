//! NODE-018：审计持久化与保留期（真库）。
//!
//! 覆盖：落库顺序与过滤、重启仍可查、保留期阈值与单次上限、关闭审计不写不读、viewer 跨设备过滤。
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use chat_node_services::audit::{
    AUDIT_DEFAULT_RETENTION_MS, AUDIT_PURGE_MAX_BATCH, AuditEvent, AuditOutcome, AuditSource,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};

fn temp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "chat-audit-persist-{tag}-{}-{counter}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn config() -> NodeServiceConfig {
    NodeServiceConfig {
        node_id: "node-audit-persist".into(),
        signing_seed: [5u8; 32],
        policy: NodeAuthPolicy::default(),
    }
}

fn event(kind: &str, at_ms: u64, device: Option<&str>) -> AuditEvent {
    let mut event = AuditEvent::new(kind, at_ms, AuditOutcome::Ok);
    if let Some(device) = device {
        event = event.with_device(device);
    }
    event
}

#[test]
fn persisted_events_survive_a_restart_and_keep_order() {
    let dir = temp_dir("restart");
    let path = dir.join("node.db");
    {
        let mut service = NodeService::open(&path, config()).expect("open");
        service
            .record_audit_persisted(event("http", 1_000, None))
            .expect("persist first");
        service
            .record_audit_persisted(event("append", 1_001, Some("dev:1:a")))
            .expect("persist second");
        service
            .record_audit_persisted(event("control", 1_002, Some("dev:1:a")))
            .expect("persist third");
        assert_eq!(service.audit_persisted_count().expect("count"), 3);
    }
    // 重启：内存空 → audit_tail 回退读库，并标注来源。
    let service = NodeService::open(&path, config()).expect("reopen");
    let (events, source) = service.audit_tail_with_source(10).expect("tail");
    assert_eq!(source, AuditSource::Persisted, "restart reads from the database");
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].kind, "http");
    assert_eq!(
        events.iter().map(|event| event.at_ms).collect::<Vec<_>>(),
        vec![1_000, 1_001, 1_002],
        "order is preserved"
    );
    assert_eq!(service.audit_persisted_count().expect("count"), 3);
    drop(service);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn query_filters_by_since_kind_and_viewer_device() {
    let dir = temp_dir("query");
    let path = dir.join("node.db");
    let mut service = NodeService::open(&path, config()).expect("open");
    service.record_audit_persisted(event("http", 100, None)).unwrap();
    service
        .record_audit_persisted(event("append", 200, Some("dev:1:mine")))
        .unwrap();
    service
        .record_audit_persisted(event("append", 300, Some("dev:1:other")))
        .unwrap();
    service
        .record_audit_persisted(event("control", 400, Some("dev:1:mine")))
        .unwrap();
    // since_ms 过滤。
    let since = service.audit_query(10, 250, None, None).expect("query");
    assert_eq!(since.len(), 2, "only events at/after 250: {since:?}");
    // kind 过滤。
    let kind = service.audit_query(10, 0, Some("append"), None).expect("kind");
    assert_eq!(kind.len(), 2);
    assert!(kind.iter().all(|event| event.kind == "append"));
    // viewer 过滤：本设备 + 节点级（NULL），其它设备不可见。
    let viewer = service
        .audit_query(10, 0, None, Some("dev:1:mine"))
        .expect("viewer");
    assert_eq!(viewer.len(), 3, "node-level + own events: {viewer:?}");
    assert!(!viewer
        .iter()
        .any(|event| event.device_id.as_deref() == Some("dev:1:other")));
    // limit 上限：0 与 201 都被拒绝。
    assert!(service.audit_query(0, 0, None, None).is_err());
    assert!(service.audit_query(201, 0, None, None).is_err());
    assert_eq!(service.audit_query(200, 0, None, None).expect("200").len(), 4);
    drop(service);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn purge_respects_the_threshold_and_the_batch_cap() {
    let dir = temp_dir("purge");
    let path = dir.join("node.db");
    let mut service = NodeService::open(&path, config()).expect("open");
    let now = 10 * AUDIT_DEFAULT_RETENTION_MS;
    // 800 条老事件（超过单批上限 512）+ 2 条新事件。
    for index in 0..800u64 {
        service
            .record_audit_persisted(event("http", index, None))
            .expect("persist old");
    }
    service
        .record_audit_persisted(event("http", now, None))
        .expect("persist new");
    service
        .record_audit_persisted(event("http", now + 1, None))
        .expect("persist newer");
    let threshold = now.saturating_sub(AUDIT_DEFAULT_RETENTION_MS);
    // 单批上限 512：第一次最多删 512 条。
    let first = service.purge_audit(threshold, 10_000).expect("purge");
    assert_eq!(first, AUDIT_PURGE_MAX_BATCH as u64, "batch cap is enforced");
    assert_eq!(service.audit_persisted_count().expect("count"), 802 - 512);
    // 再删一轮，直到老事件清空；新事件（>= 阈值）必须保留。
    let second = service.purge_audit(threshold, 10_000).expect("purge 2");
    assert_eq!(second, 288);
    assert_eq!(service.audit_persisted_count().expect("count"), 2);
    let remaining = service.audit_query(10, 0, None, None).expect("query");
    assert!(remaining.iter().all(|event| event.at_ms >= threshold));
    drop(service);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn disabled_audit_neither_writes_nor_reads() {
    let dir = temp_dir("disabled");
    let path = dir.join("node.db");
    let mut service = NodeService::open(&path, config()).expect("open");
    service
        .record_audit_persisted(event("http", 1, None))
        .expect("persist before disabling");
    service.set_audit_enabled(false).expect("disable");
    // 关闭后：不写库。
    service
        .record_audit_persisted(event("http", 2, None))
        .expect("no-op write");
    // 关闭后：不读库（返回空，且计数为 0）。
    assert_eq!(service.audit_query(10, 0, None, None).expect("query").len(), 0);
    assert_eq!(service.audit_persisted_count().expect("count"), 0);
    // 内存环里关闭前的记录仍然可读（既有语义：关闭只影响后续记录），但**落库查询**已经为空。
    let (tail, source) = service.audit_tail_with_source(10).expect("tail");
    assert_eq!(source, AuditSource::Memory, "memory still holds the pre-disable event");
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].at_ms, 1);
    // 重新打开后，关闭期间的事件确实不存在（没有被偷偷写进去）。
    service.set_audit_enabled(true).expect("enable");
    let events = service.audit_query(10, 0, None, None).expect("query");
    assert_eq!(events.len(), 1, "only the pre-disable event exists: {events:?}");
    assert_eq!(events[0].at_ms, 1);
    drop(service);
    let _ = fs::remove_dir_all(&dir);
}

