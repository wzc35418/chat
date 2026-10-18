//! SYNC-007 `consume_mailbox_page` 与信箱轮询状态机。
//!
//! 本层只负责**顺序、去重、预算与 ACK 记账**，不验证 envelope 签名/MLS/权限：
//! 那些属于 CRYPTO/STORAGE 的正式路径（本轮未接入）。因此这里的
//! “已处理”只表示“已进入本机耐久序列/暂存”，从不表示“已到达用户”。
//!
//! 硬性语义：
//!
//! - **游标只前进**：页面 `next.position` 回退、或非空页没有推进游标，都返回
//!   `INVALID_CURSOR`；条目位置必须严格递增，落在游标之后的条目只有在“同
//!   item id 已消费”时才容忍（作为重复），否则同样拒绝。
//! - **同 item id 幂等去重**：id 已在保留窗口内且内容指纹相同 → 记为重复、不重复
//!   投递；id 相同但内容不同 → `INVALID_CANDIDATE`（明确冲突，绝不静默覆盖）。
//! - **乱序容忍**：群日志条目按 `group_seq` 连续推进；跳号条目先**耐久暂存**
//!   （同时 ACK，避免节点重发），缺号到达后按序提升投递。群日志起点默认 1；
//!   已加入设备应先用 SYNC-008 把 verified head 通过
//!   [`MailboxPoller::seed_group`] 对齐（本层不推断 joined_seq）。
//! - **双预算**：单页条目数、单页总字节、单条字节、暂存条数、去重窗口、待 ACK
//!   集合都有上限，越界显式 `CONTENT_TOO_LARGE` / `MISSING_LOG_ENTRIES`，不截断。
//! - **ACK 失败不丢进度**：消费成功先推进游标并记账，ACK 失败只保留待 ACK 集合，
//!   下一轮先重试 ACK（并退避），游标不回退、条目不重投。
//! - **空页不空转**：空页退避 [`EMPTY_PAGE_BACKOFF_MS`]，非空页之间至少间隔
//!   [`PAGE_INTERVAL_MS`]；拉取失败按指数退避（上限
//!   [`PULL_FAILURE_MAX_BACKOFF_MS`]）；时间未到时不调用数据源（[`PollStatus::Idle`]）。
//!
//! 全部失败都是冻结码（PROTOCOL-014），且失败不改变游标/暂存/待 ACK 状态
//! （消费路径先用快照，任何失败整体回滚）。

use core::fmt;
use std::collections::{BTreeMap, VecDeque};

use sha2::{Digest, Sha256};

use crate::clock::Clock;
use crate::error::{
    ErrorCode, Result, content_too_large, invalid_candidate, invalid_cursor, invalid_id,
    invalid_state, invalid_target, missing_log_entries,
};
use crate::ids::{MAX_SYNC_ID_BYTES, validate_id};

/// 单页条目数上限。
pub const MAX_MAILBOX_PAGE_ITEMS: u32 = 100;
/// 单页 envelope 总字节上限。
pub const MAX_MAILBOX_PAGE_BYTES: usize = 1024 * 1024;
/// 单条 envelope 字节上限。
pub const MAX_MAILBOX_ITEM_BYTES: usize = 64 * 1024;
/// 游标不透明令牌长度上限。
pub const MAX_CURSOR_TOKEN_BYTES: usize = 1024;
/// 去重窗口保留的 item id 数。
pub const MAX_TRACKED_ITEM_IDS: usize = 4096;
/// 乱序暂存条目上限。
pub const MAX_HELD_ITEMS: usize = 512;
/// 跟踪的群数上限。
pub const MAX_TRACKED_GROUPS: usize = 256;
/// 待 ACK 集合上限。
pub const MAX_PENDING_ACK: usize = 4096;
/// 非空页之间的最小拉取间隔。
pub const PAGE_INTERVAL_MS: i64 = 1_000;
/// 空页退避（避免空转轮询）。
pub const EMPTY_PAGE_BACKOFF_MS: i64 = 30_000;
/// 拉取失败的基础退避。
pub const PULL_FAILURE_BASE_BACKOFF_MS: i64 = 1_000;
/// 拉取失败的退避上限。
pub const PULL_FAILURE_MAX_BACKOFF_MS: i64 = 60_000;
/// ACK 失败后的重试间隔。
pub const ACK_FAILURE_BACKOFF_MS: i64 = 1_000;

