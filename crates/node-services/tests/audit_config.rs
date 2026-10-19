//! NODE-018：启动前配置校验（不建库）+ 审计日志（字段/上限/开关）。
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use chat_node_services::audit::{AUDIT_CAPACITY, AuditEvent, AuditOutcome};
use chat_node_services::config_validation::NodeConfigValidation;
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};

fn temp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "chat-node-audit-{tag}-{}-{counter}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn config(node_id: &str) -> NodeServiceConfig {
    NodeServiceConfig {
        node_id: node_id.to_owned(),
        signing_seed: [9u8; 32],
        policy: NodeAuthPolicy::default(),
    }
}

#[test]
fn invalid_config_is_rejected_before_the_database_is_created() {
    let dir = temp_dir("invalid");
    let path = dir.join("node.db");
    // node_id 带空格 → INVALID_CONFIG，且**不创建库文件**。
    let error = NodeService::open(&path, config("bad node id"))
        .expect_err("invalid config must not start the node");
    assert_eq!(error.code(), "INVALID_CONFIG");
    assert!(!path.exists(), "a rejected config must not create the database");
    // 全零签名种子同样被拒。
    let mut zero_seed = config("node-audit-ok");
    zero_seed.signing_seed = [0u8; 32];
    let error = NodeService::open(&path, zero_seed).expect_err("zero seed");
    assert_eq!(error.code(), "INVALID_CONFIG");
    assert!(!path.exists(), "still no database file");
    // 合法配置才真正建库。
    let service = NodeService::open(&path, config("node-audit-ok")).expect("valid config starts");
    assert!(path.exists());
    assert_eq!(service.node_id(), "node-audit-ok");
    drop(service);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn config_validation_catches_policy_range_problems() {
    let mut broken = config("node-policy");
    broken.policy.capability.max_ttl_seconds = 0;
    assert_eq!(
        NodeConfigValidation::validate(&broken)
            .expect_err("ttl")
            .code(),
        "INVALID_CONFIG"
    );
    let mut enforced = config("node-enforced");
    enforced.policy.capability.required = true;
    enforced.policy.capability.max_ttl_seconds = 5;
    assert_eq!(
        NodeConfigValidation::validate(&enforced)
            .expect_err("enforced ttl")
            .code(),
        "INVALID_CONFIG"
    );
}

#[test]
fn audit_events_carry_metadata_and_the_ring_is_bounded() {
    let mut service = NodeService::open_in_memory(config("node-audit-ring")).expect("service");
    assert!(service.audit_enabled());
    assert_eq!(service.audit_len(), 0);
    service.record_audit(
        AuditEvent::new("capability_revoke", 1_000, AuditOutcome::Denied)
            .with_device("dev:1:abc")
            .with_group("grp:1:def")
            .with_code("PERMISSION_DENIED")
            .with_request_id("req-1"),
    );
    let tail = service.audit_tail(10);
    assert_eq!(tail.len(), 1);
    let event = &tail[0];
    assert_eq!(event.kind, "capability_revoke");
    assert_eq!(event.at_ms, 1_000);
    assert_eq!(event.device_id.as_deref(), Some("dev:1:abc"));
    assert_eq!(event.group_id.as_deref(), Some("grp:1:def"));
    assert_eq!(event.outcome, AuditOutcome::Denied);
    assert_eq!(event.code, Some("PERMISSION_DENIED"));
    assert_eq!(event.request_id.as_deref(), Some("req-1"));
    // 上限：写入超过容量后只保留最近 AUDIT_CAPACITY 条，且丢最旧的。
    for index in 0..(AUDIT_CAPACITY as u64 + 20) {
        service.record_audit(AuditEvent::new("http", index, AuditOutcome::Ok));
    }
    assert_eq!(service.audit_len(), AUDIT_CAPACITY);
    let tail = service.audit_tail(AUDIT_CAPACITY);
    assert_eq!(tail.len(), AUDIT_CAPACITY);
    assert_eq!(
        tail.last().expect("last").at_ms,
        AUDIT_CAPACITY as u64 + 19,
        "the newest event survives"
    );
    // tail(limit) 只返回最近 limit 条。
    assert_eq!(service.audit_tail(3).len(), 3);
}

#[test]
fn audit_can_be_disabled_and_relaxed_revocation_requires_it() {
    let mut service = NodeService::open_in_memory(config("node-audit-off")).expect("service");
    service.record_audit(AuditEvent::new("http", 1, AuditOutcome::Ok));
    assert_eq!(service.audit_len(), 1);
    service.set_audit_enabled(false).expect("default policy allows disabling audit");
    assert!(!service.audit_enabled());
    service.record_audit(AuditEvent::new("http", 2, AuditOutcome::Ok));
    assert_eq!(service.audit_len(), 1, "disabled audit records nothing");
    assert_eq!(service.audit_tail(10).len(), 1, "existing records remain readable");
    // 放开跨设备撤销时禁止关闭审计（否则没有追责链路）。
    let mut relaxed = config("node-audit-relaxed");
    relaxed.policy.capability.revoke.self_only = false;
    let mut service = NodeService::open_in_memory(relaxed).expect("service");
    let error = service
        .set_audit_enabled(false)
        .expect_err("relaxed revocation needs the audit trail");
    assert_eq!(error.code(), "INVALID_CONFIG");
    assert!(service.audit_enabled(), "audit stays on");
}

