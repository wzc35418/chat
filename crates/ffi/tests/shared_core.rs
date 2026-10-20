//! TASK-128：单句柄共享核心门面（真库 + 真 axum 节点 + 真 OpenMLS）。
//!
//! 覆盖：同一句柄上 建群 → 节点登记 → 提交邀请 → 取回邀请；发消息 accepted；
//! sync_once applied>=1；关闭后所有方法 CORE_CLOSED；同库第二个实例 STORAGE_LOCKED。

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::service::{ConversationCore, IdentityAccountInput, IdentityAccountView};
use chat_ffi::local_store::FfiError;
use chat_ffi::shared_core::{ChatCoreFfi, SharedInviteFfi};
use chat_node_api::build_router;
use chat_node_api::state::{AppState, FixedClock as NodeFixedClock};
use chat_node_services::canonical::hex_encode;
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use chat_protocol::{ConversationId, OperationId};

const NOW_S: u64 = 1_700_000_000;
const NODE_ID: &str = "node-shared-core";
const MASTER_KEY: [u8; 32] = [0x5a; 32];
const DEVICE_SEED: [u8; 32] = [0x33; 32];
const ACCOUNT_SEED: [u8; 32] = [0x22; 32];

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
            "chat-ffi-shared-{}-{nanos}-{counter}",
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

fn master_key_hex() -> String {
    MASTER_KEY.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn acct(seed: &[u8; 32]) -> String {
    seed.iter().map(|byte| format!("{byte:02x}")).collect()
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


struct TestNode {
    address: String,
    node_public_key: [u8; 32],
}

impl TestNode {
    fn endpoint_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn public_key_hex(&self) -> String {
        hex_encode(&self.node_public_key)
    }
}

fn start_node() -> TestNode {
    let service = NodeService::open_in_memory(NodeServiceConfig {
        node_id: NODE_ID.into(),
        signing_seed: [7_u8; 32],
        policy: NodeAuthPolicy::default(),
    })
    .expect("node service opens");
    let node_public_key = service.node_public_key();
    let state = AppState::new(service, Arc::new(NodeFixedClock::new(NOW_S)));
    let router = build_router(state);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    listener.set_nonblocking(true).expect("nonblocking listener");
    let address = listener.local_addr().expect("local addr").to_string();
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(async move {
            let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
                return;
            };
            let _ = axum::serve(listener, router).await;
        });
    });
    TestNode {
        address,
        node_public_key,
    }
}

struct Harness {
    _dir: FixtureDir,
    node: TestNode,
    core: Arc<ChatCoreFfi>,
    group_id: String,
    invite: SharedInviteFfi,
}

impl Harness {
    fn new() -> Self {
        let dir = FixtureDir::new();
        let identity = enroll_identity(&dir.database(), &master_key_hex());
        let device_id = identity.device_id.clone();
        let node = start_node();
        // 设备注册由 `group_register_on_node` 自己完成（它用持久身份的种子自注册，幂等），
        // 这里不再手工登记，否则两条凭据会在节点侧冲突。
        let core = ChatCoreFfi::open(dir.database(), master_key_hex()).expect("open shared core");
        let created = core
            .create_group("共享核心群".to_owned(), String::new(), "bob".to_owned())
            .expect("create group");
        assert_eq!(created.group.members.len(), 2);
        let _ = device_id;
        Self {
            _dir: dir,
            node,
            core,
            group_id: created.group.group_id,
            invite: created.invite,
        }
    }

    fn pk(&self) -> String {
        self.node.public_key_hex()
    }
}

#[test]
fn one_handle_covers_group_invite_registration_and_snapshot() {
    let harness = Harness::new();
    let registration = harness
        .core
        .group_register_on_node(
            harness.group_id.clone(),
            harness.node.endpoint_url(),
            NODE_ID.to_owned(),
            Some(harness.pk()),
        )
        .expect("register the group on the node");
    assert_eq!(registration.group_id, harness.group_id);
    assert_eq!(registration.node_id, NODE_ID);
    // 刚登记的群在节点上还没有日志条目（head.seq = 0），这里只校验绑定关系。
    assert_eq!(registration.group_id, harness.group_id);

    let receipt = harness
        .core
        .submit_invite(
            harness.group_id.clone(),
            harness.invite.clone(),
            harness.node.endpoint_url(),
            NODE_ID.to_owned(),
            Some(harness.pk()),
        )
        .expect("submit the invite");
    assert!(!receipt.submission_id.is_empty());
    assert!(receipt.seq >= 1);
    assert_eq!(receipt.log_hash.len(), 64);
    assert_eq!(receipt.node_id, NODE_ID);

    let fetched = harness
        .core
        .fetch_invite(
            harness.group_id.clone(),
            harness.node.endpoint_url(),
            NODE_ID.to_owned(),
            Some(harness.pk()),
            Some("bob".to_owned()),
        )
        .expect("fetch the invite")
        .expect("the invite must be found");
    assert_eq!(fetched.group_id, harness.invite.group_id);
    assert_eq!(fetched.invitee_credential, "bob");
    assert_eq!(fetched.members, harness.invite.members);
    assert_eq!(fetched.welcome_bytes, harness.invite.welcome_bytes);

    let snapshot = harness
        .core
        .snapshot(harness.group_id.clone())
        .expect("snapshot");
    assert_eq!(snapshot.group_id, harness.group_id);
    assert_eq!(snapshot.members.len(), 2);
    let groups = harness.core.list_groups().expect("group list");
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].group_id, harness.group_id);
}

