//! SND 端到端：真 axum 节点 + 真 OpenMLS 设备 + 持久 MLS 群状态 + 持久身份。
//!
//! 覆盖：建草稿 → prepare（恰好加密一次）→ dispatch（节点正式接受）→ 节点日志密文与
//! 本地耐久密文一致、对端真 MLS 设备能解出原文；重复 dispatch 幂等零重发；
//! 未配置节点 / 未入库身份 / 未加入群 / 未知草稿分别返回精确冻结码，且句柄可继续使用。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::send_runtime::protocol_group_id_for;
use chat_core::service::{ConversationCore, IdentityAccountInput, error_code};
use chat_crypto::engine::MlsDevice;
use chat_node_api::build_router;
use chat_node_api::state::{AppState, FixedClock as NodeFixedClock};
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_node_services::canonical::hex_encode;
use chat_node_services::group::{GroupCreation, GroupMemberSpec};
use chat_node_services::registration::{
    DeviceCredential, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest, RegistrationCapability,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use chat_protocol::signing::signing_key_from_seed;
use chat_protocol::{ConversationId, OperationId};
use chat_storage::formal::{
    KeyProvenance, ProtectedStorageKey, StoreOptions, open_store,
};
use ed25519_dalek::{Signature, Signer, SigningKey};

const NODE_ID: &str = "node-core-send-test";
const OWNER_ROLE: &str = "owner";
const ACCOUNT_SEED: [u8; 32] = [0x31; 32];
const DEVICE_SEED: [u8; 32] = [0x21; 32];
const MASTER_KEY: [u8; 32] = [0x5a; 32];
const BODY: &str = "hello from the send chain";
const NOW_S: u64 = 1_700_000_000;

// ---------------------------------------------------------------- 真实节点夹具

struct TestNode {
    address: String,
    node_public_key: [u8; 32],
    supervisor: SigningKey,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
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
    let (shutdown, stop) = tokio::sync::oneshot::channel();
    let thread = std::thread::spawn(move || {
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
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = stop.await;
                })
                .await;
        });
    });
    TestNode {
        address,
        node_public_key,
        supervisor: SigningKey::from_bytes(&[11_u8; 32]),
        shutdown: Some(shutdown),
        thread: Some(thread),
    }
}

impl TestNode {
    fn endpoint(&self) -> NodeEndpoint {
        NodeEndpoint::new(&format!("http://{}", self.address), NODE_ID)
            .expect("endpoint")
            .with_node_public_key(self.node_public_key)
    }

    fn client(&self, device_id: &str, key: &SigningKey) -> NodeClient {
        NodeClient::new(self.endpoint(), device_id, key.clone()).expect("node client")
    }

    fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn public_key_hex(&self) -> String {
        hex_encode(&self.node_public_key)
    }

    fn credential(&self, account_id: &str, device_id: &str, key: &SigningKey) -> DeviceCredential {
        let mut credential = DeviceCredential {
            account_id: account_id.to_owned(),
            device_id: device_id.to_owned(),
            device_public_key: hex_encode(&key.verifying_key().to_bytes()),
            endpoint_id: "endpoint-core".into(),
            credential_version: 1,
            not_before: NOW_S - 10,
            expires_at: NOW_S + 3_600,
            supervisor_public_key: hex_encode(&self.supervisor.verifying_key().to_bytes()),
            signature: String::new(),
        };
        let signature: Signature = self
            .supervisor
            .sign(&credential.signing_bytes().expect("credential bytes"));
        credential.signature = hex_encode(&signature.to_bytes());
        credential
    }

    fn capability(&self, account_id: &str, capability_id: &str, device_id: &str) -> RegistrationCapability {
        let mut capability = RegistrationCapability {
            capability_id: capability_id.to_owned(),
            account_id: account_id.to_owned(),
            device_id: device_id.to_owned(),
            purpose: REGISTRATION_CAPABILITY_PURPOSE.into(),
            issued_at: NOW_S - 10,
            expires_at: NOW_S + 600,
            revocation_version: 1,
            supervisor_public_key: hex_encode(&self.supervisor.verifying_key().to_bytes()),
            signature: String::new(),
        };
        let signature: Signature = self
            .supervisor
            .sign(&capability.signing_bytes().expect("capability bytes"));
        capability.signature = hex_encode(&signature.to_bytes());
        capability
    }
}

