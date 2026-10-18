//! 附件上传/下载接真节点的 FFI 端到端用例（ATT-U）。
//!
//! 覆盖：导入 → 上传（状态真实迁移到 available）→ 从节点下载并解出原文 → 重复上传幂等
//! 零发送 → 错误凭据 / 未注册设备 / 未知群显式失败 → 另一实例无法解密（诚实失败，不伪造）。

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::service::{ConversationCore, IdentityAccountInput};
use chat_ffi::attachments_ffi::AttachmentFfi;
use chat_ffi::local_store::FfiError;
use chat_node_api::build_router;
use chat_node_api::state::{AppState, FixedClock as NodeFixedClock};
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_node_services::canonical::hex_encode;
use chat_node_services::group::{GroupCandidate, GroupCreation, GroupMemberSpec, KIND_APPLICATION};
use chat_node_services::registration::{
    DeviceCredential, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest, RegistrationCapability,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use chat_protocol::signing::signing_key_from_seed;
use chat_protocol::{GroupId, SubmissionId};
use ed25519_dalek::{Signature, Signer, SigningKey};

const NODE_ID: &str = "node-ffi-attachment-test";
const OWNER_ROLE: &str = "owner";
const ACCOUNT_SEED: [u8; 32] = [0x31; 32];
const DEVICE_SEED: [u8; 32] = [0x21; 32];
const MASTER_KEY: [u8; 32] = [0x5a; 32];
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
    fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn public_key_hex(&self) -> String {
        hex_encode(&self.node_public_key)
    }

    fn endpoint(&self) -> NodeEndpoint {
        NodeEndpoint::new(&self.url(), NODE_ID)
            .expect("endpoint")
            .with_node_public_key(self.node_public_key)
    }

    fn client(&self, device_id: &str, key: &SigningKey) -> NodeClient {
        NodeClient::new(self.endpoint(), device_id, key.clone()).expect("node client")
    }

    fn credential(&self, account_id: &str, device_id: &str, key: &SigningKey) -> DeviceCredential {
        let mut credential = DeviceCredential {
            account_id: account_id.to_owned(),
            device_id: device_id.to_owned(),
            device_public_key: hex_encode(&key.verifying_key().to_bytes()),
            endpoint_id: "endpoint-ffi".into(),
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

    fn capability(
        &self,
        account_id: &str,
        capability_id: &str,
        device_id: &str,
    ) -> RegistrationCapability {
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

    fn stop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for TestNode {
    fn drop(&mut self) {
        self.stop();
    }
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
            "chat-ffi-attachment-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> String {
        self.0.join("chat.db").to_string_lossy().into_owned()
    }

    fn cache(&self) -> String {
        let path = self.0.join("attachments");
        fs::create_dir_all(&path).expect("create cache dir");
        path.to_string_lossy().into_owned()
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn write(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.path(name);
        fs::write(&path, bytes).expect("write fixture file");
        path.to_string_lossy().into_owned()
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

fn code(error: FfiError) -> String {
    match error {
        FfiError::Store { code, .. } => code,
    }
}

/// 在本地库里写入持久身份，返回（账户 ID，设备 ID）。
fn seed_identity(dir: &FixtureDir) -> (String, String) {
    let mut core = ConversationCore::open(&dir.database(), &master_key_hex()).expect("open core");
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
}

/// 在节点上注册设备、建群并追加一条本设备条目；返回（群 ID，source_seq）。
fn seed_node(node: &TestNode, account_id: &str, device_id: &str) -> (String, u64) {
    let signing_key = signing_key_from_seed(&DEVICE_SEED);
    let client = node.client(device_id, &signing_key);
    let group_id = GroupId::from_bytes([0x41; 32]).encode();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let group_for_task = group_id.clone();
    let group_for_return = group_id.clone();
    let device_for_task = device_id.to_owned();
    let source_seq = runtime.block_on(async move {
        client
            .register_device(&RegisterDeviceRequest {
                account_id: account_id.to_owned(),
                credential: node.credential(account_id, device_id, &signing_key),
                capability: node.capability(account_id, "cap-att", device_id),
            })
            .await
            .expect("device registers");
        client
            .create_group(&GroupCreation {
                group_id: group_for_task.clone(),
                owner_account_id: account_id.to_owned(),
                initial_control_head: hex_encode(&[0x11_u8; 32]),
                policy_version: 1,
                mls_epoch: 0,
                members: vec![GroupMemberSpec {
                    device_id: device_for_task,
                    role: OWNER_ROLE.into(),
                }],
            })
            .await
            .expect("group created");
        let head = client.get_head(&group_for_task).await.expect("group head");
        let candidate = GroupCandidate {
            protocol_major: 1,
            protocol_minor: 0,
            submission_id: SubmissionId::from_bytes([0x51; 32]).encode(),
            group_id: group_for_task.clone(),
            author_device_id: device_id.to_owned(),
            kind: KIND_APPLICATION.to_owned(),
            expected_control_head: head.control_head.clone(),
            policy_version: head.policy_version,
            mls_epoch: head.mls_epoch,
            encrypted_payload: hex_encode(b"attachment-source-entry"),
            control_metadata: None,
        };
        client.append_entry(&candidate).await.expect("source entry").seq
    });
    (group_for_return, source_seq)
}

fn small_source() -> Vec<u8> {
    let mut bytes = b"ATTACHMENT-FFI-PLAINTEXT-MARKER".to_vec();
    bytes.extend((0..1024u32).map(|index| (index as u8).wrapping_mul(7).wrapping_add(3)));
    bytes
}

// ---------------------------------------------------------------- 用例

#[test]
fn upload_reaches_the_node_and_download_returns_the_original_bytes() {
    let node = start_node();
    let dir = FixtureDir::new();
    let (account_id, device_id) = seed_identity(&dir);
    let (group_id, source_seq) = seed_node(&node, &account_id, &device_id);
    let source = small_source();
    let source_path = dir.write("photo.bin", &source);

    let ffi = AttachmentFfi::open(dir.database(), master_key_hex(), dir.cache()).expect("open");
    let imported = ffi
        .import_file(source_path, "photo.bin".to_owned())
        .expect("import");
    assert_eq!(imported.state, "imported");
    assert_eq!(imported.chunk_count, 1);
    assert!(imported.available_chunks >= 1);

    let uploaded = ffi
        .upload(
            imported.attachment_id.clone(),
            node.url(),
            NODE_ID.to_owned(),
            Some(node.public_key_hex()),
            group_id.clone(),
            source_seq,
        )
        .expect("upload");
    assert_eq!(uploaded.state, "available", "state really moved to available");
    assert_eq!(uploaded.available_chunks, uploaded.chunk_count);
    assert_eq!(uploaded.content_digest, imported.content_digest);

    // 下载：从节点取回密文块并认证解密回原文。
    let target = dir.path("downloaded.bin");
    let receipt = ffi
        .download(
            uploaded.attachment_id.clone(),
            target.to_string_lossy().into_owned(),
            node.url(),
            NODE_ID.to_owned(),
            Some(node.public_key_hex()),
            group_id.clone(),
            source_seq,
        )
        .expect("download");
    assert_eq!(receipt.written_bytes, source.len() as u64);
    assert_eq!(receipt.content_digest, imported.content_digest);
    assert_eq!(fs::read(&target).expect("read target"), source);

    // 持久状态：重新打开对象后仍是 available（状态真实落库）。
    drop(ffi);
    let reopened = AttachmentFfi::open(dir.database(), master_key_hex(), dir.cache()).expect("reopen");
    let status = reopened
        .status(uploaded.attachment_id.clone())
        .expect("status after reopen");
    assert_eq!(status.state, "available");

    // 诚实边界：FileKey 只在进程内，另一个实例无法解密（显式失败，不产出文件）。
    let second_target = dir.path("second-instance.bin");
    let error = reopened
        .download(
            uploaded.attachment_id.clone(),
            second_target.to_string_lossy().into_owned(),
            node.url(),
            NODE_ID.to_owned(),
            Some(node.public_key_hex()),
            group_id,
            source_seq,
        )
        .expect_err("a fresh instance has no file key");
    assert_eq!(code(error), "ATTACHMENT_KEY_UNAVAILABLE");
    assert!(!second_target.exists(), "no unverified output");
}

#[test]
fn repeated_upload_is_idempotent_and_sends_nothing() {
    let node = start_node();
    let dir = FixtureDir::new();
    let (account_id, device_id) = seed_identity(&dir);
    let (group_id, source_seq) = seed_node(&node, &account_id, &device_id);
    let source_path = dir.write("again.bin", &small_source());

    let ffi = AttachmentFfi::open(dir.database(), master_key_hex(), dir.cache()).expect("open");
    let imported = ffi
        .import_file(source_path, "again.bin".to_owned())
        .expect("import");
    let first = ffi
        .upload(
            imported.attachment_id.clone(),
            node.url(),
            NODE_ID.to_owned(),
            Some(node.public_key_hex()),
            group_id.clone(),
            source_seq,
        )
        .expect("upload");
    assert_eq!(first.state, "available");

    // 关掉节点：如果重复上传真的发请求，就必然失败；返回 available 说明零发送。
    let mut node = node;
    node.stop();
    let repeated = ffi
        .upload(
            imported.attachment_id.clone(),
            node.url(),
            NODE_ID.to_owned(),
            Some(node.public_key_hex()),
            group_id,
            source_seq,
        )
        .expect("idempotent upload after the node is gone");
    assert_eq!(repeated.state, "available");
    assert_eq!(repeated.transferred_chunks, first.transferred_chunks);
}

#[test]
fn wrong_credentials_and_unknown_groups_fail_explicitly() {
    let node = start_node();
    let dir = FixtureDir::new();
    let (account_id, device_id) = seed_identity(&dir);
    let (group_id, source_seq) = seed_node(&node, &account_id, &device_id);
    let source_path = dir.write("denied.bin", &small_source());
    let ffi = AttachmentFfi::open(dir.database(), master_key_hex(), dir.cache()).expect("open");
    let imported = ffi
        .import_file(source_path, "denied.bin".to_owned())
        .expect("import");

    // 1) 未知群（节点上没有）：显式失败，状态不会被标成 available。
    let error = ffi
        .upload(
            imported.attachment_id.clone(),
            node.url(),
            NODE_ID.to_owned(),
            Some(node.public_key_hex()),
            GroupId::from_bytes([0x77; 32]).encode(),
            source_seq,
        )
        .expect_err("unknown group must be rejected");
    assert!(
        matches!(
            code(error).as_str(),
            "GROUP_NOT_FOUND" | "INVALID_TARGET" | "PERMISSION_DENIED"
        ),
        "unexpected code for an unknown group"
    );
    let status = ffi.status(imported.attachment_id.clone()).expect("status");
    assert_ne!(status.state, "available", "never fake availability");

    // 2) 已被别人占用的 source_seq（作者不是本设备）：显式失败。
    let error = ffi
        .upload(
            imported.attachment_id.clone(),
            node.url(),
            NODE_ID.to_owned(),
            Some(node.public_key_hex()),
            group_id.clone(),
            999,
        )
        .expect_err("a foreign source seq must be rejected");
    assert!(
        matches!(
            code(error).as_str(),
            "PERMISSION_DENIED" | "INVALID_TARGET" | "GROUP_NOT_FOUND"
        ),
        "unexpected code for a foreign source seq"
    );

    // 3) 端点非法 / 公钥非法：本地显式拒绝（不发请求）。
    let error = ffi
        .upload(
            imported.attachment_id.clone(),
            "not-a-url".to_owned(),
            NODE_ID.to_owned(),
            None,
            group_id.clone(),
            source_seq,
        )
        .expect_err("bad endpoint");
    assert_eq!(code(error), "INVALID_TARGET");
    let error = ffi
        .upload(
            imported.attachment_id.clone(),
            node.url(),
            NODE_ID.to_owned(),
            Some("zz".to_owned()),
            group_id,
            source_seq,
        )
        .expect_err("bad node public key");
    assert_eq!(code(error), "INVALID_SIGNATURE");

    // 4) 库里设备未在节点注册（换一个未注册的签名钥由持久身份决定，故这里用未建群节点验证）：
    //    另起一个空节点，注册与建群都不做 → 上传显式失败。
    let empty = start_node();
    let error = ffi
        .upload(
            imported.attachment_id.clone(),
            empty.url(),
            NODE_ID.to_owned(),
            Some(empty.public_key_hex()),
            GroupId::from_bytes([0x41; 32]).encode(),
            source_seq,
        )
        .expect_err("unregistered device must be rejected");
    assert!(
        !code(error).is_empty(),
        "an explicit stable code is required"
    );
    let status = ffi.status(imported.attachment_id).expect("status");
    assert_ne!(status.state, "available");
}

#[test]
fn in_memory_object_refuses_upload_and_download() {
    let node = start_node();
    let dir = FixtureDir::new();
    let source_path = dir.write("memory.bin", &small_source());
    let ffi = AttachmentFfi::new(dir.cache()).expect("in-memory attachment object");
    let imported = ffi
        .import_file(source_path, "memory.bin".to_owned())
        .expect("import");
    let group_id = GroupId::from_bytes([0x41; 32]).encode();

    let error = ffi
        .upload(
            imported.attachment_id.clone(),
            node.url(),
            NODE_ID.to_owned(),
            Some(node.public_key_hex()),
            group_id.clone(),
            1,
        )
        .expect_err("the in-memory object has no persisted records");
    assert_eq!(code(error), "INVALID_STATE_TRANSITION");

    let error = ffi
        .download(
            imported.attachment_id,
            dir.path("out.bin").to_string_lossy().into_owned(),
            node.url(),
            NODE_ID.to_owned(),
            Some(node.public_key_hex()),
            group_id,
            1,
        )
        .expect_err("the in-memory object has no persisted records");
    assert_eq!(code(error), "INVALID_STATE_TRANSITION");
}

#[test]
fn download_without_a_record_is_rejected() {
    let node = start_node();
    let dir = FixtureDir::new();
    let (account_id, device_id) = seed_identity(&dir);
    let (group_id, source_seq) = seed_node(&node, &account_id, &device_id);
    let ffi = AttachmentFfi::open(dir.database(), master_key_hex(), dir.cache()).expect("open");

    let error = ffi
        .download(
            "att-00000000000000000000000000000000".to_owned(),
            dir.path("nope.bin").to_string_lossy().into_owned(),
            node.url(),
            NODE_ID.to_owned(),
            Some(node.public_key_hex()),
            group_id,
            source_seq,
        )
        .expect_err("unknown attachment");
    assert_eq!(code(error), "ATTACHMENT_NOT_FOUND");
    assert!(!dir.path("nope.bin").exists());
    // 未使用的错误路径也不能留下“静默可用”的对象。
    assert!(ffi.status("att-00000000000000000000000000000000".to_owned()).is_err());
}
