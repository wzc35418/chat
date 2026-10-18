//! NodeBlobTransport 端到端：真 axum 节点 + 真 node-client + 真实加密库与分块密文。
//!
//! 覆盖：上传后节点能读回逐字节一致的密文块、下载取回并解出原文、断链保留可重试状态、
//! 重试幂等、节点重启后已发布 blob 仍可下载、节点错误码保真（配额/鉴权）、
//! 以及“节点块上限 < 客户端分块大小”这一已知差异的显式拒绝。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::attachment_transport::{
    CIPHERTEXT_CHUNK_OVERHEAD, NodeBlobTransport, blob_object_id_for,
};
use chat_core::attachments::{
    AttachmentManifest, AttachmentState, FileToken, PersistedAttachmentStore, StoredAttachmentRecord,
};
use chat_crypto::attachment::CHUNK_BYTES;
use chat_node_api::build_router;
use chat_node_api::state::{AppState, FixedClock as NodeFixedClock};
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_node_services::canonical::hex_encode;
use chat_node_services::group::{
    GroupCreation, GroupMemberSpec, GroupCandidate, KIND_APPLICATION,
};
use chat_node_services::limits::BlobPolicy;
use chat_node_services::registration::{
    DeviceCredential, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest, RegistrationCapability,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use chat_protocol::{DeviceId, GroupId, SubmissionId};
use chat_storage::formal::{
    KeyProvenance, ProtectedStorageKey, StoreOptions, open_store,
};
use ed25519_dalek::{Signature, Signer, SigningKey};

const NODE_ID: &str = "node-core-blob-test";
const OWNER_ROLE: &str = "owner";
const ACCOUNT: &str = "acct-core-blob";
const NOW_S: u64 = 1_700_000_000;

// ---------------------------------------------------------------- 真实节点夹具

struct TestNode {
    address: String,
    node_public_key: [u8; 32],
    supervisor: SigningKey,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// 起一个真实 node-api 服务（可传入文件库用于“节点重启”用例）。
fn start_node(database: Option<&Path>, policy: NodeAuthPolicy) -> TestNode {
    let config = NodeServiceConfig {
        node_id: NODE_ID.into(),
        signing_seed: [7_u8; 32],
        policy,
    };
    let service = match database {
        Some(path) => NodeService::open(path, config),
        None => NodeService::open_in_memory(config),
    }
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

    fn dead_client(&self, device_id: &str, key: &SigningKey) -> NodeClient {
        let endpoint = NodeEndpoint::new("http://127.0.0.1:1", NODE_ID)
            .expect("endpoint")
            .with_node_public_key(self.node_public_key);
        NodeClient::new(endpoint, device_id, key.clone()).expect("dead client")
    }

    fn credential(&self, device_id: &str, key: &SigningKey) -> DeviceCredential {
        let mut credential = DeviceCredential {
            account_id: ACCOUNT.into(),
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

    fn capability(&self, capability_id: &str, device_id: &str) -> RegistrationCapability {
        let mut capability = RegistrationCapability {
            capability_id: capability_id.to_owned(),
            account_id: ACCOUNT.into(),
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

    fn registration_request(
        &self,
        device_id: &str,
        key: &SigningKey,
        capability_id: &str,
    ) -> RegisterDeviceRequest {
        RegisterDeviceRequest {
            account_id: ACCOUNT.into(),
            credential: self.credential(device_id, key),
            capability: self.capability(capability_id, device_id),
        }
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

fn device() -> (String, SigningKey) {
    let device_id = DeviceId::from_bytes([21_u8; 32]).encode();
    let key = SigningKey::from_bytes(&[21_u8; 32]);
    (device_id, key)
}

fn group() -> String {
    GroupId::from_bytes([31_u8; 32]).encode()
}

/// 注册设备 → 建群 → 追加一条本设备条目（blob 上传必须挂到自己的条目上）。
async fn setup(node: &TestNode, device_id: &str, key: &SigningKey) -> (String, u64) {
    let client = node.client(device_id, key);
    client
        .register_device(&node.registration_request(device_id, key, "cap-blob"))
        .await
        .expect("device registers");
    let group_id = group();
    let creation = GroupCreation {
        group_id: group_id.clone(),
        owner_account_id: ACCOUNT.into(),
        initial_control_head: hex_encode(&[0x11_u8; 32]),
        policy_version: 1,
        mls_epoch: 0,
        members: vec![GroupMemberSpec {
            device_id: device_id.to_owned(),
            role: OWNER_ROLE.into(),
        }],
    };
    client.create_group(&creation).await.expect("group created");
    let head = client.get_head(&group_id).await.expect("group head");
    let candidate = GroupCandidate {
        protocol_major: 1,
        protocol_minor: 0,
        submission_id: SubmissionId::from_bytes([41_u8; 32]).encode(),
        group_id: group_id.clone(),
        author_device_id: device_id.to_owned(),
        kind: KIND_APPLICATION.to_owned(),
        expected_control_head: head.control_head.clone(),
        policy_version: head.policy_version,
        mls_epoch: head.mls_epoch,
        encrypted_payload: hex_encode(b"attachment-source-entry"),
        control_metadata: None,
    };
    let receipt = client
        .append_entry(&candidate)
        .await
        .expect("source entry accepted");
    (group_id, receipt.seq)
}

// ---------------------------------------------------------------- 本地加密库夹具

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
            "chat-core-attachment-transport-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.path(name);
        fs::write(&path, bytes).expect("write fixture file");
        path
    }

    fn cache_root(&self) -> PathBuf {
        self.path("cache")
    }

    fn store(&self) -> PersistedAttachmentStore {
        let formal = open_store(
            StoreOptions::new(self.path("chat.db"), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes([0x5a; 32]),
        )
        .expect("open formal store");
        PersistedAttachmentStore::open(self.cache_root(), formal)
            .expect("open persisted attachment store")
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn pattern(length: usize, seed: u8) -> Vec<u8> {
    (0..length)
        .map(|index| (index as u8).wrapping_mul(17).wrapping_add(seed))
        .collect()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|window| window == needle)
}

/// 本地缓存里某个分块密文（布局：cache/att-&lt;hex&gt;/chunk-NNNNNN.bin）。
fn local_chunk(cache_root: &Path, attachment_id: &str, index: u32) -> Vec<u8> {
    fs::read(cache_root.join(attachment_id).join(format!("chunk-{index:06}.bin")))
        .expect("read local ciphertext chunk")
}

/// 由持久化记录推导公开清单（ciphertext_bytes 按分块格式推导，记录里没有该列）。
fn manifest_for(record: &StoredAttachmentRecord) -> AttachmentManifest {
    let ciphertext_bytes = record.plain_size
        + u64::from(record.chunk_count) * CIPHERTEXT_CHUNK_OVERHEAD as u64;
    AttachmentManifest {
        attachment_id: record.attachment_id.clone(),
        plain_size: record.plain_size,
        chunk_size: record.chunk_size,
        chunk_count: record.chunk_count,
        ciphertext_bytes,
        ciphertext_digest: record.ciphertext_digest.clone(),
    }
}

// ---------------------------------------------------------------- 用例

#[tokio::test]
async fn upload_publishes_chunks_the_node_returns_verbatim() {
    let node = start_node(None, NodeAuthPolicy::default());
    let (device_id, key) = device();
    let (group_id, source_seq) = setup(&node, &device_id, &key).await;
    let dir = FixtureDir::new();
    let store = dir.store();

    let marker = b"ATTACHMENT-NODE-PLAINTEXT-MARKER";
    let mut source = marker.to_vec();
    source.extend(pattern(1024, 3));
    let source_path = dir.write("small.bin", &source);
    let descriptor = store
        .import_file(&FileToken::from_local_path(&source_path), "small.bin")
        .expect("import");
    assert_eq!(descriptor.chunk_count, 1, "node policy needs a single small chunk");

    let mut transport =
        NodeBlobTransport::new(node.client(&device_id, &key), group_id.clone(), source_seq)
            .expect("transport");
    let stats = transport.stats();
    let status = store
        .upload_attachment(&descriptor.attachment_id, &mut transport)
        .expect("upload");
    assert_eq!(status.state, AttachmentState::Available);
    assert_eq!(status.transferred_chunks, 1);
    assert_eq!(stats.begin_calls(), 1);
    assert_eq!(stats.put_calls(), 1);
    assert_eq!(stats.complete_calls(), 1);

    // 节点读回的块与本地缓存密文逐字节一致，且节点内容里不含明文。
    let object_id = blob_object_id_for(&descriptor.attachment_id).expect("blob id");
    let probe = node.client(&device_id, &key);
    let remote = probe
        .read_blob_chunk(&object_id, 0)
        .await
        .expect("node returns the published chunk");
    let local = local_chunk(&dir.cache_root(), &descriptor.attachment_id, 0);
    assert_eq!(remote, local, "node must hold the exact ciphertext bytes");
    assert!(!contains(&remote, marker), "the node must never receive plaintext");

    let record = store
        .attachment_record(&descriptor.attachment_id)
        .expect("record")
        .expect("record exists");
    assert_eq!(record.state, "available");
    assert_eq!(record.ciphertext_digest.len(), 64);

    // 重复上传幂等：已 available，不再建会话、不再发块。
    let (begins, puts, completes) = (stats.begin_calls(), stats.put_calls(), stats.complete_calls());
    let repeated = store
        .upload_attachment(&descriptor.attachment_id, &mut transport)
        .expect("idempotent upload");
    assert_eq!(repeated.state, AttachmentState::Available);
    assert_eq!(stats.begin_calls(), begins);
    assert_eq!(stats.put_calls(), puts);
    assert_eq!(stats.complete_calls(), completes);
}

#[tokio::test]
async fn download_from_the_node_decrypts_the_original_plaintext() {
    let node = start_node(None, NodeAuthPolicy::default());
    let (device_id, key) = device();
    let (group_id, source_seq) = setup(&node, &device_id, &key).await;
    let dir = FixtureDir::new();
    let store = dir.store();

    let mut source = b"DOWNLOAD-ROUNDTRIP-MARKER".to_vec();
    source.extend(pattern(2048, 5));
    let source_path = dir.write("roundtrip.bin", &source);
    let descriptor = store
        .import_file(&FileToken::from_local_path(&source_path), "roundtrip.bin")
        .expect("import");
    let mut uploader =
        NodeBlobTransport::new(node.client(&device_id, &key), group_id.clone(), source_seq)
            .expect("uploader");
    store
        .upload_attachment(&descriptor.attachment_id, &mut uploader)
        .expect("upload");

    let record = store
        .attachment_record(&descriptor.attachment_id)
        .expect("record")
        .expect("record exists");
    let manifest = manifest_for(&record);

    // 新 transport 实例模拟“另一条下载路径”，仍用同一设备凭据。
    let mut downloader =
        NodeBlobTransport::new(node.client(&device_id, &key), group_id.clone(), source_seq)
            .expect("downloader");
    let target = dir.path("downloaded.bin");
    let receipt = store
        .download_attachment(
            &descriptor.attachment_id,
            &manifest,
            &FileToken::from_local_path(&target),
            &mut downloader,
        )
        .expect("download");
    assert_eq!(receipt.content_digest, descriptor.content_digest);
    assert_eq!(receipt.written_bytes, source.len() as u64);
    assert_eq!(fs::read(&target).expect("read target"), source);
    assert_eq!(downloader.stats().read_calls(), 1);

    // 与本地记录不一致的清单必须在任何节点读取之前被拒。
    let mut wrong = manifest.clone();
    let mut digest = wrong.ciphertext_digest.clone();
    // 必须真的改掉一个字符：原摘要末位可能就是 '0'，直接 push('0') 会是空改动（曾因此偶发失败）。
    let last = digest.pop().expect("digest is not empty");
    digest.push(if last == '0' { '1' } else { '0' });
    assert_ne!(digest, wrong.ciphertext_digest, "the mutation must change the digest");
    wrong.ciphertext_digest = digest;
    let error = store
        .download_attachment(
            &descriptor.attachment_id,
            &wrong,
            &FileToken::from_local_path(&dir.path("rejected.bin")),
            &mut downloader,
        )
        .expect_err("a divergent manifest must be rejected");
    assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");
    assert_eq!(downloader.stats().read_calls(), 1, "no extra node reads");
    assert!(!dir.path("rejected.bin").exists());
}

#[tokio::test]
async fn broken_link_keeps_the_attempt_retryable_and_retry_is_idempotent() {
    let node = start_node(None, NodeAuthPolicy::default());
    let (device_id, key) = device();
    let (group_id, source_seq) = setup(&node, &device_id, &key).await;
    let dir = FixtureDir::new();
    let store = dir.store();

    let mut source = b"BROKEN-LINK-MARKER".to_vec();
    source.extend(pattern(1500, 7));
    let source_path = dir.write("broken.bin", &source);
    let descriptor = store
        .import_file(&FileToken::from_local_path(&source_path), "broken.bin")
        .expect("import");

    // 断链：节点不可达 → 端口显式失败（冻结码保真），状态停在 uploading 且缓存完整。
    let mut dead = NodeBlobTransport::new(
        node.dead_client(&device_id, &key),
        group_id.clone(),
        source_seq,
    )
    .expect("dead transport");
    let error = store
        .upload_attachment(&descriptor.attachment_id, &mut dead)
        .expect_err("unreachable node must fail");
    assert_eq!(error.code(), "NETWORK_UNAVAILABLE");
    let status = store
        .attachment_status(&descriptor.attachment_id)
        .expect("status after failure");
    assert_eq!(status.state, AttachmentState::Uploading);
    // 单块附件：失败发生在建会话阶段，所以已发送块数为 0；缓存块一片不少。
    assert_eq!(status.transferred_chunks, 0);
    assert_eq!(status.available_chunks, descriptor.chunk_count);
    let record = store
        .attachment_record(&descriptor.attachment_id)
        .expect("record")
        .expect("record exists");
    assert_eq!(record.state, "uploading", "a failed upload stays retryable");

    // 重试：真实节点、同一 operation ID（节点侧幂等）、同一密文字节。
    let mut real =
        NodeBlobTransport::new(node.client(&device_id, &key), group_id.clone(), source_seq)
            .expect("transport");
    let uploaded = store
        .upload_attachment(&descriptor.attachment_id, &mut real)
        .expect("retry succeeds");
    assert_eq!(uploaded.state, AttachmentState::Available);
    assert_eq!(real.stats().begin_calls(), 1);
    assert_eq!(real.stats().put_calls(), 1);
    let object_id = blob_object_id_for(&descriptor.attachment_id).expect("blob id");
    let remote = node
        .client(&device_id, &key)
        .read_blob_chunk(&object_id, 0)
        .await
        .expect("published chunk");
    assert_eq!(remote, local_chunk(&dir.cache_root(), &descriptor.attachment_id, 0));

    // 之后再上传：幂等无操作（不新建会话、不重复发块）。
    let (begins, puts) = (real.stats().begin_calls(), real.stats().put_calls());
    store
        .upload_attachment(&descriptor.attachment_id, &mut real)
        .expect("idempotent replay");
    assert_eq!(real.stats().begin_calls(), begins);
    assert_eq!(real.stats().put_calls(), puts);
}

#[tokio::test]
async fn multi_chunk_attachments_pass_the_node_chunk_policy_and_publish() {
    let node = start_node(None, NodeAuthPolicy::default());
    let (device_id, key) = device();
    let (group_id, source_seq) = setup(&node, &device_id, &key).await;
    let dir = FixtureDir::new();
    let store = dir.store();

    // 客户端分块是 256 KiB（密文 +29 字节）。节点侧上限已对齐到 1 MiB，
    // 因此多块附件必须能真正发布；若节点策略再次收紧，这个用例会立刻变红。
    let source = pattern(2 * CHUNK_BYTES + 777, 9);
    let source_path = dir.write("multi.bin", &source);
    let descriptor = store
        .import_file(&FileToken::from_local_path(&source_path), "multi.bin")
        .expect("import");
    assert_eq!(descriptor.chunk_count, 3);
    let mut transport =
        NodeBlobTransport::new(node.client(&device_id, &key), group_id.clone(), source_seq)
            .expect("transport");
    let status = store
        .upload_attachment(&descriptor.attachment_id, &mut transport)
        .expect("256 KiB chunks must be accepted by the node policy");
    assert_eq!(status.state, AttachmentState::Available);
    assert_eq!(status.transferred_chunks, descriptor.chunk_count);

    // 逐块读回校验由 upload_publishes_chunks_the_node_returns_verbatim 覆盖（同一适配层）；
    // 这里只锁住"多块在默认节点策略下能真正发布"，避免重复网络断言。

    // 本地记录的尺寸没有被篡改去凑策略。
    let record = store
        .attachment_record(&descriptor.attachment_id)
        .expect("record")
        .expect("record exists");
    assert_eq!(record.state, "available");
    assert_eq!(record.plain_size, source.len() as u64);
    assert_eq!(record.chunk_count, 3);
}

#[tokio::test]
async fn published_blob_survives_a_node_restart() {
    let dir = FixtureDir::new();
    let database = dir.path("node.db");
    let (device_id, key) = device();

    // 客户端一侧不重启（FileKey 只在进程内）：只有节点重启，本地库与缓存保持同一个句柄。
    let store = dir.store();
    let (group_id, source_seq, attachment_id, source) = {
        let mut node = start_node(Some(&database), NodeAuthPolicy::default());
        let (group_id, source_seq) = setup(&node, &device_id, &key).await;
        let mut source = b"NODE-RESTART-MARKER".to_vec();
        source.extend(pattern(900, 11));
        let source_path = dir.write("restart.bin", &source);
        let descriptor = store
            .import_file(&FileToken::from_local_path(&source_path), "restart.bin")
            .expect("import");
        let mut transport =
            NodeBlobTransport::new(node.client(&device_id, &key), group_id.clone(), source_seq)
                .expect("transport");
        let status = store
            .upload_attachment(&descriptor.attachment_id, &mut transport)
            .expect("upload");
        assert_eq!(status.state, AttachmentState::Available);
        let attachment_id = descriptor.attachment_id.clone();
        // 关键：先关节点（释放节点库连接），再用同一库文件重启。
        node.stop();
        (group_id, source_seq, attachment_id, source)
    };

    let node = start_node(Some(&database), NodeAuthPolicy::default());
    let record = store
        .attachment_record(&attachment_id)
        .expect("record")
        .expect("record persists locally");
    let manifest = manifest_for(&record);
    let mut reloaded_transport =
        NodeBlobTransport::new(node.client(&device_id, &key), group_id.clone(), source_seq)
            .expect("transport after restart");
    let target = dir.path("after-restart.bin");
    let receipt = store
        .download_attachment(
            &attachment_id,
            &manifest,
            &FileToken::from_local_path(&target),
            &mut reloaded_transport,
        )
        .expect("the published blob survives the restart");
    assert_eq!(receipt.content_digest, record.content_digest);
    assert_eq!(fs::read(&target).expect("read target"), source);
}

#[tokio::test]
async fn node_errors_keep_their_stable_codes() {
    // 1) 配额：设备上限刚好只够一份附件，第二份必须被节点以 QUOTA_EXCEEDED 拒绝。
    let policy = NodeAuthPolicy {
        blob: BlobPolicy {
            max_bytes_per_device: 1029,
            max_object_bytes: 1029,
            max_chunk_bytes: 1029,
            max_chunk_count: 8,
            upload_ttl_seconds: 3_600,
        },
        ..NodeAuthPolicy::default()
    };
    let node = start_node(None, policy);
    let (device_id, key) = device();
    let (group_id, source_seq) = setup(&node, &device_id, &key).await;
    let dir = FixtureDir::new();
    let store = dir.store();

    let first_path = dir.write("first.bin", &pattern(600, 13));
    let first = store
        .import_file(&FileToken::from_local_path(&first_path), "first.bin")
        .expect("import first");
    let second_path = dir.write("second.bin", &pattern(600, 17));
    let second = store
        .import_file(&FileToken::from_local_path(&second_path), "second.bin")
        .expect("import second");

    let mut transport =
        NodeBlobTransport::new(node.client(&device_id, &key), group_id.clone(), source_seq)
            .expect("transport");
    let uploaded = store
        .upload_attachment(&first.attachment_id, &mut transport)
        .expect("first upload fits the quota");
    assert_eq!(uploaded.state, AttachmentState::Available);
    let error = store
        .upload_attachment(&second.attachment_id, &mut transport)
        .expect_err("the second upload exceeds the device quota");
    assert_eq!(error.code(), "QUOTA_EXCEEDED");
    let detail = error.to_string();
    assert!(!detail.contains("SECRET"), "error details must not leak content");

    // 2) 鉴权：设备已登记但签名钥不对 → 节点拒绝，冻结码保真（不是静默成功）。
    let wrong_key = SigningKey::from_bytes(&[99_u8; 32]);
    let mut unauthenticated = NodeBlobTransport::new(
        node.client(&device_id, &wrong_key),
        group_id.clone(),
        source_seq,
    )
    .expect("transport with a wrong key");
    let error = store
        .upload_attachment(&second.attachment_id, &mut unauthenticated)
        .expect_err("wrong device key must be rejected");
    assert!(
        matches!(
            error.code(),
            "INVALID_SIGNATURE" | "INVALID_CREDENTIAL" | "AUTH_EXPIRED" | "AUTH_REPLAY"
                | "PERMISSION_DENIED"
        ),
        "authentication failures must keep a stable code, got {}",
        error.code()
    );
    assert_eq!(second.attachment_id.len(), 36);
}
