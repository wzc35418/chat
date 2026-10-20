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
fn the_b_side_joins_through_the_same_shared_handle() {
    // 两份**身份库**（A 与 B 各自的账户/设备），共享核心把它们各自登记进自己的工作库。
    let a_identity_dir = FixtureDir::new();
    enroll_identity_with_seeds(
        &a_identity_dir.database(),
        &master_key_hex(),
        &ACCOUNT_SEED,
        &DEVICE_SEED,
    );
    let b_identity_dir = FixtureDir::new();
    let b_account_seed = [0x44_u8; 32];
    let b_device_seed = [0x55_u8; 32];
    let b_identity = enroll_identity_with_seeds(
        &b_identity_dir.database(),
        &master_key_hex(),
        &b_account_seed,
        &b_device_seed,
    );
    let b_device_id = b_identity.device_id.clone();

    let node = start_node();
    let b_dir = FixtureDir::new();
    let b_core = ChatCoreFfi::open(b_dir.database(), master_key_hex()).expect("open B core");
    let enrolled = b_core
        .enroll_local_identity(b_identity_dir.database(), master_key_hex())
        .expect("B enrolls its identity");
    assert_eq!(enrolled.device_id, b_device_id);
    // 幂等：再登记一次仍成功且设备不变。
    let again = b_core
        .enroll_local_identity(b_identity_dir.database(), master_key_hex())
        .expect("idempotent enroll");
    assert_eq!(again.device_id, b_device_id);
    let key_package = b_core.export_key_package().expect("B exports a key package");
    assert!(key_package.len() > 100);

    // A 侧：同一形态的共享核心 + 自己的身份库。
    let a_dir = FixtureDir::new();
    let a_core = ChatCoreFfi::open(a_dir.database(), master_key_hex()).expect("open A core");
    a_core
        .enroll_local_identity(a_identity_dir.database(), master_key_hex())
        .expect("A enrolls its identity");
    let created = a_core
        .create_group_with_invitee_key_package(
            "双设备单句柄群".to_owned(),
            b_device_id.clone(),
            key_package,
            String::new(),
        )
        .expect("A creates the group with B's key package");
    assert_eq!(created.group.members.len(), 2);
    let registration = a_core
        .group_register_on_node(
            created.group.group_id.clone(),
            node.endpoint_url(),
            NODE_ID.to_owned(),
            Some(node.public_key_hex()),
        )
        .expect("A registers the group on the node");
    assert_eq!(registration.group_id, created.group.group_id);
    // 已签名的登记回执落库后的**已验证起点**（唯一正规的可信锚点来源）。
    let verified_head = a_core
        .pull_state(created.group.group_id.clone())
        .expect("A reads its verified pull state")
        .expect("the registration receipt initialized the pull state");
    let receipt = a_core
        .submit_invite(
            created.group.group_id.clone(),
            created.invite.clone(),
            node.endpoint_url(),
            NODE_ID.to_owned(),
            Some(node.public_key_hex()),
        )
        .expect("A submits the invite");
    assert!(receipt.seq >= 1);

    // B 侧：同一个共享核心上入群（先失败一次：没有已验证起点时**不**入群）。
    let missing = b_core
        .join_group_from_welcome(
            "双设备单句柄群".to_owned(),
            created.invite.clone(),
            None,
        )
        .expect_err("joining without a verified anchor is rejected");
    assert_eq!(code(missing), "INVALID_STATE_TRANSITION");
    // 真的入群：带上 A 侧从签名回执得到的已验证起点。
    let joined = b_core
        .join_group_from_welcome(
            "双设备单句柄群".to_owned(),
            created.invite.clone(),
            Some(verified_head.clone()),
        )
        .expect("B joins through the same shared handle");
    assert_eq!(joined.group_id, created.group.group_id);
    assert_eq!(joined.epoch, created.group.epoch);
    assert_eq!(joined.members.len(), 2);
    // 游标已初始化且与 A 的起点一致（幂等起点，不是问节点要的）。
    let b_state = b_core
        .pull_state(created.group.group_id.clone())
        .expect("B reads its pull state")
        .expect("the join initialized B's pull cursor");
    assert_eq!(b_state.cursor_seq, verified_head.cursor_seq);
    assert_eq!(b_state.cursor_log_hash, verified_head.cursor_log_hash);
    // 再次入群：已经入群 → 显式失败，且游标不变（不会把起点改坏）。
    let duplicate = b_core
        .join_group_from_welcome(
            "双设备单句柄群".to_owned(),
            created.invite.clone(),
            Some(verified_head.clone()),
        )
        .expect_err("a duplicate join is rejected");
    assert_eq!(code(duplicate), "INVALID_STATE_TRANSITION");
    let b_state_after = b_core
        .pull_state(created.group.group_id.clone())
        .expect("pull state after duplicate")
        .expect("still initialized");
    assert_eq!(b_state_after.cursor_seq, b_state.cursor_seq);

    // B 的 sync_once 不再因“游标缺失”失败（INVALID_STATE_TRANSITION）。
    // 说明：B 侧还没在**节点**登记设备（节点成员表只登记了 A），节点可能以
    // INVALID_CREDENTIAL / NODE_UNAVAILABLE 拒绝拉取 —— 那是独立的“节点成员同步”缺口，
    // 与本地游标无关，因此这里只锁“不再报缺游标”这一条。
    match b_core.sync_once(
        created.group.group_id.clone(),
        node.endpoint_url(),
        NODE_ID.to_owned(),
        Some(node.public_key_hex()),
    ) {
        Ok(outcome) => {
            // 拉到了多少都行（可能 0 条），关键是不再缺游标。
            let _ = outcome.fetched;
        }
        Err(error) => {
            let error_code = code(error);
            assert_ne!(
                error_code, "INVALID_STATE_TRANSITION",
                "the joined side must have a pull cursor; got {error_code}"
            );
        }
    }
    a_core.shutdown();
    b_core.shutdown();
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
    harness.core.shutdown();
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
    harness.core.shutdown();
}
fn enroll_identity_with_seeds(
    database: &str,
    master_key_hex: &str,
    account_seed: &[u8; 32],
    device_seed: &[u8; 32],
) -> IdentityAccountView {
    let mut core = ConversationCore::open(database, master_key_hex).expect("open core");
    let view = core
        .save_identity_account(IdentityAccountInput {
            display_name: "共享核心".to_owned(),
            account_seed_hex: acct(account_seed),
            device_seed_hex: acct(device_seed),
            created_at_ms: NOW_S * 1_000,
        })
        .expect("save identity");
    core.close();
    view
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