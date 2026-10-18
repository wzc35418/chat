//! CORE-005/006/007/010 竖直切片的端到端测试。
//!
//! 真实临时目录 + 真实加密库：建库 → 建草稿 → 写一条已验证消息 → 分页读回，
//! 另覆盖幂等、冲突、ID 规范校验、关闭后显式失败与重开（含错误密钥拒绝）。

use chat_core::service::{
    ConversationCore, ConversationPageView, DraftView, MessagePageView, error_code,
};
use chat_protocol::{AccountId, ConversationId, DeviceId, MessageId, OperationId};
use chat_storage::formal::VerifiedEntryInput;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MASTER_KEY_HEX: &str = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
const CREATED_AT: i64 = 1_700_000_000_000;

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "chat-core-service-{tag}-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture directory");
        Self { dir }
    }

    fn database(&self) -> PathBuf {
        self.dir.join("chat.db")
    }

    fn database_text(&self) -> String {
        self.database().to_string_lossy().into_owned()
    }

    fn open(&self) -> ConversationCore {
        ConversationCore::open(&self.database_text(), MASTER_KEY_HEX).expect("open core")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn conversation(seed: u8) -> String {
    ConversationId::from_bytes([seed; 32]).encode()
}

fn message(seed: u8) -> String {
    MessageId::from_bytes([seed; 32]).encode()
}

fn operation(seed: u8) -> String {
    OperationId::from_bytes([seed; 32]).encode()
}

fn account(seed: u8) -> String {
    AccountId::from_bytes([seed; 32]).encode()
}

fn device(seed: u8) -> String {
    DeviceId::from_bytes([seed; 32]).encode()
}

fn entry(
    conversation_id: &str,
    message_id: &str,
    author_account: &str,
    author_device: &str,
    body: &str,
    mine: bool,
    created_at_ms: i64,
) -> VerifiedEntryInput {
    VerifiedEntryInput {
        conversation_id: conversation_id.to_owned(),
        title: "产品讨论".to_owned(),
        message_id: message_id.to_owned(),
        author_account: author_account.to_owned(),
        author_device: author_device.to_owned(),
        body: body.to_owned(),
        mine,
        created_at_ms,
    }
}

fn failure_code<T: std::fmt::Debug>(result: anyhow::Result<T>) -> String {
    match result {
        Ok(value) => panic!("expected an explicit failure, got {value:?}"),
        Err(error) => error_code(&error).to_owned(),
    }
}

#[test]
fn end_to_end_draft_then_verified_entry_then_pages() {
    let fixture = Fixture::new("e2e");
    let mut core = fixture.open();
    let conversation_id = conversation(1);
    let operation_id = operation(2);
    let account_id = account(3);
    let device_id = device(4);

    // 1. 建草稿：幂等（同一 operation 只产生一个草稿），正文保持明文只在内存。
    let draft: DraftView = core
        .create_draft(&conversation_id, &operation_id, "第一条草稿")
        .expect("create draft");
    assert_eq!(draft.conversation_id, conversation_id);
    assert_eq!(draft.operation_id, operation_id);
    assert_eq!(draft.body, "第一条草稿");
    assert_eq!(draft.state, "queued");
    assert!(draft.submission_id.is_none());
    assert!(!draft.draft_id.is_empty());
    let replayed = core
        .create_draft(&conversation_id, &operation_id, "第一条草稿")
        .expect("idempotent draft");
    assert_eq!(replayed.draft_id, draft.draft_id);

    // 2. 草稿状态可查；未知草稿返回 None，畸形 ID 显式 INVALID_INPUT。
    let status = core
        .draft_status(&draft.draft_id)
        .expect("draft status")
        .expect("draft exists");
    assert_eq!(status, draft);
    assert!(
        core.draft_status("draft-00000000000000000000000000000000")
            .expect("unknown draft")
            .is_none()
    );
    assert_eq!(
        failure_code(core.draft_status("not a draft id")),
        "INVALID_INPUT"
    );

    // 3. 写一条本机已认证条目（会话行此时已由草稿建立，摘要补标题/预览）。
    let applied = core
        .apply_verified_entry(entry(
            &conversation_id,
            &message(5),
            &account_id,
            &device_id,
            "第一条正式消息",
            true,
            CREATED_AT,
        ))
        .expect("apply verified entry");
    assert_eq!(applied.body, "第一条正式消息");
    assert!(applied.mine);
    assert_eq!(applied.author, account_id);
    assert_eq!(applied.created_at_ms as i64, CREATED_AT);

    // 4. 消息页：本机待发意图在前，正式消息在后；两者都判为 mine。
    let page: MessagePageView = core
        .list_messages(&conversation_id, None, 10)
        .expect("message page");
    assert_eq!(page.items.len(), 2, "pending draft + accepted message");
    assert_eq!(page.items[0].message_id, draft.draft_id);
    assert!(page.items[0].mine);
    assert_eq!(page.items[0].author, account_id);
    assert_eq!(page.items[0].body, "第一条草稿");
    assert_eq!(page.items[1].message_id, message(5));
    assert!(page.items[1].mine);
    assert_eq!(page.items[1].body, "第一条正式消息");
    assert!(page.next_cursor.is_none());

    // 5. 会话页：标题/预览/活动时间来自写入的摘要，未读显式未知。
    let conversations: ConversationPageView = core
        .list_conversations(None, 10)
        .expect("conversation page");
    assert_eq!(conversations.items.len(), 1);
    let view = &conversations.items[0];
    assert_eq!(view.conversation_id, conversation_id);
    assert_eq!(view.title, "产品讨论");
    assert_eq!(view.preview, "第一条正式消息");
    assert_eq!(view.unread_count, None);
    assert!(view.updated_at_ms as i64 >= CREATED_AT);
    assert!(conversations.next_cursor.is_none());
}

#[test]
fn message_pages_are_keyset_paginated_and_bound_cursors() {
    let fixture = Fixture::new("pages");
    let mut core = fixture.open();
    let conversation_id = conversation(11);
    let account_id = account(12);
    let device_id = device(13);
    for index in 0..5u8 {
        core.apply_verified_entry(entry(
            &conversation_id,
            &message(20 + index),
            &account_id,
            &device_id,
            &format!("第 {} 条", index + 1),
            true,
            CREATED_AT + i64::from(index),
        ))
        .expect("apply entry");
    }

    let mut collected: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let page = core
            .list_messages(&conversation_id, cursor.as_deref(), 2)
            .expect("message page");
        assert!(page.items.len() <= 2, "limit is honoured");
        for item in &page.items {
            collected.push(item.message_id.clone());
        }
        pages += 1;
        match page.next_cursor {
            Some(next) => {
                assert!(next.starts_with("spg1"), "storage cursor token: {next}");
                cursor = Some(next);
            }
            None => break,
        }
        assert!(pages < 10, "pagination must terminate");
    }
    assert_eq!(pages, 3);
    assert_eq!(collected.len(), 5);
    assert_eq!(collected[0], message(24), "newest first");
    assert_eq!(collected[4], message(20));
    let mut unique = collected.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 5, "no duplicates across pages");

    // 伪造/跨资源的游标显式失败，绝不退化为“最新页”。
    assert_eq!(
        failure_code(core.list_messages(&conversation_id, Some("spg1.deadbeef"), 2)),
        "INVALID_CURSOR"
    );
    assert_eq!(
        failure_code(core.list_conversations(Some("spg1.deadbeef"), 2)),
        "INVALID_CURSOR"
    );
    // limit 越界由 storage 有界拒绝。
    assert_eq!(
        failure_code(core.list_messages(&conversation_id, None, 0)),
        "INVALID_INPUT"
    );
}

