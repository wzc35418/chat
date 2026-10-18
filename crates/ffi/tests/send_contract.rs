//! FFI 发送链路契约：方法名、返回投影与错误码保真（不含正文/密钥）。
//!
//! 这些用例不需要真节点：它们锁的是 Kotlin 侧能观察到的契约——
//! 未配置节点时一律 `NODE_UNAVAILABLE`、配置只保存不发请求、未知草稿/未入库身份显式失败、
//! 关闭后 `CORE_CLOSED`，且任何失败都不会让对象变成不可用（除非已关闭）。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_ffi::local_store::{FfiError, LocalStoreFfi};
use chat_protocol::{ConversationId, OperationId};

const MASTER_KEY: [u8; 32] = [0x5a; 32];
const UNKNOWN_DRAFT: &str = "draft-00000000000000000000000000000000";

fn master_key_hex() -> String {
    MASTER_KEY.iter().map(|byte| format!("{byte:02x}")).collect()
}

struct FixtureDir(PathBuf);

impl FixtureDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "chat-ffi-send-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> String {
        self.0.join("chat.db").to_string_lossy().into_owned()
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn store(dir: &FixtureDir) -> std::sync::Arc<LocalStoreFfi> {
    LocalStoreFfi::open(dir.database(), master_key_hex()).expect("open local store")
}

fn conversation() -> String {
    ConversationId::from_bytes([0x11; 32]).encode()
}

fn operation(seed: u8) -> String {
    OperationId::from_bytes([seed; 32]).encode()
}

fn code(error: FfiError) -> String {
    match error {
        FfiError::Store { code, .. } => code,
    }
}

#[test]
fn send_steps_require_a_configured_node() {
    let dir = FixtureDir::new();
    let store = store(&dir);
    let draft = store
        .create_draft(conversation(), operation(1), "hello".to_owned())
        .expect("draft");

    // 未配置节点：四个动作都返回稳定错误码，绝不伪造成功。
    assert_eq!(
        code(store.send_prepare(draft.draft_id.clone()).unwrap_err()),
        "NODE_UNAVAILABLE"
    );
    assert_eq!(
        code(store.send_dispatch(draft.draft_id.clone()).unwrap_err()),
        "NODE_UNAVAILABLE"
    );
    assert_eq!(
        code(store.send_retry(draft.draft_id.clone()).unwrap_err()),
        "NODE_UNAVAILABLE"
    );
    assert_eq!(
        code(store.send_reconcile(draft.draft_id.clone()).unwrap_err()),
        "NODE_UNAVAILABLE"
    );
    // 草稿仍可查询（失败没有破坏对象状态）。
    let status = store
        .draft_status_view(draft.draft_id)
        .expect("status")
        .expect("draft exists");
    assert_eq!(status.status, "queued");
}

#[test]
fn configure_send_node_validates_without_contacting_the_network() {
    let dir = FixtureDir::new();
    let store = store(&dir);

    // 端点不是可解析的 http(s) 地址 → INVALID_TARGET。
    assert_eq!(
        code(
            store
                .configure_send_node("not-a-url".to_owned(), "node-a".to_owned(), None)
                .unwrap_err()
        ),
        "INVALID_TARGET"
    );
    // 节点公钥必须正好 64 位十六进制 → INVALID_SIGNATURE。
    assert_eq!(
        code(
            store
                .configure_send_node(
                    "http://127.0.0.1:1".to_owned(),
                    "node-a".to_owned(),
                    Some("zz".to_owned())
                )
                .unwrap_err()
        ),
        "INVALID_SIGNATURE"
    );
    // 群键是有界不透明文本：空串与超长都显式拒绝。
    assert_eq!(
        code(store.configure_send_group(String::new()).unwrap_err()),
        "INVALID_INPUT"
    );
    assert_eq!(
        code(store.configure_send_group("a".repeat(129)).unwrap_err()),
        "INVALID_INPUT"
    );

    // 合法配置成功（只保存，不发任何请求）。
    store
        .configure_send_node("http://127.0.0.1:1".to_owned(), "node-a".to_owned(), None)
        .expect("valid configuration is stored");
    store
        .configure_send_group("0011223344556677".to_owned())
        .expect("opaque group key is stored");
    // 配置之后对象仍可用（配置本身不联网、不改草稿状态）。
    assert!(store.is_open());
    store
        .create_draft(conversation(), operation(2), "still usable".to_owned())
        .expect("draft after configuring");
}

#[test]
fn configured_but_unenrolled_database_fails_explicitly() {
    let dir = FixtureDir::new();
    let store = store(&dir);
    store
        .configure_send_node("http://127.0.0.1:1".to_owned(), "node-a".to_owned(), None)
        .expect("configure");
    let draft = store
        .create_draft(conversation(), operation(3), "hello".to_owned())
        .expect("draft");

    // 本库没有入库身份：显式失败（不是伪造 accepted，也不是 panic）。
    assert_eq!(
        code(store.send_prepare(draft.draft_id.clone()).unwrap_err()),
        "INVALID_STATE_TRANSITION"
    );
    // 句柄已放回：状态查询与新建草稿都还能用。
    assert!(store.is_open());
    assert!(
        store
            .draft_status_view(draft.draft_id)
            .expect("status")
            .is_some()
    );
    store
        .create_draft(conversation(), operation(4), "second".to_owned())
        .expect("draft after a failed send step");
}

#[test]
fn unknown_draft_is_rejected_with_a_precise_code() {
    let dir = FixtureDir::new();
    let store = store(&dir);
    store
        .configure_send_node("http://127.0.0.1:1".to_owned(), "node-a".to_owned(), None)
        .expect("configure");

    assert_eq!(
        code(
            store
                .send_dispatch(UNKNOWN_DRAFT.to_owned())
                .unwrap_err()
        ),
        "INVALID_TARGET"
    );
    // 结构非法的草稿 ID（含空格）→ INVALID_INPUT；合法形状但不存在 → INVALID_TARGET。
    assert_eq!(
        code(store.send_prepare("draft with space".to_owned()).unwrap_err()),
        "INVALID_INPUT"
    );
    assert_eq!(
        code(store.send_prepare("not-a-draft".to_owned()).unwrap_err()),
        "INVALID_TARGET"
    );
}

#[test]
fn send_after_shutdown_reports_closed() {
    let dir = FixtureDir::new();
    let store = store(&dir);
    let draft = store
        .create_draft(conversation(), operation(5), "hello".to_owned())
        .expect("draft");
    store
        .configure_send_node("http://127.0.0.1:1".to_owned(), "node-a".to_owned(), None)
        .expect("configure");
    store.shutdown();
    assert!(!store.is_open());
    assert_eq!(
        code(store.send_dispatch(draft.draft_id).unwrap_err()),
        "CORE_CLOSED"
    );
}