/// 邮箱条目 ID（节点侧不透明 ID，非可信消息去重键）。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MailboxItemId(String);

impl MailboxItemId {
    /// 解析并校验条目 ID。
    pub fn parse(text: impl Into<String>) -> Result<Self> {
        let text = text.into();
        if text.len() > MAX_SYNC_ID_BYTES {
            return Err(invalid_id("mailbox item ID exceeds the ID budget"));
        }
        validate_id("mailbox item", &text)?;
        Ok(Self(text))
    }

    /// 文本形式。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MailboxItemId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// 信箱游标：单调位置 + 不透明令牌。
///
/// 位置只用于本层单调性校验；真实节点游标语义（签名/过期）由 TRANSPORT-009 负责，
/// 本层不解析令牌内容。MailboxCursor 与群 verified cursor 是不同类型，不能互换。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailboxCursor {
    position: u64,
    token: String,
}

impl MailboxCursor {
    /// 初始游标（位置 0，无令牌）。
    pub fn initial() -> Self {
        Self {
            position: 0,
            token: String::new(),
        }
    }

    /// 由服务端给出的位置与令牌构造（有界校验）。
    pub fn from_parts(position: u64, token: impl Into<String>) -> Result<Self> {
        let token = token.into();
        if token.len() > MAX_CURSOR_TOKEN_BYTES {
            return Err(invalid_cursor("mailbox cursor token exceeds the token budget"));
        }
        if token.chars().any(char::is_control) {
            return Err(invalid_cursor("mailbox cursor token contains control characters"));
        }
        Ok(Self { position, token })
    }

    /// 单调位置。
    pub const fn position(&self) -> u64 {
        self.position
    }

    /// 不透明令牌（可为空）。
    pub fn token(&self) -> &str {
        &self.token
    }
}

/// 邮箱条目类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MailboxItemKind {
    /// 加入群所需的 Welcome。
    Welcome,
    /// 群日志条目（按 group_seq 连续）。
    LogEntry,
    /// 投递/已读回执。
    Receipt,
    /// 本层不认识的类别（保留但不解释，不能当作正式消息）。
    Unknown,
}

/// 一条邮箱条目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailboxItem {
    /// 节点侧条目 ID。
    pub item_id: MailboxItemId,
    /// 节点分配的服务端位置（严格递增）。
    pub position: u64,
    /// 条目类别。
    pub kind: MailboxItemKind,
    /// 群 ID（LogEntry 必填）。
    pub group_id: Option<String>,
    /// 群序号（LogEntry 必填，≥1）。
    pub group_seq: Option<u64>,
    /// 不透明信封字节（本层不解密）。
    pub envelope: Vec<u8>,
}

/// 一页邮箱条目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailboxPage {
    /// 本页条目（位置严格递增）。
    pub items: Vec<MailboxItem>,
    /// 下一页游标（位置只能前进）。
    pub next: MailboxCursor,
}

/// 信箱数据源（TRANSPORT-009 `pull_mailbox` 的端口）。
pub trait MailboxSource {
    /// 从游标之后拉取一页，`limit` 为本层给出的条目上限。
    fn pull(&mut self, cursor: &MailboxCursor, limit: u32) -> Result<MailboxPage>;
}

/// ACK 汇（TRANSPORT-010 `ack_mailbox` 的端口）。
pub trait AckSink {
    /// 确认这些条目已被安全处理/耐久暂存；失败表示 ACK 尚未送达。
    fn ack(&mut self, item_ids: &[MailboxItemId]) -> Result<()>;
}

