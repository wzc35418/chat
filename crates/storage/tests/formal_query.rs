//! STORAGE-012 / STORAGE-013 分页查询集成测试。
//!
//! 覆盖：分页边界与重复页（并发到达不重复/跳页）、limit 上限、非法/过期/跨会话/跨资源游标、
//! 待发意图与正式消息的合并排序、会话列表稳定次键、篡改密文显式失败、
//! fail-closed 后不泄漏旧摘要。

use chat_storage::formal::{
    KeyProvenance, MAX_CONVERSATION_PAGE_LIMIT, MAX_MESSAGE_PAGE_LIMIT, MessageContent,
    MessageOrigin, MessageViewState, PageCursor, PreparedWrite, ProtectedStorageKey, StorageError,
    Store, StoreOptions, ValidatedSendRequest, WriteScope, commit_transaction, open_store,
    persist_prepared_attempt, persist_send_intent,
};
use rusqlite::params;
use std::fs;
use std::path::PathBuf;

const CONVERSATION: &str = "conv-1";
const CONVERSATION_TWO: &str = "conv-2";
const CONVERSATION_THREE: &str = "conv-3";
const NOW: i64 = 1_700_000_000_000;

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
    key_bytes: [u8; 32],
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("chat-formal-query-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).expect("create fixture directory");
        let path = dir.join("chat.db");
        let mut key_bytes = [0u8; 32];
        key_bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        key_bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Self {
            dir,
            path,
            key_bytes,
        }
    }

    fn key(&self) -> ProtectedStorageKey {
        ProtectedStorageKey::from_bytes(self.key_bytes)
    }

    fn open(&self) -> Store {
        open_store(
            StoreOptions::new(&self.path, KeyProvenance::Raw32),
            self.key(),
        )
        .expect("open formal store")
    }

    fn seed(&self, store: &Store, conversations: &[&str]) {
        let connection = store.connection();
        connection
            .execute(
                "INSERT INTO accounts (account_id, public_key, protected_profile) VALUES ('account-1', X'00', X'00')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO device_credentials (device_id, account_id, credential, endpoint_id, status, version)
                 VALUES ('device-1', 'account-1', X'00', 'endpoint-1', 'active', 0)",
                [],
            )
            .unwrap();
        for conversation in conversations {
            connection
                .execute(
                    "INSERT INTO conversations (conversation_id, kind, coordinator_id, protected_metadata, state)
                     VALUES (?1, 'direct', 'node-1', X'00', 'active')",
                    [conversation],
                )
                .unwrap();
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn error_code<T>(result: Result<T, StorageError>) -> &'static str {
    match result {
        Ok(_) => panic!("expected an explicit failure"),
        Err(error) => error.code(),
    }
}

fn create_draft(store: &mut Store, conversation: &str, operation: &str, text: &str, created_at: i64) -> String {
    let request = ValidatedSendRequest {
        conversation_id: conversation.to_owned(),
        client_operation_id: operation.to_owned(),
        content: MessageContent::text(text).unwrap(),
        reply_to: None,
        origin: MessageOrigin::UserInput,
        created_at,
    };
    let mut transaction = store
        .begin_transaction(WriteScope::intent(conversation))
        .unwrap();
    let draft_id = persist_send_intent(&mut transaction, request).unwrap().draft_id;
    commit_transaction(transaction).unwrap();
    draft_id
}

/// 插入一条远端已接受消息（含独立 attempt，符合 0001 的 origin CHECK）。
fn add_remote_message(
    store: &Store,
    conversation: &str,
    message_id: &str,
    group_seq: i64,
    display_created_at: i64,
    text: &str,
) {
    let submission_id = format!("sub-{message_id}");
    let sealed = store
        .seal_message_content(conversation, message_id, &MessageContent::text(text).unwrap())
        .unwrap();
    let connection = store.connection();
    connection
        .execute(
            "INSERT INTO message_attempts (submission_id, origin, draft_id, operation_id, conversation_id,
                                           mls_epoch, immutable_ciphertext, candidate_hash, state)
             VALUES (?1, 'remote', NULL, NULL, ?2, 1, X'0102', ?3, 'accepted')",
            params![submission_id, conversation, vec![0u8; 32]],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO messages (message_id, conversation_id, draft_id, submission_id, author_account_id,
                                   author_device_id, group_seq, protected_content, display_created_at, send_state)
             VALUES (?1, ?2, NULL, ?3, 'account-1', 'device-1', ?4, ?5, ?6, 'accepted')",
            params![
                message_id,
                conversation,
                submission_id,
                group_seq,
                sealed,
                display_created_at
            ],
        )
        .unwrap();
}

