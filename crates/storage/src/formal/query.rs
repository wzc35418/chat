//! STORAGE-012 / STORAGE-013：稳定键集分页查询。
//!
//! 两个查询都满足契约的硬性要求：
//! - **稳定排序**：正式消息按 `(group_seq DESC, message_id DESC)`，待发意图按
//!   `(created_at DESC, draft_id DESC)`（独立本地排序键），会话列表按
//!   `(last_activity_at DESC, conversation_id DESC)`；同一时刻用稳定次键消歧。
//! - **键集游标**：不使用 OFFSET，取 `limit+1` 判断是否还有下一页，`nextCursor=null`
//!   表示没有更多；游标经认证封装（见子模块 `cursor`），改字节/换会话/过期都显式失败。
//! - **方向语义**：`before/cursor = None` 表示第一页（最新一页）；`Some` 表示
//!   “严格更旧的一页”。没有“更新方向”的游标，调用方必须从最新页开始。
//! - **只读快照**：整个查询在一个只读事务里完成，两条 SQL 看到同一快照，
//!   因此并发到达的消息不会造成重复或跳页。
//! - **有界**：limit 上限、游标文本上限、会话标识上限；不读全历史。
//!
//! 限制（与正式语义的差异，均在 docs/implementation/storage.md 记录）：
//! - 游标是内部 `spg1` 令牌，不是 PROTOCOL-012 的 `cur1` 线协议令牌；
//! - 待发意图暂时用 `draft_id` 作为稳定本地投影 ID，正式 MessageId 在 STORAGE-008
//!   接受条目后才存在；
//! - 未读计数需要尚未实现的 read-cursor 存储，这里显式返回 `None`，不伪造。

use rusqlite::{Connection, OptionalExtension, params};

use super::cursor::{
    CONVERSATION_SORT_VERSION, CursorScope, MESSAGE_SORT_VERSION, PageCursor, PagePosition,
    PageResource, decode_page_cursor, encode_page_cursor,
};
use super::error::StorageError;
use super::seal::{RecordSealer, SealPurpose};
use super::{MessageContent, Result, Store, ensure_id, now_ms};

/// 消息页单页上限（契约：limit 1..100）。
pub const MAX_MESSAGE_PAGE_LIMIT: u32 = 100;
/// 会话列表单页上限（与消息页同界，避免两个入口行为不一致）。
pub const MAX_CONVERSATION_PAGE_LIMIT: u32 = 100;

/// 消息页条目的展示状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageViewState {
    /// 本机待发意图（尚未成为正式消息）。
    Pending,
    /// 正式接受。
    Accepted,
    /// 已送达。
    Delivered,
}

/// 消息页条目：只读展示投影，不含 MLS 对象或数据库句柄。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageView {
    /// 稳定 ID：正式消息为 MessageId，待发意图为草稿派生的本地投影 ID。
    pub message_id: String,
    /// 会话 ID。
    pub conversation_id: String,
    /// 展示状态。
    pub state: MessageViewState,
    /// 正式群/会话序号；待发意图为 `None`（本地 pending 投影）。
    pub group_seq: Option<u64>,
    /// 展示时间（正式消息为服务端展示时间，待发意图为本地创建时间）。
    pub display_created_at: i64,
    /// 作者账户（待发意图为 `None`，本机身份不落库）。
    pub author_account_id: Option<String>,
    /// 作者设备（待发意图为 `None`）。
    pub author_device_id: Option<String>,
    /// 解密后的可见正文。
    pub content: MessageContent,
    /// 关联的 SubmissionId（本机提交；远端消息为原始提交 ID）。
    pub submission_id: Option<String>,
    /// 是否本机待发投影。
    pub local_pending: bool,
}

/// 一页消息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagePage {
    /// 本页条目（最新在前）。
    pub items: Vec<MessageView>,
    /// 下一页游标；`None` 表示没有更多。
    pub next_cursor: Option<PageCursor>,
}