/// 一次 tick 的结果类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollStatus {
    /// 退避尚未到期：本次不拉取、不 ACK。
    Idle,
    /// 正常完成一次「补 ACK + 拉取一页 + 消费」。
    Polled,
    /// 拉取失败（游标未动，按退避重试）。
    PullFailed,
    /// ACK 失败：待 ACK 集合保留，下次重试。
    AckDeferred,
    /// 待 ACK 集合已满：停止拉取直到 ACK 成功（背压）。
    Backpressure,
}

/// tick 结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PollOutcome {
    /// 结果类别。
    pub status: PollStatus,
    /// 本页收到的条目数。
    pub received: u32,
    /// 本次按序可应用（交付上层）的条目。
    pub delivered: Vec<MailboxItem>,
    /// 本次进入暂存的乱序条目数。
    pub held: u32,
    /// 被识别为重复（或过旧）而跳过的条目数。
    pub duplicates: u32,
    /// 本次成功 ACK 的条数。
    pub acked: u32,
    /// 仍待 ACK 的条数。
    pub pending_ack: u32,
    /// 当前游标。
    pub cursor: MailboxCursor,
    /// 下一次允许轮询的时间。
    pub next_poll_at_ms: i64,
    /// 失败时的冻结码。
    pub error: Option<ErrorCode>,
}

#[derive(Clone, Debug)]
struct HeldItem {
    group_id: String,
    group_seq: u64,
    item: MailboxItem,
}

#[derive(Clone, Debug, Default)]
struct Counts {
    received: u32,
    held: u32,
    duplicates: u32,
}

/// 信箱轮询状态机。
#[derive(Clone, Debug)]
pub struct MailboxPoller {
    cursor: MailboxCursor,
    consumed: VecDeque<(MailboxItemId, u64)>,
    pending_ack: Vec<MailboxItemId>,
    groups: BTreeMap<String, u64>,
    held: Vec<HeldItem>,
    next_poll_at_ms: i64,
    consecutive_pull_failures: u32,
}

impl MailboxPoller {
    /// 由初始游标构造。
    pub fn new(initial: MailboxCursor) -> Self {
        Self {
            cursor: initial,
            consumed: VecDeque::new(),
            pending_ack: Vec::new(),
            groups: BTreeMap::new(),
            held: Vec::new(),
            next_poll_at_ms: 0,
            consecutive_pull_failures: 0,
        }
    }

    /// 当前游标。
    pub fn cursor(&self) -> &MailboxCursor {
        &self.cursor
    }

    /// 待 ACK 条数。
    pub fn pending_ack_len(&self) -> usize {
        self.pending_ack.len()
    }

    /// 暂存条数。
    pub fn held_len(&self) -> usize {
        self.held.len()
    }

    /// 下一次允许轮询的时间。
    pub const fn next_poll_at_ms(&self) -> i64 {
        self.next_poll_at_ms
    }

    /// 现在是否允许轮询。
    pub fn should_poll(&self, now_unix_ms: i64) -> bool {
        now_unix_ms >= self.next_poll_at_ms
    }

    /// 用已补齐的 verified head 对齐群日志起点（SYNC-008 之后调用）。
    ///
    /// 已暂存的条目不会被丢弃，下一次 [`Self::tick`] 会把连续部分按序提升。
    pub fn seed_group(&mut self, group_id: &str, next_seq: u64) -> Result<()> {
        validate_id("mailbox group", group_id)?;
        if next_seq == 0 {
            return Err(invalid_target("group log sequence starts at 1"));
        }
        if !self.groups.contains_key(group_id) && self.groups.len() >= MAX_TRACKED_GROUPS {
            return Err(content_too_large("too many tracked groups"));
        }
        self.groups.insert(group_id.to_owned(), next_seq);
        Ok(())
    }

