//! CORE-003/004 的最小事件流 + CORE-010 的发送状态服务（service 层包装）。
//!
//! 设计要点：
//!
//! - **事件不是可靠消息日志**：每次写入提交后才发布“变了什么”，调用方必须用
//!   [`crate::service::draft_status_view`] / `list_*` 重查真实状态；
//! - **有界环形缓冲 + 单调序号**：[`EventBus`] 只保留最近 `capacity` 条事件，序号从 1
//!   单调递增；`poll_events(since_seq, limit)` 返回 `since_seq` 之后的事件；
//! - **溢出显式 resync**：调用方的游标早于缓冲起点（事件已丢）或超前于本实例时，
//!   返回 `resync_required = true` 与一条 [`CoreEvent::ResyncRequired`]，绝不静默丢事件；
//! - **重复 poll 幂等**：poll 不改变任何状态，同参数重复调用返回同一批事件；
//! - **事件不含正文/密钥/指纹**：[`CoreEvent`] 的字段只有 ID 与稳定状态文本。
//!
//! 写入路径的接线方式：**不改** [`crate::send_orchestrator`]（保持它是纯编排器），
//! 由 [`SendService`] 在 service 层包装 `begin_send`/`prepare`/`dispatch`/
//! `retry_draft`/`cancel_draft`/`reconcile`：调用成功后再查询真实状态，状态**变化**时
//! 才发布 `DraftChanged` + `ConversationChanged`（不会因为幂等重放反复刷事件）。

use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use chat_storage::formal::Store;
use chat_sync::clock::Clock;

use crate::send_orchestrator::{
    CancelOutcome, CryptoEngine, DispatchOutcome, NodeSubmitter, PrepareOutcome, ReconcileReport,
    SendOrchestrator,
};
use crate::service::{CoreError, DraftStatusView};

/// 默认事件缓冲容量。
pub const DEFAULT_EVENT_CAPACITY: usize = 256;
/// 事件缓冲容量上限（有界拒绝）。
pub const MAX_EVENT_CAPACITY: usize = 4096;
/// 单次 poll 的事件条数上限。
pub const MAX_EVENT_BATCH: u32 = 256;

/// 内核变更事件：只含 ID 与稳定状态文本，不含正文、密钥或指纹。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreEvent {
    /// 草稿状态变化（`status` 取值同 [`DraftStatusView::status`]）。
    DraftChanged {
        /// 本地草稿 ID。
        draft_id: String,
        /// 所属会话 ID。
        conversation_id: String,
        /// 稳定状态文本。
        status: String,
        /// 最新尝试的 SubmissionId（尚无尝试时为 `None`）。
        submission_id: Option<String>,
    },
    /// 会话列表可能需要刷新（摘要/活动时间变化）。
    ConversationChanged {
        /// 会话 ID。
        conversation_id: String,
    },
    /// 事件缓冲溢出或游标不可信：调用方必须整体重查（不是增量）。
    ResyncRequired {
        /// 静态原因（不含用户内容）。
        reason: String,
    },
    /// 内核已关闭（最后一个事件）。
    CoreClosed,
}

/// 一页事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventBatch {
    /// 本批事件（按序号升序；resync 时只有一条 `ResyncRequired`）。
    pub events: Vec<CoreEvent>,
    /// 下一次 poll 应传入的游标（本批最后一条事件的序号；无新事件时等于入参）。
    pub next_seq: u64,
    /// 是否要求整体重查（此时本批不含增量事件）。
    pub resync_required: bool,
}

/// 事件总线的输入/容量错误（只有两种，均为有界拒绝）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventError {
    /// 容量不在 `1..=MAX_EVENT_CAPACITY`。
    InvalidCapacity,
    /// 单次 poll 的 limit 不在 `1..=MAX_EVENT_BATCH`。
    InvalidLimit,
}