/// 会话列表条目：列表摘要 + 未读/待发/故障摘要。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationView {
    /// 会话 ID。
    pub conversation_id: String,
    /// 会话种类。
    pub kind: String,
    /// 会话状态。
    pub state: String,
    /// 最近活动时间（消息展示时间与本地意图创建时间的较大值）。
    pub last_activity_at: i64,
    /// 待发意图数（`drafts.state = 'queued'`）。
    pub pending_intents: u64,
    /// 故障摘要：`failed` 状态的 outbox 任务数。
    pub failed_outbox_jobs: u64,
    /// 故障摘要：结果不确定的尝试数。
    pub unknown_attempts: u64,
    /// 未读计数。当前永远为 `None`：正式未读需要 read-cursor 存储，尚未实现，
    /// 这里绝不返回伪造的 0。
    pub unread_count: Option<u32>,
}

/// 一页会话。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationPage {
    /// 本页条目（最近活动在前）。
    pub items: Vec<ConversationView>,
    /// 下一页游标；`None` 表示没有更多。
    pub next_cursor: Option<PageCursor>,
}

fn validate_limit(kind: &str, limit: u32, maximum: u32) -> Result<()> {
    if !(1..=maximum).contains(&limit) {
        return Err(StorageError::InvalidInput(format!(
            "{kind} page limit must be within 1..={maximum}"
        )));
    }
    Ok(())
}

fn validate_time(now_unix_ms: i64) -> Result<()> {
    if now_unix_ms < 0 {
        return Err(StorageError::InvalidInput(
            "time must be a non-negative Unix millisecond timestamp".to_owned(),
        ));
    }
    Ok(())
}

fn ensure_visible(connection: &Connection, conversation_id: &str) -> Result<()> {
    let state: Option<String> = connection
        .query_row(
            "SELECT state FROM conversations WHERE conversation_id = ?1",
            [conversation_id],
            |row| row.get(0),
        )
        .optional()?;
    match state.as_deref() {
        None => Err(StorageError::PermissionDenied(format!(
            "conversation {conversation_id} does not exist"
        ))),
        Some("left") | Some("dissolved") => Err(StorageError::PermissionDenied(format!(
            "conversation {conversation_id} is no longer visible"
        ))),
        Some(_) => Ok(()),
    }
}

fn open_draft_content(
    store: &Store,
    conversation_id: &str,
    draft_id: &str,
    operation_id: &str,
    sealed: &[u8],
) -> Result<MessageContent> {
    let sealer = RecordSealer::new(&store.key, SealPurpose::DraftContent);
    let plaintext = sealer.open(
        &[
            conversation_id.as_bytes(),
            draft_id.as_bytes(),
            operation_id.as_bytes(),
        ],
        sealed,
    )?;
    MessageContent::from_canonical(plaintext.as_slice())
}

fn open_message_content(
    store: &Store,
    conversation_id: &str,
    message_id: &str,
    sealed: &[u8],
) -> Result<MessageContent> {
    let sealer = RecordSealer::new(&store.key, SealPurpose::MessageContent);
    let plaintext = sealer.open(
        &[conversation_id.as_bytes(), message_id.as_bytes()],
        sealed,
    )?;
    MessageContent::from_canonical(plaintext.as_slice())
}

/// 消息页的起始位置。
enum MessageSection {
    /// 第一页：从最新的待发意图与正式消息开始。
    Start,
    /// 从某个待发意图之后继续（严格更旧），并沿用游标里的正式序号水位。
    Pending {
        created_at: i64,
        draft_id: String,
        formal_boundary: i64,
    },
    /// 正式消息段（严格更旧）；不再返回待发意图。
    Formal { group_seq: i64, message_id: String },
}

struct DraftPageRow {
    draft_id: String,
    operation_id: String,
    created_at: i64,
    protected_content: Vec<u8>,
    submission_id: Option<String>,
}

