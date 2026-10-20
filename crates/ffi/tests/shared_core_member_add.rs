//! GROUPS-021：**单句柄 ChatCoreFfi** 完成「B 自注册 → A member_add → B 读日志 200」。
//!
//! 证明不需要 GroupFfi（那会和 ChatCore 抢同一个库 → STORAGE_LOCKED）。
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::service::{ConversationCore, IdentityAccountInput};
use chat_ffi::shared_core::ChatCoreFfi;
use chat_node_api::build_router;
use chat_node_api::state::{AppState, FixedClock as NodeFixedClock};
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_node_services::canonical::hex_encode;
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, StoreOptions, open_store};
use ed25519_dalek::SigningKey;

const NOW_S: u64 = 1_700_000_000;
const NODE_ID: &str = "node-shared-member";
const MASTER_KEY: [u8; 32] = [0x7e; 32];

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
            "chat-ffi-shared-member-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("fixture dir");
        Self(dir)
    }

    fn database(&self) -> PathBuf {
        self.0.join("chat.db")
    }

    fn database_text(&self) -> String {
        self.database().to_string_lossy().into_owned()
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

fn seed_hex(seed: &[u8; 32]) -> String {
    seed.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(future)
}

struct TestNode {
    address: String,
    node_public_key: [u8; 32],
}

impl TestNode {
    fn start() -> Self {
        let service = NodeService::open_in_memory(NodeServiceConfig {
            node_id: NODE_ID.into(),
            signing_seed: [7u8; 32],
            policy: NodeAuthPolicy::default(),
        })
        .expect("node service");
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
        Self {
            address,
            node_public_key,
        }
    }

    fn endpoint_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn public_key_hex(&self) -> String {
        hex_encode(&self.node_public_key)
    }

    fn client(&self, device_id: &str, seed: [u8; 32]) -> NodeClient {
        let endpoint = NodeEndpoint::new(&self.endpoint_url(), NODE_ID)
            .expect("endpoint")
            .with_node_public_key(self.node_public_key);
        NodeClient::new(endpoint, device_id.to_owned(), SigningKey::from_bytes(&seed))
            .expect("node client")
    }
}

/// 建库 + 持久身份（顺序使用单写者：建完就关）。
fn enroll(dir: &FixtureDir, device_seed: [u8; 32]) -> String {
    let mut core = ConversationCore::open(&dir.database_text(), &master_key_hex()).expect("open core");
    let view = core
        .save_identity_account(IdentityAccountInput {
            display_name: "shared".to_owned(),
            account_seed_hex: seed_hex(&[0x24u8; 32]),
            device_seed_hex: seed_hex(&device_seed),
            created_at_ms: NOW_S * 1_000,
        })
        .expect("save identity");
    core.close();
    view.device_id
}

/// 建一个本地群（真实 MLS），返回协议群 ID。
fn create_local_group(dir: &FixtureDir) -> String {
    let mut store = open_store(
        StoreOptions::new(dir.database(), KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes(MASTER_KEY),
    )
    .expect("open store");
    let mut alice = chat_crypto::engine::MlsDevice::new("alice").expect("alice");
    let bob = chat_crypto::engine::MlsDevice::new("bob").expect("bob");
    let request = chat_core::groups::CreateGroupRequest::new("shared-group", "alice");
    let peer = chat_core::groups::GroupBootstrapPeer {
        credential: "bob".to_owned(),
        device: &bob,
    };
    let (view, _welcome) =
        chat_core::groups::create_group_with_welcome(&mut alice, &peer, &mut store, &request)
            .expect("create group");
    view.group_id
}

#[test]
fn the_single_handle_chat_core_can_self_register_and_add_a_member() {
    let node = TestNode::start();

    // A：身份 + 本地群 + 节点登记（全部单写者顺序完成）。
    let dir_a = FixtureDir::new();
    let _device_a = enroll(&dir_a, [0x51u8; 32]);
    let group_id = create_local_group(&dir_a);
    let core_a = ChatCoreFfi::open(dir_a.database_text(), master_key_hex()).expect("chat core a");
    core_a
        .group_register_on_node(
            group_id.clone(),
            node.endpoint_url(),
            NODE_ID.to_owned(),
            Some(node.public_key_hex()),
        )
        .expect("group registers on the node");

    // B：独立库 + 持久身份；用**同一个 ChatCoreFfi 句柄**自注册到节点。
    let dir_b = FixtureDir::new();
    let device_b = enroll(&dir_b, [0x52u8; 32]);
    let core_b = ChatCoreFfi::open(dir_b.database_text(), master_key_hex()).expect("chat core b");
    core_b
        .register_device_on_node(
            node.endpoint_url(),
            NODE_ID.to_owned(),
            Some(node.public_key_hex()),
        )
        .expect("B self-registers through the single handle");
    // 幂等：再注册一次仍然成功。
    core_b
        .register_device_on_node(
            node.endpoint_url(),
            NODE_ID.to_owned(),
            Some(node.public_key_hex()),
        )
        .expect("self-registration is idempotent");

    // B 还不是群成员：读日志被拒。
    let bob_client = node.client(&device_b, [0x52u8; 32]);
    assert!(
        block_on(bob_client.fetch_log(&group_id, 0, 10)).is_err(),
        "a non-member must not read the log"
    );

    // A 通过**同一个 ChatCoreFfi** 提交成员变更。
    let receipt = core_a
        .submit_member_add(
            group_id.clone(),
            device_b.clone(),
            node.endpoint_url(),
            NODE_ID.to_owned(),
            Some(node.public_key_hex()),
        )
        .expect("member add is accepted");
    assert!(!receipt.submission_id.is_empty());
    assert_eq!(receipt.node_id, NODE_ID);

    // 现在 B 能读日志。
    let page = block_on(bob_client.fetch_log(&group_id, 0, 10))
        .expect("the member is authorized after the member-add commit");
    assert_eq!(page.group_id, group_id);

    // 重复提交幂等（同一回执）。
    let again = core_a
        .submit_member_add(
            group_id.clone(),
            device_b,
            node.endpoint_url(),
            NODE_ID.to_owned(),
            Some(node.public_key_hex()),
        )
        .expect("repeat member add is idempotent");
    assert_eq!(again.submission_id, receipt.submission_id);
    assert_eq!(again.seq, receipt.seq);
}