impl EventError {
    /// 稳定文本码（供 error_code 透传）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidCapacity => "INVALID_EVENT_CAPACITY",
            Self::InvalidLimit => "INVALID_EVENT_LIMIT",
        }
    }
}

impl fmt::Display for EventError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::error::Error for EventError {}

/// 有界事件环形缓冲 + 单调序号。
#[derive(Debug)]
pub struct EventBus {
    capacity: usize,
    next_seq: u64,
    events: VecDeque<(u64, CoreEvent)>,
}

impl EventBus {
    /// 按容量构造（`1..=MAX_EVENT_CAPACITY`，越界显式拒绝）。
    pub fn new(capacity: usize) -> Result<Self, EventError> {
        if !(1..=MAX_EVENT_CAPACITY).contains(&capacity) {
            return Err(EventError::InvalidCapacity);
        }
        Ok(Self {
            capacity,
            next_seq: 1,
            events: VecDeque::with_capacity(capacity),
        })
    }

    /// 缓冲容量。
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// 已发布的最大序号（0 = 尚无事件）。
    pub fn latest_seq(&self) -> u64 {
        self.next_seq.saturating_sub(1)
    }

    /// 当前缓冲条数。
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// 缓冲是否为空。
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// 发布一条事件，返回其序号（单调递增，从 1 开始）。溢出的旧事件被丢弃，
    /// 之后的 poll 会以 `ResyncRequired` 显式告知调用方。
    pub fn push(&mut self, event: CoreEvent) -> u64 {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        self.events.push_back((seq, event));
        while self.events.len() > self.capacity {
            self.events.pop_front();
        }
        seq
    }

    /// 读取 `since_seq` 之后的增量事件。
    ///
    /// - `since_seq = 0` 表示“从头”；`next_seq` 是本批最后一条的序号（没有新事件时原样返回）；
    /// - 游标早于缓冲起点（事件已丢）或超前于本实例时返回 `resync_required = true`；
    /// - 只读：重复调用同样参数返回同样的结果（幂等）。
    pub fn poll(&self, since_seq: u64, limit: u32) -> Result<EventBatch, EventError> {
        if !(1..=MAX_EVENT_BATCH).contains(&limit) {
            return Err(EventError::InvalidLimit);
        }
        let latest = self.latest_seq();
        if since_seq > latest {
            return Ok(self.resync("event cursor is ahead of this core's event sequence", latest));
        }
        let first_available = self
            .events
            .front()
            .map(|(seq, _)| *seq)
            .unwrap_or(self.next_seq);
        if since_seq.saturating_add(1) < first_available {
            return Ok(self.resync("event buffer overflowed before this cursor", latest));
        }
        let capacity = limit as usize;
        let mut events = Vec::new();
        let mut next_seq = since_seq;
        for (seq, event) in &self.events {
            if *seq <= since_seq {
                continue;
            }
            if events.len() == capacity {
                break;
            }
            events.push(event.clone());
            next_seq = *seq;
        }
        Ok(EventBatch {
            events,
            next_seq,
            resync_required: false,
        })
    }

    fn resync(&self, reason: &str, latest: u64) -> EventBatch {
        EventBatch {
            events: vec![CoreEvent::ResyncRequired {
                reason: reason.to_owned(),
            }],
            next_seq: latest,
            resync_required: true,
        }
    }
}

/// 发送服务：在 service 层包装 [`SendOrchestrator`] 并把状态变化发布成事件。
///
/// 存储是单写者独占打开，因此本对象拥有编排器（编排器拥有 `Store`）；同一个库不要
/// 同时再开 `ConversationCore`。
pub struct SendService<S: NodeSubmitter, C: CryptoEngine, K: Clock> {
    orchestrator: Option<SendOrchestrator<S, C, K>>,
    events: EventBus,
    last_status: BTreeMap<String, String>,
}