struct MessagePageRow {
    message_id: String,
    group_seq: i64,
    author_account_id: String,
    author_device_id: String,
    protected_content: Vec<u8>,
    send_state: String,
    submission_id: String,
    draft_id: Option<String>,
    display_created_at: i64,
}

fn query_pending_drafts(
    connection: &Connection,
    conversation_id: &str,
    after: Option<(i64, &str)>,
    limit: i64,
) -> Result<Vec<DraftPageRow>> {
    let sql = "
        SELECT d.draft_id, d.operation_id, d.created_at, d.protected_content,
               (SELECT a.submission_id FROM message_attempts a
                 WHERE a.draft_id = d.draft_id
                 ORDER BY a.submission_id DESC LIMIT 1)
          FROM drafts d
         WHERE d.conversation_id = ?1
           AND d.state = 'queued'
           AND NOT EXISTS (SELECT 1 FROM messages m WHERE m.draft_id = d.draft_id)
           AND (?2 IS NULL OR d.created_at < ?2 OR (d.created_at = ?2 AND d.draft_id < ?3))
         ORDER BY d.created_at DESC, d.draft_id DESC
         LIMIT ?4";
    let (after_created_at, after_draft_id) = match after {
        Some((created_at, draft_id)) => (Some(created_at), Some(draft_id)),
        None => (None, None),
    };
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(
        params![conversation_id, after_created_at, after_draft_id, limit],
        |row| {
            Ok(DraftPageRow {
                draft_id: row.get(0)?,
                operation_id: row.get(1)?,
                created_at: row.get(2)?,
                protected_content: row.get(3)?,
                submission_id: row.get(4)?,
            })
        },
    )?;
    let mut drafts = Vec::new();
    for row in rows {
        drafts.push(row?);
    }
    Ok(drafts)
}

fn query_messages(
    connection: &Connection,
    conversation_id: &str,
    boundary: Option<i64>,
    after: Option<(i64, &str)>,
    limit: i64,
) -> Result<Vec<MessagePageRow>> {
    let sql = "
        SELECT m.message_id, m.group_seq, m.author_account_id, m.author_device_id,
               m.protected_content, m.send_state, m.submission_id, m.draft_id,
               m.display_created_at
          FROM messages m
         WHERE m.conversation_id = ?1
           AND (?2 IS NULL OR m.group_seq <= ?2)
           AND (?3 IS NULL OR m.group_seq < ?3 OR (m.group_seq = ?3 AND m.message_id < ?4))
         ORDER BY m.group_seq DESC, m.message_id DESC
         LIMIT ?5";
    let (after_group_seq, after_message_id) = match after {
        Some((group_seq, message_id)) => (Some(group_seq), Some(message_id)),
        None => (None, None),
    };
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(
        params![conversation_id, boundary, after_group_seq, after_message_id, limit],
        |row| {
            Ok(MessagePageRow {
                message_id: row.get(0)?,
                group_seq: row.get(1)?,
                author_account_id: row.get(2)?,
                author_device_id: row.get(3)?,
                protected_content: row.get(4)?,
                send_state: row.get(5)?,
                submission_id: row.get(6)?,
                draft_id: row.get(7)?,
                display_created_at: row.get(8)?,
            })
        },
    )?;
    let mut messages = Vec::new();
    for row in rows {
        messages.push(row?);
    }
    Ok(messages)
}

impl Store {
    /// STORAGE-012：会话消息分页（生产入口，使用系统 UTC 时钟校验游标）。
    pub fn query_message_page(
        &self,
        conversation_id: &str,
        before: Option<&PageCursor>,
        limit: u32,
    ) -> Result<MessagePage> {
        self.query_message_page_at(conversation_id, before, limit, now_ms())
    }