    /// 主循环的一步：补 ACK → 拉取 → 消费；时间未到则返回 `Idle`。
    pub fn tick(
        &mut self,
        clock: &dyn Clock,
        source: &mut dyn MailboxSource,
        ack: &mut dyn AckSink,
    ) -> Result<PollOutcome> {
        let now = clock.now_unix_ms();
        if now < 0 {
            return Err(invalid_target("mailbox clock must be non-negative"));
        }
        if now < self.next_poll_at_ms {
            return Ok(self.outcome(PollStatus::Idle, Counts::default(), Vec::new(), 0, None));
        }

        // 1. 先重试待 ACK：失败只保留集合并退避，游标与已消费记录都不回退。
        let mut acked = 0u32;
        if !self.pending_ack.is_empty() {
            let ids = std::mem::take(&mut self.pending_ack);
            match ack.ack(&ids) {
                Ok(()) => {
                    acked = u32::try_from(ids.len()).unwrap_or(u32::MAX);
                }
                Err(error) => {
                    self.pending_ack = ids;
                    self.next_poll_at_ms = now.saturating_add(ACK_FAILURE_BACKOFF_MS);
                    return Ok(self.outcome(
                        PollStatus::AckDeferred,
                        Counts::default(),
                        Vec::new(),
                        acked,
                        Some(error.code()),
                    ));
                }
            }
        }

        // 2. 拉取：失败时游标不动，按指数退避；绝不忙循环。
        let page = match source.pull(&self.cursor, MAX_MAILBOX_PAGE_ITEMS) {
            Ok(page) => page,
            Err(error) => {
                self.consecutive_pull_failures = self.consecutive_pull_failures.saturating_add(1);
                let backoff = pull_backoff(self.consecutive_pull_failures);
                self.next_poll_at_ms = now.saturating_add(backoff);
                return Ok(self.outcome(
                    PollStatus::PullFailed,
                    Counts::default(),
                    Vec::new(),
                    acked,
                    Some(error.code()),
                ));
            }
        };
        self.consecutive_pull_failures = 0;

        // 3. 消费：畸形页显式失败且不改变任何状态（内部快照回滚）。
        let consumed = match self.consume(page) {
            Ok(consumed) => consumed,
            Err(error) => {
                // 畸形页也要退避，避免坏节点把客户端拖进忙循环。
                self.next_poll_at_ms = now.saturating_add(EMPTY_PAGE_BACKOFF_MS);
                return Err(error);
            }
        };
        let mut delivered = consumed.delivered;
        // 4. 把此前暂存、现在已经连续的条目按序提升。
        let promoted = self.drain_all(&mut delivered);

        // 5. 待 ACK 集合满了：停止拉取，交给上层把 ACK 送出去（背压）。
        if self.pending_ack.len() >= MAX_PENDING_ACK {
            self.next_poll_at_ms = now.saturating_add(ACK_FAILURE_BACKOFF_MS);
            return Ok(self.outcome(
                PollStatus::Backpressure,
                consumed.counts,
                delivered,
                acked,
                Some(ErrorCode::ContentTooLarge),
            ));
        }

        let progress = consumed.counts.received > 0 || promoted > 0;
        self.next_poll_at_ms = now.saturating_add(if progress {
            PAGE_INTERVAL_MS
        } else {
            EMPTY_PAGE_BACKOFF_MS
        });
        Ok(self.outcome(
            PollStatus::Polled,
            consumed.counts,
            delivered,
            acked,
            None,
        ))
    }

    fn outcome(
        &self,
        status: PollStatus,
        counts: Counts,
        delivered: Vec<MailboxItem>,
        acked: u32,
        error: Option<ErrorCode>,
    ) -> PollOutcome {
        PollOutcome {
            status,
            received: counts.received,
            delivered,
            held: counts.held,
            duplicates: counts.duplicates,
            acked,
            pending_ack: u32::try_from(self.pending_ack.len()).unwrap_or(u32::MAX),
            cursor: self.cursor.clone(),
            next_poll_at_ms: self.next_poll_at_ms,
            error,
        }
    }