impl<S: NodeSubmitter, C: CryptoEngine, K: Clock> fmt::Debug for SendService<S, C, K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SendService")
            .field("open", &self.is_open())
            .field("events", &self.events.len())
            .field("latest_seq", &self.events.latest_seq())
            .finish_non_exhaustive()
    }
}

impl<S: NodeSubmitter, C: CryptoEngine, K: Clock> SendService<S, C, K> {
    /// 用默认事件容量构造。
    pub fn new(
        store: Store,
        submitter: S,
        engine: C,
        clock: K,
        worker: &str,
    ) -> anyhow::Result<Self> {
        Self::with_event_capacity(store, submitter, engine, clock, worker, DEFAULT_EVENT_CAPACITY)
    }

    /// 指定事件容量构造（`1..=MAX_EVENT_CAPACITY`）。
    pub fn with_event_capacity(
        store: Store,
        submitter: S,
        engine: C,
        clock: K,
        worker: &str,
        event_capacity: usize,
    ) -> anyhow::Result<Self> {
        let events = EventBus::new(event_capacity).map_err(anyhow::Error::new)?;
        let orchestrator =
            SendOrchestrator::new(store, submitter, engine, clock, worker).map_err(anyhow::Error::new)?;
        Ok(Self {
            orchestrator: Some(orchestrator),
            events,
            last_status: BTreeMap::new(),
        })
    }

    /// 是否仍处于打开状态。
    pub fn is_open(&self) -> bool {
        self.orchestrator.is_some()
    }

    /// 事件总线（只读；`poll_events` 是推荐入口）。
    pub fn events(&self) -> &EventBus {
        &self.events
    }

    /// 读取增量事件（重复调用幂等；溢出返回 `resync_required = true`）。
    pub fn poll_events(&self, since_seq: u64, limit: u32) -> anyhow::Result<EventBatch> {
        self.events.poll(since_seq, limit).map_err(anyhow::Error::new)
    }

    /// CORE-010：草稿真实状态（无正文）；未知草稿返回 `None`。
    pub fn get_draft_status(&self, draft_id: &str) -> anyhow::Result<Option<DraftStatusView>> {
        match crate::service::draft_status_view(self.store()?, draft_id) {
            Ok(view) => Ok(view),
            Err(error) => Err(anyhow::Error::new(error)),
        }
    }

    /// 只读存储句柄（诊断/断言用）。
    pub fn store(&self) -> anyhow::Result<&Store> {
        match &self.orchestrator {
            Some(orchestrator) => Ok(orchestrator.store()),
            None => Err(anyhow::Error::new(CoreError::Closed)),
        }
    }

    /// 可变存储句柄（outbox 领取、恢复等上层集成用）。
    pub fn store_mut(&mut self) -> anyhow::Result<&mut Store> {
        match self.orchestrator.as_mut() {
            Some(orchestrator) => Ok(orchestrator.store_mut()),
            None => Err(anyhow::Error::new(CoreError::Closed)),
        }
    }

    /// 只读编排器（诊断用）。
    pub fn orchestrator(&self) -> anyhow::Result<&SendOrchestrator<S, C, K>> {
        self.orchestrator
            .as_ref()
            .ok_or_else(|| anyhow::Error::new(CoreError::Closed))
    }

    /// 可变编排器（时间推进、租约观测等）。
    pub fn orchestrator_mut(&mut self) -> anyhow::Result<&mut SendOrchestrator<S, C, K>> {
        self.orchestrator
            .as_mut()
            .ok_or_else(|| anyhow::Error::new(CoreError::Closed))
    }

    /// CORE-007：建意图（成功后发布 `queued` 事件）。
    pub fn begin_send(
        &mut self,
        conversation_id: &str,
        operation_id: &str,
        body: &str,
    ) -> anyhow::Result<String> {
        let draft_id = match self.orchestrator.as_mut() {
            Some(orchestrator) => orchestrator
                .begin_send(conversation_id, operation_id, body)
                .map_err(anyhow::Error::new)?,
            None => return Err(anyhow::Error::new(CoreError::Closed)),
        };
        self.publish_status(&draft_id);
        Ok(draft_id)
    }