    /// STORAGE-012（注入时间戳）：游标过期检查使用调用方提供的可信时间。
    pub fn query_message_page_at(
        &self,
        conversation_id: &str,
        before: Option<&PageCursor>,
        limit: u32,
        now_unix_ms: i64,
    ) -> Result<MessagePage> {
        self.ensure_ready()?;
        ensure_id("conversation", conversation_id)?;
        validate_limit("message", limit, MAX_MESSAGE_PAGE_LIMIT)?;
        validate_time(now_unix_ms)?;

        let scope = CursorScope {
            conversation_id,
            resource: PageResource::Messages,
            sort_version: MESSAGE_SORT_VERSION,
        };
        let section = match before {
            None => MessageSection::Start,
            Some(cursor) => match decode_page_cursor(&self.key, &scope, cursor, now_unix_ms)? {
                PagePosition::Pending {
                    created_at,
                    draft_id,
                    formal_boundary,
                } => MessageSection::Pending {
                    created_at,
                    draft_id,
                    formal_boundary,
                },
                PagePosition::Formal {
                    group_seq,
                    message_id,
                } => MessageSection::Formal {
                    group_seq,
                    message_id,
                },
                PagePosition::Conversation { .. } => {
                    return Err(StorageError::InvalidCursor(
                        "message page cursor carries a conversation-list position".to_owned(),
                    ));
                }
            },
        };

        // 只读快照：两条查询看到同一版本的数据，避免并发到达造成重复/跳页。
        let snapshot = self.connection.unchecked_transaction()?;
        ensure_visible(&snapshot, conversation_id)?;

        let pending_after = match &section {
            MessageSection::Pending {
                created_at,
                draft_id,
                ..
            } => Some((*created_at, draft_id.as_str())),
            MessageSection::Start | MessageSection::Formal { .. } => None,
        };
        let page_limit = i64::from(limit);
        let pending_rows = match section {
            MessageSection::Formal { .. } => Vec::new(),
            _ => query_pending_drafts(
                &snapshot,
                conversation_id,
                pending_after,
                page_limit.saturating_add(1),
            )?,
        };
        let formal_after = match &section {
            MessageSection::Formal {
                group_seq,
                message_id,
            } => Some((*group_seq, message_id.as_str())),
            _ => None,
        };
        // 待发段与正式段没有可比较的排序键，因此这里固定一个正式序号水位：
        // - 第一页取当前最大序号；已经处于待发段的游标沿用（只减不增）原水位，
        //   这样翻页期间新到的消息不会插进“更旧”的一页。
        let (formal_cap, pending_boundary) = match &section {
            MessageSection::Formal { .. } => (None, 0),
            _ => {
                let current_max = max_group_seq(&snapshot, conversation_id)?;
                let cap = match &section {
                    MessageSection::Pending { formal_boundary, .. } => {
                        Some(current_max.min(*formal_boundary))
                    }
                    _ => None,
                };
                (cap, cap.unwrap_or(current_max))
            }
        };

        let mut entries: Vec<(MessageView, PagePosition)> = Vec::new();
        let capacity = usize::try_from(limit).unwrap_or(usize::MAX);
        let has_more;
        if pending_rows.len() > capacity {
            has_more = true;
            for row in pending_rows.into_iter().take(capacity) {
                entries.push(pending_entry(
                    self,
                    conversation_id,
                    row,
                    pending_boundary,
                )?);
            }
        } else {
            let remaining = capacity + 1 - pending_rows.len();
            let message_rows = query_messages(
                &snapshot,
                conversation_id,
                formal_cap,
                formal_after,
                i64::try_from(remaining).unwrap_or(i64::MAX),
            )?;
            has_more = pending_rows.len() + message_rows.len() > capacity;
            for row in pending_rows {
                entries.push(pending_entry(
                    self,
                    conversation_id,
                    row,
                    pending_boundary,
                )?);
            }
            for row in message_rows {
                entries.push(message_entry(self, conversation_id, row)?);
            }
            entries.truncate(capacity);
        }

        let next_cursor = if has_more {
            match entries.last() {
                Some((_, position)) => Some(encode_page_cursor(
                    &self.key,
                    &scope,
                    position,
                    now_unix_ms,
                )?),
                None => None,
            }
        } else {
            None
        };

        Ok(MessagePage {
            items: entries.into_iter().map(|(view, _)| view).collect(),
            next_cursor,
        })
    }