    /// 消费一页：任何失败都不改变状态（先克隆快照，失败整体回滚）。
    fn consume(&mut self, page: MailboxPage) -> Result<ConsumedPage> {
        let backup = self.clone();
        match self.consume_inner(page) {
            Ok(consumed) => Ok(consumed),
            Err(error) => {
                *self = backup;
                Err(error)
            }
        }
    }

    fn consume_inner(&mut self, page: MailboxPage) -> Result<ConsumedPage> {
        // 阶段 1：结构与预算校验（不改状态）。
        if usize::try_from(MAX_MAILBOX_PAGE_ITEMS).unwrap_or(usize::MAX) < page.items.len() {
            return Err(content_too_large("mailbox page exceeds the item budget"));
        }
        let mut total_bytes = 0usize;
        let mut previous_position: Option<u64> = None;
        for item in &page.items {
            if item.envelope.len() > MAX_MAILBOX_ITEM_BYTES {
                return Err(content_too_large("mailbox item exceeds the byte budget"));
            }
            total_bytes = total_bytes
                .checked_add(item.envelope.len())
                .ok_or_else(|| content_too_large("mailbox page byte total overflows"))?;
            if let Some(previous) = previous_position
                && item.position <= previous
            {
                return Err(invalid_cursor("mailbox item positions must increase"));
            }
            previous_position = Some(item.position);
            if item.position <= self.cursor.position
                && self.fingerprint_of(&item.item_id) != Some(fingerprint(item))
            {
                return Err(invalid_cursor("mailbox item position is behind the cursor"));
            }
            if item.kind == MailboxItemKind::LogEntry {
                let group_id = item
                    .group_id
                    .as_deref()
                    .ok_or_else(|| invalid_state("log mailbox item has no group ID"))?;
                validate_id("mailbox group", group_id)?;
                if item.group_seq.unwrap_or(0) == 0 {
                    return Err(invalid_state("log mailbox item has no group sequence"));
                }
            }
        }
        if total_bytes > MAX_MAILBOX_PAGE_BYTES {
            return Err(content_too_large("mailbox page exceeds the byte budget"));
        }
        if page.next.position < self.cursor.position {
            return Err(invalid_cursor("mailbox cursor regressed"));
        }
        if page.next.position == self.cursor.position && !page.items.is_empty() {
            return Err(invalid_cursor("mailbox page made no cursor progress"));
        }

        // 阶段 2：应用（任何容量越界仍返回错误，由 consume 整体回滚）。
        let mut delivered = Vec::new();
        let mut counts = Counts::default();
        for item in page.items {
            counts.received = counts.received.saturating_add(1);
            let item_fingerprint = fingerprint(&item);
            if let Some(known) = self.fingerprint_of(&item.item_id) {
                if known != item_fingerprint {
                    return Err(invalid_candidate(
                        "duplicate mailbox item ID with different content",
                    ));
                }
                counts.duplicates = counts.duplicates.saturating_add(1);
                self.ack_later(item.item_id);
                continue;
            }
            self.remember(&item, item_fingerprint);
            if item.kind != MailboxItemKind::LogEntry {
                self.ack_later(item.item_id.clone());
                delivered.push(item);
                continue;
            }
            let group_id = item
                .group_id
                .clone()
                .ok_or_else(|| invalid_state("log mailbox item has no group ID"))?;
            let group_seq = item
                .group_seq
                .ok_or_else(|| invalid_state("log mailbox item has no group sequence"))?;
            let expected = self.groups.get(&group_id).copied().unwrap_or(1);
            if group_seq < expected {
                // 已交付过的旧序号：按重复处理，不重复投递。
                counts.duplicates = counts.duplicates.saturating_add(1);
                self.ack_later(item.item_id);
            } else if group_seq == expected {
                self.ensure_group_slot(&group_id)?;
                self.groups
                    .insert(group_id.clone(), expected.saturating_add(1));
                self.ack_later(item.item_id.clone());
                delivered.push(item);
                self.drain_group(&group_id, &mut delivered);
            } else {
                self.ensure_group_slot(&group_id)?;
                self.groups.entry(group_id.clone()).or_insert(1);
                if self.held.len() >= MAX_HELD_ITEMS {
                    return Err(missing_log_entries("mailbox hold buffer is full"));
                }
                counts.held = counts.held.saturating_add(1);
                self.ack_later(item.item_id.clone());
                self.held.push(HeldItem {
                    group_id,
                    group_seq,
                    item,
                });
            }
        }
        if self.pending_ack.len() > MAX_PENDING_ACK {
            return Err(content_too_large("pending ACK set exceeds the budget"));
        }
        self.cursor = page.next;
        Ok(ConsumedPage { counts, delivered })
    }