#[test]
fn message_page_merges_pending_and_accepted_without_duplicates_or_skips() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    fixture.seed(&store, &[CONVERSATION]);
    for index in 1..=5 {
        add_remote_message(
            &store,
            CONVERSATION,
            &format!("msg-{index}"),
            index,
            NOW + index,
            &format!("正文 {index}"),
        );
    }
    create_draft(&mut store, CONVERSATION, "op-old", "待发较早", NOW - 10);
    let newest_draft = create_draft(&mut store, CONVERSATION, "op-new", "待发较新", NOW + 100);

    let mut collected: Vec<String> = Vec::new();
    let mut cursor: Option<PageCursor> = None;
    let mut pages = 0;
    loop {
        let page = store
            .query_message_page_at(CONVERSATION, cursor.as_ref(), 2, NOW + 200)
            .expect("page");
        pages += 1;
        assert!(page.items.len() <= 2, "limit must be honoured");
        for item in &page.items {
            collected.push(item.message_id.clone());
        }
        // 翻页过程中插入更新的数据：新条目只出现在“最新页”，旧游标不得重复或跳页。
        if pages == 1 {
            add_remote_message(
                &store,
                CONVERSATION,
                "msg-6",
                6,
                NOW + 300,
                "并发到达",
            );
            create_draft(&mut store, CONVERSATION, "op-concurrent", "并发意图", NOW + 300);
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(pages < 20, "pagination must terminate");
    }
    assert_eq!(collected.len(), 7, "7 pre-existing items, no duplicates: {collected:?}");
    let mut unique = collected.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), collected.len(), "page must not repeat items");
    // 最新页 = 两条待发意图（较新在前），然后正式消息按 seq 降序。
    assert_eq!(
        collected[0], newest_draft,
        "newest pending intent comes first: {collected:?}"
    );
    assert_eq!(collected[2], "msg-5");
    assert_eq!(collected[6], "msg-1");
    // 并发插入的条目比游标新，因此不会出现在后续页里。
    assert!(!collected.iter().any(|id| id == "msg-6"));
    assert!(!collected.iter().any(|id| id == "op-concurrent"));
}

#[test]
fn message_page_enforces_limits_and_rejects_bad_cursors() {
    let fixture = Fixture::new();
    let store = fixture.open();
    fixture.seed(&store, &[CONVERSATION, CONVERSATION_TWO, CONVERSATION_THREE]);
    for index in 1..=3 {
        add_remote_message(
            &store,
            CONVERSATION,
            &format!("msg-{index}"),
            index,
            NOW + index,
            "正文",
        );
        add_remote_message(
            &store,
            CONVERSATION_TWO,
            &format!("other-{index}"),
            index,
            NOW + index,
            "别的会话",
        );
    }
    assert_eq!(
        error_code(store.query_message_page_at(CONVERSATION, None, 0, NOW)),
        "INVALID_INPUT"
    );
    assert_eq!(
        error_code(store.query_message_page_at(
            CONVERSATION,
            None,
            MAX_MESSAGE_PAGE_LIMIT + 1,
            NOW
        )),
        "INVALID_INPUT"
    );
    assert_eq!(
        error_code(store.query_message_page_at(CONVERSATION, None, 10, -1)),
        "INVALID_INPUT"
    );
    assert_eq!(
        error_code(store.query_message_page_at("conv-missing", None, 10, NOW)),
        "PERMISSION_DENIED"
    );
    store
        .connection()
        .execute(
            "UPDATE conversations SET state = 'left' WHERE conversation_id = ?1",
            [CONVERSATION_TWO],
        )
        .unwrap();
    assert_eq!(
        error_code(store.query_message_page_at(CONVERSATION_TWO, None, 10, NOW)),
        "PERMISSION_DENIED"
    );

    let first = store
        .query_message_page_at(CONVERSATION, None, 1, NOW)
        .unwrap();
    let cursor = first.next_cursor.expect("more pages");
    assert_eq!(first.items.len(), 1);

    // 篡改一字节：认证失败。
    let mut chars: Vec<char> = cursor.as_str().chars().collect();
    let last = chars.len() - 1;
    chars[last] = if chars[last] == '0' { '1' } else { '0' };
    let tampered = PageCursor::from_text(chars.into_iter().collect::<String>());
    assert_eq!(
        error_code(store.query_message_page_at(CONVERSATION, Some(&tampered), 1, NOW)),
        "INVALID_CURSOR"
    );
    // 跨会话重放。
    assert_eq!(
        error_code(store.query_message_page_at(CONVERSATION_TWO, Some(&cursor), 1, NOW)),
        "INVALID_CURSOR"
    );
    // 过期游标：TTL 之外必须显式失败，不跳到最新页。
    assert_eq!(
        error_code(store.query_message_page_at(
            CONVERSATION,
            Some(&cursor),
            1,
            NOW + 10 * 60 * 1000
        )),
        "INVALID_CURSOR"
    );
    // 会话列表游标不能用于消息页。
    let conversation_cursor = store
        .query_conversation_page_at(None, 1, NOW)
        .unwrap()
        .next_cursor
        .expect("two active conversations");
    assert_eq!(
        error_code(store.query_message_page_at(
            CONVERSATION,
            Some(&conversation_cursor),
            1,
            NOW
        )),
        "INVALID_CURSOR"
    );
    // 垃圾文本。
    let garbage = PageCursor::from_text("spg1.zz");
    assert_eq!(
        error_code(store.query_message_page_at(CONVERSATION, Some(&garbage), 1, NOW)),
        "INVALID_CURSOR"
    );
}