    /// STORAGE-013：会话列表分页（生产入口，使用系统 UTC 时钟校验游标）。
    pub fn query_conversation_page(
        &self,
        cursor: Option<&PageCursor>,
        limit: u32,
    ) -> Result<ConversationPage> {
        self.query_conversation_page_at(cursor, limit, now_ms())
    }

    /// STORAGE-013（注入时间戳）：游标过期检查使用调用方提供的可信时间。
    pub fn query_conversation_page_at(
        &self,
        cursor: Option<&PageCursor>,
        limit: u32,
        now_unix_ms: i64,
    ) -> Result<ConversationPage> {
        self.ensure_ready()?;
        validate_limit("conversation", limit, MAX_CONVERSATION_PAGE_LIMIT)?;
        validate_time(now_unix_ms)?;

        let scope = CursorScope {
            conversation_id: "",
            resource: PageResource::Conversations,
            sort_version: CONVERSATION_SORT_VERSION,
        };
        let after = match cursor {
            None => None,
            Some(cursor) => match decode_page_cursor(&self.key, &scope, cursor, now_unix_ms)? {
                PagePosition::Conversation {
                    last_activity_at,
                    conversation_id,
                } => Some((last_activity_at, conversation_id)),
                _ => {
                    return Err(StorageError::InvalidCursor(
                        "conversation page cursor carries a message position".to_owned(),
                    ));
                }
            },
        };

        let snapshot = self.connection.unchecked_transaction()?;
        let capacity = usize::try_from(limit).unwrap_or(usize::MAX);
        let (after_activity, after_id) = match &after {
            Some((activity, conversation_id)) => (Some(*activity), Some(conversation_id.as_str())),
            None => (None, None),
        };
        let sql = "
            SELECT conversation_id, kind, state, last_activity_at,
                   pending_intents, failed_jobs, unknown_attempts
              FROM (
                SELECT c.conversation_id AS conversation_id,
                       c.kind AS kind,
                       c.state AS state,
                       MAX(
                         COALESCE((SELECT MAX(m.display_created_at) FROM messages m
                                    WHERE m.conversation_id = c.conversation_id), 0),
                         COALESCE((SELECT MAX(d.created_at) FROM drafts d
                                    WHERE d.conversation_id = c.conversation_id), 0)
                       ) AS last_activity_at,
                       (SELECT COUNT(*) FROM drafts d
                         WHERE d.conversation_id = c.conversation_id AND d.state = 'queued') AS pending_intents,
                       (SELECT COUNT(*) FROM outbox_jobs j
                         WHERE j.conversation_id = c.conversation_id AND j.state = 'failed') AS failed_jobs,
                       (SELECT COUNT(*) FROM message_attempts a
                         WHERE a.conversation_id = c.conversation_id AND a.state = 'unknown') AS unknown_attempts
                  FROM conversations c
                 WHERE c.state NOT IN ('left', 'dissolved')
              ) AS page
             WHERE ?1 IS NULL OR last_activity_at < ?1 OR (last_activity_at = ?1 AND conversation_id < ?2)
             ORDER BY last_activity_at DESC, conversation_id DESC
             LIMIT ?3";
        let mut entries: Vec<(ConversationView, PagePosition)> = Vec::new();
        {
            let mut statement = snapshot.prepare(sql)?;
            let rows = statement.query_map(
                params![
                    after_activity,
                    after_id,
                    i64::try_from(capacity + 1).unwrap_or(i64::MAX)
                ],
                |row| {
                    Ok(ConversationView {
                        conversation_id: row.get(0)?,
                        kind: row.get(1)?,
                        state: row.get(2)?,
                        last_activity_at: row.get(3)?,
                        pending_intents: row.get::<_, i64>(4)?.max(0) as u64,
                        failed_outbox_jobs: row.get::<_, i64>(5)?.max(0) as u64,
                        unknown_attempts: row.get::<_, i64>(6)?.max(0) as u64,
                        unread_count: None,
                    })
                },
            )?;
            for row in rows {
                let view = row?;
                entries.push((
                    view.clone(),
                    PagePosition::Conversation {
                        last_activity_at: view.last_activity_at,
                        conversation_id: view.conversation_id.clone(),
                    },
                ));
            }
        }
        let has_more = entries.len() > capacity;
        entries.truncate(capacity);
        let next_cursor = if has_more {
            match entries.last() {
                Some((_, position)) => Some(encode_page_cursor(
                    &self.key,
                    &scope,
                    position,
                    now_unix_ms,
                )?),
                None => None,
            }
        } else {
            None
        };
        Ok(ConversationPage {
            items: entries.into_iter().map(|(view, _)| view).collect(),
            next_cursor,
        })
    }