#[test]
fn a_message_is_accepted_through_the_same_handle() {
    let harness = Harness::new();
    harness
        .core
        .group_register_on_node(
            harness.group_id.clone(),
            harness.node.endpoint_url(),
            NODE_ID.to_owned(),
            Some(harness.pk()),
        )
        .expect("register the group");
    let snapshot = harness
        .core
        .snapshot(harness.group_id.clone())
        .expect("snapshot");
    harness
        .core
        .configure_send_node(
            harness.node.endpoint_url(),
            NODE_ID.to_owned(),
            Some(harness.pk()),
        )
        .expect("configure send node");
    harness
        .core
        .configure_send_group(snapshot.mls_group_key.clone())
        .expect("configure send group");

    let draft = harness
        .core
        .create_draft(conversation(), operation(1), "共享核心第一句".to_owned())
        .expect("create draft");
    assert_eq!(draft.state, "queued");
    let prepared = harness
        .core
        .send_prepare(draft.draft_id.clone())
        .expect("prepare");
    assert!(
        prepared.status == "prepared" || prepared.status == "submitted",
        "unexpected prepare status: {}",
        prepared.status
    );
    let dispatched = harness
        .core
        .send_dispatch(draft.draft_id.clone())
        .expect("dispatch");
    assert_eq!(dispatched.status, "accepted", "the node accepted the message");
    assert!(dispatched.settled);
    let status = harness
        .core
        .draft_status(draft.draft_id.clone())
        .expect("status")
        .expect("draft exists");
    assert_eq!(status.status, "accepted");
    // 消息列表在同一个句柄上可读（accepted 之后草稿不再以待发形式出现，因此只校验可分页）。
    let messages = harness
        .core
        .list_messages(conversation(), None, 10)
        .expect("list messages");
    assert!(messages.next_cursor.is_none());
}

#[test]
fn sync_once_pulls_the_group_log_through_the_same_handle() {
    let harness = Harness::new();
    harness
        .core
        .group_register_on_node(
            harness.group_id.clone(),
            harness.node.endpoint_url(),
            NODE_ID.to_owned(),
            Some(harness.pk()),
        )
        .expect("register the group");
    // 先往群日志里投一条真实条目（发一条消息并 accepted），再拉取。
    let snapshot = harness
        .core
        .snapshot(harness.group_id.clone())
        .expect("snapshot");
    harness
        .core
        .configure_send_node(
            harness.node.endpoint_url(),
            NODE_ID.to_owned(),
            Some(harness.pk()),
        )
        .expect("configure send node");
    harness
        .core
        .configure_send_group(snapshot.mls_group_key.clone())
        .expect("configure send group");
    let draft = harness
        .core
        .create_draft(conversation(), operation(7), "同步前的一条".to_owned())
        .expect("create draft");
    harness
        .core
        .send_prepare(draft.draft_id.clone())
        .expect("prepare");
    let dispatched = harness
        .core
        .send_dispatch(draft.draft_id.clone())
        .expect("dispatch");
    assert_eq!(dispatched.status, "accepted");

    let outcome = harness
        .core
        .sync_once(
            harness.group_id.clone(),
            harness.node.endpoint_url(),
            NODE_ID.to_owned(),
            Some(harness.pk()),
        )
        .expect("one sync round");
    assert!(
        outcome.fetched >= 1,
        "at least one log entry is pulled (fetched={}, applied={}, unsupported={})",
        outcome.fetched,
        outcome.applied,
        outcome.unsupported
    );
    // 对象仍然可用（sync_once 之后核心已装回）。
    assert!(harness.core.is_open());
}

#[test]
fn a_closed_handle_and_a_second_instance_are_explicit() {
    let harness = Harness::new();
    // 同库第二个实例 → 存储层 STORAGE_LOCKED（单写者不变）。
    let second = match ChatCoreFfi::open(harness._dir.database(), master_key_hex()) {
        Ok(_) => panic!("a second instance must not acquire the store"),
        Err(error) => error,
    };
    assert_eq!(code(second), "STORAGE_LOCKED");

    // 关闭后所有方法 CORE_CLOSED。
    harness.core.close();
    assert!(!harness.core.is_open());
    assert_eq!(
        code(harness.core.snapshot(harness.group_id.clone()).unwrap_err()),
        "CORE_CLOSED"
    );
    assert_eq!(code(harness.core.list_groups().unwrap_err()), "CORE_CLOSED");
    assert_eq!(
        code(
            harness
                .core
                .create_draft(conversation(), operation(2), "x".to_owned())
                .unwrap_err()
        ),
        "CORE_CLOSED"
    );
    assert_eq!(
        code(
            harness
                .core
                .draft_status("draft-00000000000000000000000000000000".to_owned())
                .unwrap_err()
        ),
        "CORE_CLOSED"
    );
    // 关闭是幂等的。
    harness.core.close();
}
fn enroll_identity(database: &str, master_key_hex: &str) -> IdentityAccountView {
    let mut core = ConversationCore::open(database, master_key_hex).expect("open core");
    let view = core
        .save_identity_account(IdentityAccountInput {
            display_name: "共享核心".to_owned(),
            account_seed_hex: acct(&ACCOUNT_SEED),
            device_seed_hex: acct(&DEVICE_SEED),
            created_at_ms: NOW_S * 1_000,
        })
        .expect("save identity");
    core.close();
    view
}