impl Drop for TestNode {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn conversation() -> String {
    ConversationId::from_bytes([0x11; 32]).encode()
}

fn operation(seed: u8) -> String {
    OperationId::from_bytes([seed; 32]).encode()
}

// ---------------------------------------------------------------- 本地夹具

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
            "chat-core-send-runtime-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn db(&self) -> PathBuf {
        self.0.join("chat.db")
    }

    fn db_text(&self) -> String {
        self.db().to_string_lossy().into_owned()
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn master_key_hex() -> String {
    hex_encode(&MASTER_KEY)
}

fn open_formal(path: &Path) -> chat_storage::formal::Store {
    open_store(
        StoreOptions::new(path, KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes(MASTER_KEY),
    )
    .expect("open formal store")
}

fn hex_to_bytes(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    assert!(bytes.len() % 2 == 0, "hex text has an odd length");
    bytes
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16).expect("hex digit") as u8;
            let low = (pair[1] as char).to_digit(16).expect("hex digit") as u8;
            (high << 4) | low
        })
        .collect()
}

/// 在本地库里写入身份 + MLS 群状态，并在节点上注册设备、用派生群 ID 建群。
///
/// 返回（账户 ID，设备 ID，MLS 群键，对端 MLS 设备）。
async fn seed_local_state(dir: &FixtureDir, node: &TestNode) -> (String, String, String, MlsDevice) {
    // 身份走内核的正式入口（派生账户/设备 ID 并封装种子），随后关闭以释放独占写锁。
    let (account_id, device_id) = {
        let mut core =
            ConversationCore::open(&dir.db_text(), &master_key_hex()).expect("open core");
        let view = core
            .save_identity_account(IdentityAccountInput {
                display_name: "Alice".into(),
                account_seed_hex: hex_encode(&ACCOUNT_SEED),
                device_seed_hex: hex_encode(&DEVICE_SEED),
                created_at_ms: 1_700_000_000_000,
            })
            .expect("identity saved");
        core.close();
        (view.account_id, view.device_id)
    };
    let mut store = open_formal(&dir.db());

    // 真 OpenMLS：alice 建群邀请 bob，bob 真加入；只把 alice 的群状态写进库。
    let mut alice = MlsDevice::new("alice").expect("alice device");
    let mut bob = MlsDevice::new("bob").expect("bob device");
    let welcome = alice
        .create_and_invite(&bob.key_package().expect("bob key package"), &bob)
        .expect("create and invite");
    bob.join(&welcome, &alice).expect("bob joins");
    let group_key = alice.group_key().expect("alice group key");
    alice.export_to_store(&mut store).expect("mls state exported");

    // 节点侧：注册本机设备（签名钥与本地身份同源）+ 用派生群 ID 建群。
    let signing_key = signing_key_from_seed(&DEVICE_SEED);
    let group_id = protocol_group_id_for(&group_key).expect("protocol group id");
    let client = node.client(&device_id, &signing_key);
    client
        .register_device(&RegisterDeviceRequest {
            account_id: account_id.clone(),
            credential: node.credential(&account_id, &device_id, &signing_key),
            capability: node.capability(&account_id, "cap-send", &device_id),
        })
        .await
        .expect("device registers");
    client
        .create_group(&GroupCreation {
            group_id,
            owner_account_id: account_id.clone(),
            initial_control_head: hex_encode(&[0x11_u8; 32]),
            policy_version: 1,
            mls_epoch: 0,
            members: vec![GroupMemberSpec {
                device_id: device_id.clone(),
                role: OWNER_ROLE.into(),
            }],
        })
        .await
        .expect("group created");
    (account_id, device_id, group_key, bob)
}

/// 完整夹具：节点 + 本地身份/MLS 群 + 已配置节点的内核。
struct Harness {
    node: TestNode,
    dir: FixtureDir,
    device_id: String,
    signing_key: SigningKey,
    group_key: String,
    peer: MlsDevice,
    core: ConversationCore,
}

impl Harness {
    async fn start() -> Self {
        let node = start_node();
        let dir = FixtureDir::new();
        let (_account_id, device_id, group_key, peer) = seed_local_state(&dir, &node).await;
        let mut core =
            ConversationCore::open(&dir.db_text(), &master_key_hex()).expect("open core");
        core.configure_send_node(&node.url(), NODE_ID, Some(&node.public_key_hex()))
            .expect("configure send node");
        Self {
            node,
            dir,
            device_id,
            signing_key: signing_key_from_seed(&DEVICE_SEED),
            group_key,
            peer,
            core,
        }
    }