    /// SYNC-003 执行侧：准备一次加密尝试（状态变化时发布事件）。
    pub fn prepare(&mut self, draft_id: &str) -> anyhow::Result<PrepareOutcome> {
        let outcome = match self.orchestrator.as_mut() {
            Some(orchestrator) => orchestrator
                .prepare(draft_id)
                .map_err(anyhow::Error::new)?,
            None => return Err(anyhow::Error::new(CoreError::Closed)),
        };
        self.publish_status(draft_id);
        Ok(outcome)
    }

    /// SYNC-004 执行侧：提交并落库（状态变化时发布事件）。
    pub fn dispatch(&mut self, draft_id: &str) -> anyhow::Result<DispatchOutcome> {
        let outcome = match self.orchestrator.as_mut() {
            Some(orchestrator) => orchestrator
                .dispatch(draft_id)
                .map_err(anyhow::Error::new)?,
            None => return Err(anyhow::Error::new(CoreError::Closed)),
        };
        self.publish_status(draft_id);
        Ok(outcome)
    }

    /// CORE-008：显式重试（状态变化时发布事件）。
    pub fn retry_draft(&mut self, draft_id: &str) -> anyhow::Result<PrepareOutcome> {
        let outcome = match self.orchestrator.as_mut() {
            Some(orchestrator) => orchestrator
                .retry_draft(draft_id)
                .map_err(anyhow::Error::new)?,
            None => return Err(anyhow::Error::new(CoreError::Closed)),
        };
        self.publish_status(draft_id);
        Ok(outcome)
    }

    /// CORE-009：取消（状态变化时发布事件）。
    pub fn cancel_draft(&mut self, draft_id: &str) -> anyhow::Result<CancelOutcome> {
        let outcome = match self.orchestrator.as_mut() {
            Some(orchestrator) => orchestrator
                .cancel_draft(draft_id)
                .map_err(anyhow::Error::new)?,
            None => return Err(anyhow::Error::new(CoreError::Closed)),
        };
        self.publish_status(draft_id);
        Ok(outcome)
    }

    /// outbox 结果对账（修复草稿状态时发布事件）。
    pub fn reconcile(&mut self, draft_id: &str) -> anyhow::Result<ReconcileReport> {
        let report = match self.orchestrator.as_mut() {
            Some(orchestrator) => orchestrator
                .reconcile(draft_id)
                .map_err(anyhow::Error::new)?,
            None => return Err(anyhow::Error::new(CoreError::Closed)),
        };
        self.publish_status(draft_id);
        Ok(report)
    }

    /// 关闭：丢弃编排器与独占存储句柄，并发布 `CoreClosed`；之后所有调用显式
    /// `CORE_CLOSED`。重复调用幂等（不会重复发布事件）。
    pub fn shutdown(&mut self) {
        if self.orchestrator.take().is_some() {
            self.events.push(CoreEvent::CoreClosed);
        }
    }

    /// 查询当前状态，仅当有效状态**变化**时才发布事件（幂等重放不刷事件）。
    fn publish_status(&mut self, draft_id: &str) {
        let Ok(store) = self.store() else {
            return;
        };
        let Ok(Some(view)) = crate::service::draft_status_view(store, draft_id) else {
            return;
        };
        let unchanged = self
            .last_status
            .get(&view.draft_id)
            .is_some_and(|status| status == &view.status);
        if unchanged {
            return;
        }
        self.last_status
            .insert(view.draft_id.clone(), view.status.clone());
        self.events.push(CoreEvent::DraftChanged {
            draft_id: view.draft_id,
            conversation_id: view.conversation_id.clone(),
            status: view.status,
            submission_id: view.submission_id,
        });
        self.events.push(CoreEvent::ConversationChanged {
            conversation_id: view.conversation_id,
        });
    }
}