#[test]
fn verified_entry_is_idempotent_and_conflict_is_explicit() {
    let fixture = Fixture::new("idempotent");
    let mut core = fixture.open();
    let conversation_id = conversation(31);
    let account_id = account(32);
    let device_id = device(33);
    let input = entry(
        &conversation_id,
        &message(34),
        &account_id,
        &device_id,
        "同一条消息",
        true,
        CREATED_AT,
    );
    let first = core
        .apply_verified_entry(input.clone())
        .expect("first apply");
    let second = core
        .apply_verified_entry(input)
        .expect("idempotent replay");
    assert_eq!(first, second);
    let page = core
        .list_messages(&conversation_id, None, 10)
        .expect("message page");
    assert_eq!(page.items.len(), 1, "no duplicate rows");

    assert_eq!(
        failure_code(core.apply_verified_entry(entry(
            &conversation_id,
            &message(34),
            &account_id,
            &device_id,
            "被篡改的正文",
            true,
            CREATED_AT,
        ))),
        "MESSAGE_ID_CONFLICT"
    );
}

#[test]
fn identifiers_are_validated_against_the_protocol_contract() {
    let fixture = Fixture::new("ids");
    let mut core = fixture.open();
    let account_id = account(41);
    let device_id = device(42);
    // 非 PROTOCOL-001 文本的会话/操作 ID 在 core 层显式拒绝，而不是写进库。
    assert_eq!(
        failure_code(core.create_draft("conv-1", &operation(43), "正文")),
        "INVALID_INPUT"
    );
    assert_eq!(
        failure_code(core.create_draft(&conversation(44), "op-1", "正文")),
        "INVALID_INPUT"
    );
    assert_eq!(
        failure_code(core.apply_verified_entry(entry(
            &conversation(45),
            "msg-1",
            &account_id,
            &device_id,
            "正文",
            true,
            CREATED_AT,
        ))),
        "INVALID_INPUT"
    );
    // 正文超过线协议上限：显式拒绝。
    let oversized = "x".repeat(chat_protocol::wire::MAX_TEXT_BYTES + 1);
    assert_eq!(
        failure_code(core.create_draft(&conversation(46), &operation(47), &oversized)),
        "INVALID_INPUT"
    );
    // 会话页在没有任何写入时为空，而不是报错。
    let empty = core.list_conversations(None, 10).expect("empty page");
    assert!(empty.items.is_empty());
    assert!(empty.next_cursor.is_none());
}