    /// 节点日志：(SubmissionId, 密文 hex)。
    async fn node_log(&self) -> Vec<(String, String)> {
        let group_id = protocol_group_id_for(&self.group_key).expect("group id");
        let page = self
            .node
            .client(&self.device_id, &self.signing_key)
            .fetch_log(&group_id, 0, 50)
            .await
            .expect("node log");
        page.entries
            .into_iter()
            .map(|entry| {
                (
                    entry.receipt.submission_id,
                    entry.candidate.candidate.encrypted_payload,
                )
            })
            .collect()
    }

    fn local_ciphertext(&mut self, submission_id: &str) -> Vec<u8> {
        // 先释放内核持有的独占写锁，再用同一主密钥只读打开（不重建库）。
        self.core.close();
        let store = open_formal(&self.dir.db());
        store
            .read_attempt(submission_id)
            .expect("read attempt")
            .expect("attempt exists")
            .immutable_ciphertext
    }
}

// ---------------------------------------------------------------- 用例

#[tokio::test]
async fn prepare_and_dispatch_reach_the_node_with_the_durable_ciphertext() {
    let mut harness = Harness::start().await;
    let draft = harness
        .core
        .create_draft(&conversation(), &operation(1), BODY)
        .expect("draft");

    let prepared = harness.core.send_prepare(&draft.draft_id).expect("prepare");
    assert_eq!(prepared.status, "prepared");
    assert!(prepared.settled, "a prepared attempt is a settled result");
    assert_eq!(prepared.attempt_index, Some(0));
    let submission_id = prepared.submission_id.clone().expect("submission id");

    let dispatched = harness
        .core
        .send_dispatch(&draft.draft_id)
        .expect("dispatch");
    assert_eq!(dispatched.status, "accepted");
    assert!(dispatched.settled);
    assert_eq!(dispatched.submission_id.as_deref(), Some(submission_id.as_str()));

    // 节点日志：恰好一条、绑定本次 SubmissionId。
    let log = harness.node_log().await;
    assert_eq!(log.len(), 1, "exactly one accepted entry");
    assert_eq!(log[0].0, submission_id);

    // 密文一致：节点日志密文 == 本地耐久密文。
    let durable = harness.local_ciphertext(&submission_id);
    assert_eq!(hex_encode(&durable), log[0].1);
    // 对端真 MLS 设备能解出原文（证明是真实可解密的 MLS 密文）。
    let plaintext = harness
        .peer
        .decrypt(&hex_to_bytes(&log[0].1), "alice")
        .expect("peer decrypts");
    assert_eq!(plaintext, BODY);

    // 已接受后：retry 不再推进、reconcile 报告一致，都不再重发。
    let mut harness_after = harness;
    let reopened = ConversationCore::open(&harness_after.dir.db_text(), &master_key_hex())
        .expect("reopen core");
    let mut reopened = reopened;
    reopened
        .configure_send_node(&harness_after.node.url(), NODE_ID, Some(&harness_after.node.public_key_hex()))
        .expect("configure");
    let retried = reopened.send_retry(&draft.draft_id).expect("retry");
    assert_eq!(retried.status, "accepted");
    assert!(retried.settled);
    let reconciled = reopened.send_reconcile(&draft.draft_id).expect("reconcile");
    assert_eq!(reconciled.status, "accepted");
    assert!(reconciled.settled);
    let log_after = harness_after.node_log().await;
    assert_eq!(log_after.len(), 1, "retry/reconcile never resend");
    let _ = &mut harness_after;
}

#[tokio::test]
async fn repeated_dispatch_is_idempotent_and_never_resends() {
    let mut harness = Harness::start().await;
    let draft = harness
        .core
        .create_draft(&conversation(), &operation(2), BODY)
        .expect("draft");
    harness.core.send_prepare(&draft.draft_id).expect("prepare");
    let first = harness.core.send_dispatch(&draft.draft_id).expect("dispatch");
    assert_eq!(first.status, "accepted");
    let log = harness.node_log().await;
    assert_eq!(log.len(), 1);

    // 重复 dispatch：幂等重放，节点没有第二条日志（零重发）。
    let second = harness.core.send_dispatch(&draft.draft_id).expect("replay");
    assert_eq!(second.status, "accepted");
    assert_eq!(second.submission_id, first.submission_id);
    let log_after = harness.node_log().await;
    assert_eq!(log_after.len(), 1, "the node never receives a duplicate");
    assert_eq!(log_after[0].1, log[0].1, "the same ciphertext bytes are reused");
}