#[test]
fn message_page_reports_end_of_stream_and_bounds_the_last_page() {
    let fixture = Fixture::new();
    let store = fixture.open();
    fixture.seed(&store, &[CONVERSATION]);
    let empty = store
        .query_message_page_at(CONVERSATION, None, 10, NOW)
        .unwrap();
    assert!(empty.items.is_empty());
    assert!(empty.next_cursor.is_none());

    add_remote_message(&store, CONVERSATION, "msg-1", 1, NOW + 1, "一");
    let exactly = store
        .query_message_page_at(CONVERSATION, None, 1, NOW)
        .unwrap();
    assert_eq!(exactly.items.len(), 1);
    assert!(
        exactly.next_cursor.is_none(),
        "exactly limit rows means no more pages"
    );

    for index in 2..=MAX_MESSAGE_PAGE_LIMIT as i64 {
        add_remote_message(
            &store,
            CONVERSATION,
            &format!("msg-{index}"),
            index,
            NOW + index,
            "正文",
        );
    }
    let full = store
        .query_message_page_at(CONVERSATION, None, MAX_MESSAGE_PAGE_LIMIT, NOW)
        .unwrap();
    assert_eq!(full.items.len(), MAX_MESSAGE_PAGE_LIMIT as usize);
    assert!(full.next_cursor.is_none());
    assert_eq!(full.items[0].message_id, format!("msg-{MAX_MESSAGE_PAGE_LIMIT}"));
    assert_eq!(full.items[0].state, MessageViewState::Accepted);
    assert_eq!(full.items[0].group_seq, Some(MAX_MESSAGE_PAGE_LIMIT as u64));
    assert_eq!(full.items[0].content, MessageContent::text("正文").unwrap());
    assert!(full.items.iter().all(|item| !item.local_pending));
}

#[test]
fn message_page_fails_closed_on_tampered_content() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    fixture.seed(&store, &[CONVERSATION]);
    add_remote_message(&store, CONVERSATION, "msg-1", 1, NOW + 1, "正文");
    store
        .connection()
        .execute(
            "UPDATE messages SET protected_content = X'00ff00ff' WHERE message_id = 'msg-1'",
            [],
        )
        .unwrap();
    assert_eq!(
        error_code(store.query_message_page_at(CONVERSATION, None, 10, NOW)),
        "STATE_CORRUPTED"
    );

    let draft_id = create_draft(&mut store, CONVERSATION, "op-1", "草稿正文", NOW + 2);
    store
        .connection()
        .execute(
            "UPDATE drafts SET protected_content = X'00ff00ff' WHERE draft_id = ?1",
            [&draft_id],
        )
        .unwrap();
    assert_eq!(
        error_code(store.query_message_page_at(CONVERSATION, None, 10, NOW)),
        "STATE_CORRUPTED"
    );
}