    fn ensure_group_slot(&mut self, group_id: &str) -> Result<()> {
        if !self.groups.contains_key(group_id) && self.groups.len() >= MAX_TRACKED_GROUPS {
            return Err(content_too_large("too many tracked groups"));
        }
        Ok(())
    }

    fn remember(&mut self, item: &MailboxItem, item_fingerprint: u64) {
        if self.consumed.len() >= MAX_TRACKED_ITEM_IDS {
            self.consumed.pop_front();
        }
        self.consumed.push_back((item.item_id.clone(), item_fingerprint));
    }

    fn fingerprint_of(&self, item_id: &MailboxItemId) -> Option<u64> {
        self.consumed
            .iter()
            .rev()
            .find(|(known, _)| known == item_id)
            .map(|(_, value)| *value)
    }

    fn ack_later(&mut self, item_id: MailboxItemId) {
        if !self.pending_ack.contains(&item_id) {
            self.pending_ack.push(item_id);
        }
    }

    fn drain_group(&mut self, group_id: &str, delivered: &mut Vec<MailboxItem>) {
        while let Some(expected) = self.groups.get(group_id).copied() {
            let Some(index) = self
                .held
                .iter()
                .position(|held| held.group_id == group_id && held.group_seq == expected)
            else {
                break;
            };
            let held = self.held.remove(index);
            self.groups
                .insert(group_id.to_owned(), expected.saturating_add(1));
            delivered.push(held.item);
        }
    }

    fn drain_all(&mut self, delivered: &mut Vec<MailboxItem>) -> u32 {
        let mut promoted = 0u32;
        loop {
            let before = delivered.len();
            let groups: Vec<String> = self.groups.keys().cloned().collect();
            for group_id in groups {
                self.drain_group(&group_id, delivered);
            }
            if delivered.len() == before {
                break;
            }
            promoted = promoted.saturating_add(
                u32::try_from(delivered.len() - before).unwrap_or(u32::MAX),
            );
        }
        promoted
    }
}

struct ConsumedPage {
    counts: Counts,
    delivered: Vec<MailboxItem>,
}

fn fingerprint(item: &MailboxItem) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(b"chat/sync/mailbox-item/v1");
    hasher.update(item.item_id.as_str().as_bytes());
    hasher.update(item.position.to_be_bytes());
    hasher.update([kind_tag(item.kind)]);
    if let Some(group_id) = item.group_id.as_deref() {
        hasher.update((group_id.len() as u64).to_be_bytes());
        hasher.update(group_id.as_bytes());
    }
    hasher.update(item.group_seq.unwrap_or(0).to_be_bytes());
    hasher.update((item.envelope.len() as u64).to_be_bytes());
    hasher.update(item.envelope.as_slice());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes)
}

const fn kind_tag(kind: MailboxItemKind) -> u8 {
    match kind {
        MailboxItemKind::Welcome => 1,
        MailboxItemKind::LogEntry => 2,
        MailboxItemKind::Receipt => 3,
        MailboxItemKind::Unknown => 4,
    }
}

fn pull_backoff(consecutive_failures: u32) -> i64 {
    let exponent = consecutive_failures.saturating_sub(1).min(6);
    let factor = 1i64 << exponent;
    PULL_FAILURE_BASE_BACKOFF_MS
        .saturating_mul(factor)
        .min(PULL_FAILURE_MAX_BACKOFF_MS)
}