#[tokio::test]
async fn unconfigured_node_fails_explicitly_and_the_handle_stays_usable() {
    let node = start_node();
    let dir = FixtureDir::new();
    let (_account, _device, _group, _peer) = seed_local_state(&dir, &node).await;
    let mut core = ConversationCore::open(&dir.db_text(), &master_key_hex()).expect("core");
    let draft = core
        .create_draft(&conversation(), &operation(3), BODY)
        .expect("draft");

    let error = core.send_prepare(&draft.draft_id).expect_err("node is not configured");
    assert_eq!(error_code(&error), "NODE_UNAVAILABLE");
    let error = core.send_dispatch(&draft.draft_id).expect_err("node is not configured");
    assert_eq!(error_code(&error), "NODE_UNAVAILABLE");
    // 未配置节点时草稿与库都还是可用的（没有把句柄弄丢）。
    assert!(core.get_draft_status(&draft.draft_id).expect("status").is_some());

    // 配置之后同一条草稿可以真的准备成功。
    core.configure_send_node(&node.url(), NODE_ID, Some(&node.public_key_hex()))
        .expect("configure");
    let prepared = core.send_prepare(&draft.draft_id).expect("prepare");
    assert_eq!(prepared.status, "prepared");
}

#[tokio::test]
async fn unknown_draft_is_rejected_before_any_network_call() {
    let mut harness = Harness::start().await;
    let error = harness
        .core
        .send_dispatch("draft-00000000000000000000000000000000")
        .expect_err("unknown draft");
    assert_eq!(error_code(&error), "INVALID_TARGET");
    assert!(harness.node_log().await.is_empty(), "nothing was submitted");
    // 句柄仍可用。
    assert!(harness.core.create_draft(&conversation(), &operation(4), BODY).is_ok());
}

#[tokio::test]
async fn missing_identity_or_mls_group_fails_explicitly() {
    let node = start_node();

    // 1) 没有入库身份：显式失败，且句柄可继续使用。
    let dir = FixtureDir::new();
    let mut core = ConversationCore::open(&dir.db_text(), &master_key_hex()).expect("core");
    core.configure_send_node(&node.url(), NODE_ID, Some(&node.public_key_hex()))
        .expect("configure");
    let draft = core
        .create_draft(&conversation(), &operation(5), BODY)
        .expect("draft");
    let error = core.send_prepare(&draft.draft_id).expect_err("no identity");
    assert_eq!(error_code(&error), "INVALID_STATE_TRANSITION");
    assert!(core.get_draft_status(&draft.draft_id).expect("status").is_some());
    assert!(
        core.create_draft(&conversation(), &operation(6), "second").is_ok(),
        "the store handle was returned to the core"
    );

    // 2) 有身份但没有 MLS 群状态：同样显式失败（不会新建空群）。
    let dir = FixtureDir::new();
    {
        let mut core =
            ConversationCore::open(&dir.db_text(), &master_key_hex()).expect("open core");
        core.save_identity_account(IdentityAccountInput {
            display_name: "Bob".into(),
            account_seed_hex: hex_encode(&ACCOUNT_SEED),
            device_seed_hex: hex_encode(&DEVICE_SEED),
            created_at_ms: 1_700_000_000_000,
        })
        .expect("identity saved");
        core.close();
    }
    let mut core = ConversationCore::open(&dir.db_text(), &master_key_hex()).expect("core");
    core.configure_send_node(&node.url(), NODE_ID, Some(&node.public_key_hex()))
        .expect("configure");
    let draft = core
        .create_draft(&conversation(), &operation(7), BODY)
        .expect("draft");
    let error = core.send_prepare(&draft.draft_id).expect_err("no MLS group");
    assert_eq!(error_code(&error), "INVALID_STATE_TRANSITION");
    assert!(core.get_draft_status(&draft.draft_id).expect("status").is_some());

    // 3) 显式指定的群键不存在：仍然是显式失败。
    core.configure_send_group("0011223344556677")
        .expect("group key accepted as opaque text");
    let error = core.send_prepare(&draft.draft_id).expect_err("unknown group");
    assert_eq!(error_code(&error), "INVALID_STATE_TRANSITION");
}
