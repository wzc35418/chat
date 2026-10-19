//! 结构化审计日志（内存环形，≤1024 条；不含正文/密钥）。
//!
//! 语义：
//!
//! - 每条事件只有**元数据**：种类、时间、设备/群（可选）、结果、稳定错误码（可选）、请求标识（可选）；
//!   绝不记录正文、令牌、密钥或凭据材料；
//! - 环形缓冲：超过容量丢最旧的；`tail(limit)` 只返回最近 `limit` 条（`limit` 被夹到容量）；
//! - 可在运行期关闭（`set_enabled(false)`）：关闭后不再记录（既有记录保留、可继续读取）。
use std::collections::VecDeque;

/// 审计事件容量上限。
pub const AUDIT_CAPACITY: usize = 1024;
/// 审计保留期默认值（7 天，毫秒）。
pub const AUDIT_DEFAULT_RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
/// 审计查询条数上限。
pub const AUDIT_QUERY_MAX_LIMIT: u32 = 200;
/// 单次清理的最大条数。
pub const AUDIT_PURGE_MAX_BATCH: usize = 512;

/// 本次 `audit_tail` 读取的数据来源（便于调用方标注“内存/落库”）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditSource {
    /// 内存环形缓冲。
    Memory,
    /// 持久化表（内存为空时的回退读）。
    Persisted,
}

/// 审计结果分类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditOutcome {
    /// 操作成功。
    Ok,
    /// 被拒绝（鉴权/权限/撤销/限流等）。
    Denied,
    /// 执行失败（存储/内部错误）。
    Failed,
}

impl AuditOutcome {
    /// 稳定文本（写进审计与日志）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Denied => "denied",
            Self::Failed => "failed",
        }
    }
}

/// 一条审计事件（只含元数据）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditEvent {
    /// 事件种类（例如 `http` / `capability_revoke`）。
    pub kind: String,
    /// 事件时间（Unix 毫秒）。
    pub at_ms: u64,
    /// 相关设备（匿名端点可以是 None）。
    pub device_id: Option<String>,
    /// 相关群（群内操作才有）。
    pub group_id: Option<String>,
    /// 结果分类。
    pub outcome: AuditOutcome,
    /// 稳定错误码（成功时为 None）。
    pub code: Option<&'static str>,
    /// 请求标识（HTTP 层注入，用于贯通响应头与审计）。
    pub request_id: Option<String>,
}

impl AuditEvent {
    /// 构造一条只含必需字段的事件（其余为 None）。
    pub fn new(kind: impl Into<String>, at_ms: u64, outcome: AuditOutcome) -> Self {
        Self {
            kind: kind.into(),
            at_ms,
            device_id: None,
            group_id: None,
            outcome,
            code: None,
            request_id: None,
        }
    }

    /// 附带设备标识。
    pub fn with_device(mut self, device_id: impl Into<String>) -> Self {
        self.device_id = Some(device_id.into());
        self
    }

    /// 附带群标识。
    pub fn with_group(mut self, group_id: impl Into<String>) -> Self {
        self.group_id = Some(group_id.into());
        self
    }

    /// 附带稳定错误码。
    pub fn with_code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }

    /// 附带请求标识。
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }
}

/// 内存环形审计日志。
#[derive(Clone, Debug)]
pub struct AuditLog {
    capacity: usize,
    enabled: bool,
    events: VecDeque<AuditEvent>,
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new(AUDIT_CAPACITY)
    }
}

impl AuditLog {
    /// 以给定容量创建（1..=1024，越界夹到上限/下限）。
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.clamp(1, AUDIT_CAPACITY),
            enabled: true,
            events: VecDeque::new(),
        }
    }

    /// 容量。
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// 是否记录新事件。
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// 打开/关闭记录（关闭不清空既有记录）。
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// 当前条数。
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// 记录一条事件（关闭时丢弃；超容量丢最旧的）。
    pub fn record(&mut self, event: AuditEvent) {
        if !self.enabled {
            return;
        }
        if self.events.len() == self.capacity {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    /// 最近 `limit` 条（最旧的在前；`limit = 0` 返回空）。
    pub fn tail(&self, limit: usize) -> Vec<AuditEvent> {
        if limit == 0 {
            return Vec::new();
        }
        let take = limit.min(self.events.len());
        self.events
            .iter()
            .skip(self.events.len() - take)
            .cloned()
            .collect()
    }

    /// 清空（保留容量与开关）。
    pub fn clear(&mut self) {
        self.events.clear();
    }
}

/// 把数据库里的错误码字符串驻留成 `&'static str`。
///
/// 常用码只有几十个（`NodeError::code()` 的全集），因此驻留表大小有界：
/// 同一个码只泄漏一次，之后复用同一份静态字符串；不会随事件数量增长。
pub fn intern_code(value: &str) -> &'static str {
    use std::sync::{Mutex, OnceLock};
    static INTERNED: OnceLock<Mutex<std::collections::HashSet<&'static str>>> = OnceLock::new();
    let table = INTERNED.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    let mut guard = match table.lock() {
        Ok(guard) => guard,
        // 锁中毒时退化为一次性泄漏（不影响正确性，只会多一份副本）。
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(existing) = guard.get(value) {
        return existing;
    }
    let leaked: &'static str = Box::leak(value.to_owned().into_boxed_str());
    guard.insert(leaked);
    leaked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_the_newest_events_and_respects_the_capacity() {
        let mut log = AuditLog::new(3);
        for index in 0..5u64 {
            log.record(AuditEvent::new("http", index, AuditOutcome::Ok));
        }
        assert_eq!(log.len(), 3, "capacity is enforced");
        let tail = log.tail(10);
        assert_eq!(tail.len(), 3);
        assert_eq!(tail[0].at_ms, 2, "the oldest events are dropped");
        assert_eq!(tail[2].at_ms, 4);
        assert_eq!(log.tail(1)[0].at_ms, 4);
        assert!(log.tail(0).is_empty());
    }

    #[test]
    fn disabled_log_records_nothing_but_keeps_existing_events() {
        let mut log = AuditLog::default();
        log.record(AuditEvent::new("http", 1, AuditOutcome::Ok));
        log.set_enabled(false);
        log.record(AuditEvent::new("http", 2, AuditOutcome::Denied));
        assert_eq!(log.len(), 1, "disabled logs ignore new events");
        assert!(!log.enabled());
        log.set_enabled(true);
        log.record(AuditEvent::new("http", 3, AuditOutcome::Failed));
        assert_eq!(log.tail(5).last().expect("last").at_ms, 3);
    }

    #[test]
    fn events_carry_only_metadata() {
        let event = AuditEvent::new("capability_revoke", 10, AuditOutcome::Denied)
            .with_device("dev:1:abc")
            .with_group("grp:1:def")
            .with_code("PERMISSION_DENIED")
            .with_request_id("req-1");
        assert_eq!(event.kind, "capability_revoke");
        assert_eq!(event.device_id.as_deref(), Some("dev:1:abc"));
        assert_eq!(event.group_id.as_deref(), Some("grp:1:def"));
        assert_eq!(event.code, Some("PERMISSION_DENIED"));
        assert_eq!(event.request_id.as_deref(), Some("req-1"));
        assert_eq!(event.outcome.as_str(), "denied");
    }
}