#[test]
fn conversation_page_orders_by_activity_with_stable_tiebreak() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    fixture.seed(
        &store,
        &[
            CONVERSATION,
            CONVERSATION_TWO,
            CONVERSATION_THREE,
            "conv-4",
        ],
    );
    // conv-1 最活跃；conv-2 与 conv-3 有相同的最近活动时间（靠稳定次键消歧）。
    add_remote_message(&store, CONVERSATION, "msg-1", 1, NOW + 500, "最新");
    add_remote_message(&store, CONVERSATION_TWO, "msg-2", 1, NOW + 100, "同时间");
    add_remote_message(&store, CONVERSATION_THREE, "msg-3", 1, NOW + 100, "同时间");
    // conv-4 只有本地意图；conv-1 追加一条待发意图与一个故障任务。
    create_draft(&mut store, "conv-4", "op-4", "意图", NOW + 50);
    create_draft(&mut store, CONVERSATION, "op-1", "待发意图", NOW + 600);
    store
        .connection()
        .execute(
            "INSERT INTO outbox_jobs (job_id, dedup_key, kind, payload_ref, conversation_id, depends_on,
                                      state, retry_count, retry_at, lease_owner, lease_until,
                                      lease_version, cancel_requested)
             VALUES ('job-failed', 'manual:failed', 'prepare', 'op-1', ?1, NULL, 'failed', 3, ?2, NULL, NULL, 0, 0)",
            params![CONVERSATION, NOW],
        )
        .unwrap();

    let page = store
        .query_conversation_page_at(None, 10, NOW + 1000)
        .unwrap();
    let ids: Vec<&str> = page
        .items
        .iter()
        .map(|item| item.conversation_id.as_str())
        .collect();
    assert_eq!(ids, vec![CONVERSATION, CONVERSATION_THREE, CONVERSATION_TWO, "conv-4"]);
    assert!(page.next_cursor.is_none());
    let first = &page.items[0];
    assert_eq!(first.last_activity_at, NOW + 600);
    assert_eq!(first.pending_intents, 1);
    assert_eq!(first.failed_outbox_jobs, 1);
    assert_eq!(first.unknown_attempts, 0);
    assert_eq!(first.unread_count, None, "unread must not be fabricated");
    assert_eq!(page.items[3].last_activity_at, NOW + 50);
    assert_eq!(page.items[3].pending_intents, 1);

    // 翻页使用稳定键集，不重复也不跳页。
    let mut collected = Vec::new();
    let mut cursor = None;
    loop {
        let page = store
            .query_conversation_page_at(cursor.as_ref(), 3, NOW + 1000)
            .unwrap();
        assert!(page.items.len() <= 3);
        for item in &page.items {
            collected.push(item.conversation_id.clone());
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(collected, ids);
    assert_eq!(
        error_code(store.query_conversation_page_at(None, 0, NOW + 1000)),
        "INVALID_INPUT"
    );
    assert_eq!(
        error_code(store.query_conversation_page_at(
            None,
            MAX_CONVERSATION_PAGE_LIMIT + 1,
            NOW + 1000
        )),
        "INVALID_INPUT"
    );
    let message_cursor = store
        .query_message_page_at(CONVERSATION, None, 1, NOW + 1000)
        .unwrap()
        .next_cursor
        .expect("more messages");
    assert_eq!(
        error_code(store.query_conversation_page_at(
            Some(&message_cursor),
            2,
            NOW + 1000
        )),
        "INVALID_CURSOR"
    );
    assert_eq!(
        error_code(store.query_conversation_page_at(
            Some(&PageCursor::from_text("spg1.00")),
            2,
            NOW + 1000
        )),
        "INVALID_CURSOR"
    );
}

#[test]
fn queries_fail_closed_after_the_store_is_marked_faulted() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    fixture.seed(&store, &[CONVERSATION]);
    add_remote_message(&store, CONVERSATION, "msg-1", 1, NOW + 1, "正文");
    let cached = store
        .query_message_page_at(CONVERSATION, None, 10, NOW)
        .unwrap();
    assert_eq!(cached.items.len(), 1);
    store.mark_faulted("simulated rollback failure");
    assert_eq!(
        error_code(store.query_message_page_at(CONVERSATION, None, 10, NOW)),
        "STORAGE_FAILURE",
        "faulted store must not serve cached summaries"
    );
    assert_eq!(
        error_code(store.query_conversation_page_at(None, 10, NOW)),
        "STORAGE_FAILURE"
    );
}

#[test]
fn prepared_attempts_are_exposed_as_pending_projection_with_submission_id() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    fixture.seed(&store, &[CONVERSATION]);
    let draft_id = create_draft(&mut store, CONVERSATION, "op-1", "待发", NOW);
    let draft = store.read_draft(&draft_id).unwrap().unwrap();
    let prepared = PreparedWrite {
        draft_id: draft_id.clone(),
        conversation_id: CONVERSATION.to_owned(),
        submission_id: "submission-1".to_owned(),
        mls_epoch: 3,
        immutable_ciphertext: vec![7u8; 32],
        request_hash: draft.request_hash,
        crypto_changes: Vec::new(),
    };
    let mut transaction = store
        .begin_transaction(WriteScope::attempt(CONVERSATION))
        .unwrap();
    persist_prepared_attempt(&mut transaction, prepared).unwrap();
    commit_transaction(transaction).unwrap();

    let page = store
        .query_message_page_at(CONVERSATION, None, 10, NOW + 1)
        .unwrap();
    assert_eq!(page.items.len(), 1);
    let item = &page.items[0];
    assert_eq!(item.message_id, draft_id);
    assert_eq!(item.state, MessageViewState::Pending);
    assert_eq!(item.group_seq, None);
    assert_eq!(item.submission_id.as_deref(), Some("submission-1"));
    assert!(item.local_pending);
    assert_eq!(item.author_account_id, None);
    assert_eq!(item.content, MessageContent::text("待发").unwrap());
}