#[test]
fn close_releases_the_writer_and_open_rejects_bad_keys_or_directories() {
    let fixture = Fixture::new("lifecycle");
    let database = fixture.database_text();
    let core = ConversationCore::open(&database, MASTER_KEY_HEX).expect("open core");
    // 单写者：第二个句柄立即失败（STORAGE_LOCKED），不排队、不重建空库。
    assert_eq!(
        failure_code(ConversationCore::open(&database, MASTER_KEY_HEX)),
        "STORAGE_LOCKED"
    );
    core.close();
    assert!(!core.is_open());
    assert_eq!(failure_code(core.list_conversations(None, 10)), "CORE_CLOSED");

    // close 之后独占锁释放：同密钥可重开，且数据仍可读。
    let mut reopened = ConversationCore::open(&database, MASTER_KEY_HEX).expect("reopen");
    assert!(reopened.is_open());
    let conversation_id = conversation(51);
    reopened
        .apply_verified_entry(entry(
            &conversation_id,
            &message(52),
            &account(53),
            &device(54),
            "重开后仍可写",
            true,
            CREATED_AT,
        ))
        .expect("write after reopen");
    reopened.close();

    // 错误密钥显式失败，绝不重建同名空库。
    let wrong_key = "ff".repeat(32);
    assert_eq!(
        failure_code(ConversationCore::open(&database, &wrong_key)),
        "STATE_CORRUPTED"
    );
    // 畸形密钥与不存在的目录同样显式失败。
    assert_eq!(
        failure_code(ConversationCore::open(&database, "abcd")),
        "INVALID_INPUT"
    );
    assert_eq!(
        failure_code(ConversationCore::open(
            &database.replace("chat.db", "missing-dir/chat.db"),
            MASTER_KEY_HEX
        )),
        "INVALID_INPUT"
    );
    // 只用 opens 的句柄做了失败尝试：原数据仍可用正确密钥完整读出。
    let recovered = ConversationCore::open(&database, MASTER_KEY_HEX).expect("reopen again");
    let page = recovered
        .list_messages(&conversation(51), None, 10)
        .expect("message page after reopen");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].body, "重开后仍可写");
    assert!(page.items[0].mine);
    recovered.close();
}