    /// STORAGE-008 的写入契约：封装 `messages.protected_content`。
    ///
    /// AAD 绑定 `(conversation_id, message_id)`，把密文挪到别的会话/消息会认证失败。
    pub fn seal_message_content(
        &self,
        conversation_id: &str,
        message_id: &str,
        content: &MessageContent,
    ) -> Result<Vec<u8>> {
        self.ensure_ready()?;
        ensure_id("conversation", conversation_id)?;
        ensure_id("message", message_id)?;
        content.validate()?;
        let sealer = RecordSealer::new(&self.key, SealPurpose::MessageContent);
        sealer.seal(
            &[conversation_id.as_bytes(), message_id.as_bytes()],
            content.canonical_bytes(),
        )
    }
}

fn max_group_seq(connection: &Connection, conversation_id: &str) -> Result<i64> {
    let maximum: Option<i64> = connection.query_row(
        "SELECT MAX(group_seq) FROM messages WHERE conversation_id = ?1",
        [conversation_id],
        |row| row.get(0),
    )?;
    Ok(maximum.unwrap_or(0))
}

fn pending_entry(
    store: &Store,
    conversation_id: &str,
    row: DraftPageRow,
    formal_boundary: i64,
) -> Result<(MessageView, PagePosition)> {
    let content = open_draft_content(
        store,
        conversation_id,
        &row.draft_id,
        &row.operation_id,
        &row.protected_content,
    )?;
    let position = PagePosition::Pending {
        created_at: row.created_at,
        draft_id: row.draft_id.clone(),
        formal_boundary,
    };
    Ok((
        MessageView {
            message_id: row.draft_id,
            conversation_id: conversation_id.to_owned(),
            state: MessageViewState::Pending,
            group_seq: None,
            display_created_at: row.created_at,
            author_account_id: None,
            author_device_id: None,
            content,
            submission_id: row.submission_id,
            local_pending: true,
        },
        position,
    ))
}

fn message_entry(
    store: &Store,
    conversation_id: &str,
    row: MessagePageRow,
) -> Result<(MessageView, PagePosition)> {
    let content = open_message_content(
        store,
        conversation_id,
        &row.message_id,
        &row.protected_content,
    )?;
    let state = match row.send_state.as_str() {
        "accepted" => MessageViewState::Accepted,
        "delivered" => MessageViewState::Delivered,
        other => {
            return Err(StorageError::StateCorrupted(format!(
                "stored message send state {other} is not a display state"
            )));
        }
    };
    let position = PagePosition::Formal {
        group_seq: row.group_seq,
        message_id: row.message_id.clone(),
    };
    Ok((
        MessageView {
            message_id: row.message_id,
            conversation_id: conversation_id.to_owned(),
            state,
            group_seq: u64::try_from(row.group_seq).ok(),
            display_created_at: row.display_created_at,
            author_account_id: Some(row.author_account_id),
            author_device_id: Some(row.author_device_id),
            content,
            submission_id: Some(row.submission_id),
            local_pending: row.draft_id.is_some(),
        },
        position,
    ))
}
