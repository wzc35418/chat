//! 跨进程端到端：真的启一个 chat-node 子进程，用 NodeClient 走完注册 → 建群 → 日志 → 信箱 → 附件，
//! 然后**杀掉进程**并用同一个 data-dir 重启，验证节点数据（head、日志、信箱、已发布 blob）都还在。
//!
//! 前置条件：先构建节点二进制（cargo build --offline -p chat-node），或用环境变量 CHAT_NODE_BIN 指向它。
//! 二进制缺失时本测试会**显式失败并给出可操作原因**，不会静默跳过（避免把“什么都没测”当成通过）。
//!
//! 不依赖任何网络：所有连接都是 127.0.0.1 随机端口；端口被占用时自动换端口重试。
use std::fs::File;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chat_node_client::{NodeClient, NodeEndpoint};
use chat_node_services::blob::{CipherManifest, PublicBlobDescriptor};
use chat_node_services::canonical::{hex_encode, sha256};
use chat_node_services::group::{GroupCandidate, GroupCreation, GroupMemberSpec, KIND_APPLICATION};
use chat_node_services::mailbox::MailboxItem;
use chat_node_services::registration::{
    DeviceCredential, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest, RegistrationCapability,
};
use chat_protocol::{DeviceId, GroupId, ObjectId, OperationId, SubmissionId};
use ed25519_dalek::{Signature, Signer, SigningKey};

const NODE_ID: &str = "node-cross-process";
const ACCOUNT_ID: &str = "acct-cross-process";
const OWNER_ROLE: &str = "owner";
const MEMBER_ROLE: &str = "member";
/// 节点种子文件内容（64 位十六进制 = 32 字节）。
const NODE_SEED_HEX: &str = "9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b";
/// 客户端自定的初始控制头摘要。
const INITIAL_CONTROL_HEAD: [u8; 32] = [0x11; 32];
const READY_TIMEOUT: Duration = Duration::from_secs(20);
const PORT_ATTEMPTS: usize = 3;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_secs()
}

fn temp_dir(tag: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("chat-node-xproc-{}-{tag}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn binary_name() -> &'static str {
    if cfg!(windows) { "chat-node.exe" } else { "chat-node" }
}

/// 找到已构建的 chat-node 二进制；找不到时给出可操作的失败原因。
fn locate_node_binary() -> PathBuf {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(explicit) = std::env::var("CHAT_NODE_BIN") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return path;
        }
        panic!("CHAT_NODE_BIN 指向的路径不是文件：{}", path.display());
    }
    // 集成测试可执行文件在 target/<profile>/deps/ 下，节点二进制在同级的 profile 目录。
    if let Ok(exe) = std::env::current_exe() {
        if let Some(profile_dir) = exe.parent().and_then(Path::parent) {
            candidates.push(profile_dir.join(binary_name()));
        }
    }
    // 退回 workspace 的默认 target 目录（CARGO_TARGET_DIR 覆盖时以第一个候选为准）。
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(workspace) = manifest_dir.ancestors().nth(2) {
        candidates.push(workspace.join("target").join("debug").join(binary_name()));
        if let Ok(target_dir) = std::env::var("CARGO_TARGET_DIR") {
            candidates.push(PathBuf::from(target_dir).join("debug").join(binary_name()));
        }
    }
    for candidate in &candidates {
        if candidate.is_file() {
            return candidate.clone();
        }
    }
    panic!(
        "找不到节点二进制 {}\n已尝试：{:?}\n先运行：cargo build --offline -p chat-node；或用环境变量 CHAT_NODE_BIN 指定路径",
        binary_name(),
        candidates
    );
}

/// 拿一个当前空闲的本地端口（监听后立刻释放，随后由节点子进程重新绑定）。
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

/// 一次最小 HTTP/1.1 GET；返回 (status, body)。
fn http_get(address: &str, path: &str) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect(address)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes())?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let text = String::from_utf8_lossy(&raw).to_string();
    let header_end = text
        .find("\r\n\r\n")
        .ok_or_else(|| std::io::Error::other("no header terminator"))?;
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| std::io::Error::other("no status code"))?;
    Ok((status, text[header_end + 4..].to_owned()))
}

/// 一个真实的 chat-node 子进程（自己的端口、自己的数据目录）。
struct NodeProcess {
    child: Option<Child>,
    address: String,
    node_id: String,
    node_public_key: [u8; 32],
    data_dir: PathBuf,
    log_dir: PathBuf,
}

impl NodeProcess {
    /// 启动节点并等到 /v1/health 就绪；端口冲突时换端口重试。
    fn start(binary: &Path, data_dir: &Path, seed_file: &Path, log_dir: &Path) -> Self {
        let mut attempts: Vec<String> = Vec::new();
        for attempt in 0..PORT_ATTEMPTS {
            let port = free_port();
            let address = format!("127.0.0.1:{port}");
            let out_path = log_dir.join(format!("node-out-{attempt}.log"));
            let err_path = log_dir.join(format!("node-err-{attempt}.log"));
            let stdout = File::create(&out_path).expect("node stdout log");
            let stderr = File::create(&err_path).expect("node stderr log");
            let child = Command::new(binary)
                .arg("--bind")
                .arg(&address)
                .arg("--data-dir")
                .arg(data_dir)
                .arg("--seed-file")
                .arg(seed_file)
                .arg("--node-id")
                .arg(NODE_ID)
                .stdin(Stdio::null())
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr))
                .spawn()
                .unwrap_or_else(|error| panic!("无法启动节点进程 {}: {error}", binary.display()));
            match Self::wait_ready(child, &address, &out_path, &err_path) {
                Ok(ready) => {
                    return Self {
                        child: Some(ready.0),
                        address,
                        node_id: ready.1,
                        node_public_key: ready.2,
                        data_dir: data_dir.to_path_buf(),
                        log_dir: log_dir.to_path_buf(),
                    };
                }
                Err(reason) => attempts.push(format!("端口 {port}: {reason}")),
            }
        }
        panic!(
            "节点在 {PORT_ATTEMPTS} 次尝试后仍未就绪：{}\n数据目录 {}\n日志目录 {}",
            attempts.join(" | "),
            data_dir.display(),
            log_dir.display()
        );
    }

    /// 轮询 /v1/health 直到返回 200；子进程先退出则立刻返回退出原因。
    fn wait_ready(
        mut child: Child,
        address: &str,
        out_path: &Path,
        err_path: &Path,
    ) -> Result<(Child, String, [u8; 32]), String> {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                let _ = child.wait();
                return Err(format!(
                    "节点提前退出（{status}）：stderr={} stdout={}",
                    read_log(err_path),
                    read_log(out_path)
                ));
            }
            match http_get(address, "/v1/health") {
                Ok((200, body)) => {
                    let value: serde_json::Value =
                        serde_json::from_str(&body).map_err(|error| format!("health 不是 JSON: {error}"))?;
                    let node_id = value["node_id"]
                        .as_str()
                        .ok_or("health 缺少 node_id")?
                        .to_owned();
                    let key_hex = value["node_public_key"]
                        .as_str()
                        .ok_or("health 缺少 node_public_key")?;
                    let key = chat_node_services::canonical::hex_decode::<32>("node public key", key_hex)
                        .map_err(|error| format!("node_public_key 非法: {}", error.code()))?;
                    return Ok((child, node_id, key));
                }
                _ => thread::sleep(Duration::from_millis(100)),
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{} 秒内没有就绪：stderr={} stdout={}",
                    READY_TIMEOUT.as_secs(),
                    read_log(err_path),
                    read_log(out_path)
                ));
            }
        }
    }

    /// 客户端端点：固定本次启动的节点公钥（换端口不换身份）。
    fn endpoint(&self) -> NodeEndpoint {
        NodeEndpoint::new(&format!("http://{}", self.address), &self.node_id)
            .expect("node endpoint")
            .with_node_public_key(self.node_public_key)
    }

    /// 该进程的 SQLite 库路径（同一 data-dir 重启会复用同一个库）。
    fn database_path(&self) -> PathBuf {
        self.data_dir.join("node.db")
    }

    /// 启动日志（只含 node_id/address/database）。
    fn startup_log(&self) -> String {
        self.log_dir
            .read_dir()
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| read_log(&entry.path()))
                    .filter(|text| text.contains("chat-node started"))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    /// 强杀（模拟崩溃）：不走优雅关闭，验证数据靠持久层而不是靠尽力而为的收尾。
    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for NodeProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

fn read_log(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default().trim().to_owned()
}

fn device_text(seed: u8) -> String {
    DeviceId::from_bytes([seed; 32]).encode()
}

fn group_text(seed: u8) -> String {
    GroupId::from_bytes([seed; 32]).encode()
}

fn submission_text(seed: u8) -> String {
    SubmissionId::from_bytes([seed; 32]).encode()
}

fn object_text(seed: u8) -> String {
    ObjectId::from_bytes([seed; 32]).encode()
}

fn operation_text(seed: u8) -> String {
    OperationId::from_bytes([seed; 32]).encode()
}

/// 设备密钥与标识。
struct TestDevice {
    device_id: String,
    key: SigningKey,
}

impl TestDevice {
    fn new(seed: u8) -> Self {
        Self {
            device_id: device_text(seed),
            key: SigningKey::from_bytes(&[seed; 32]),
        }
    }
}

/// 账户主管钥（签发设备凭据与注册能力）。
fn supervisor() -> SigningKey {
    SigningKey::from_bytes(&[0x11; 32])
}

fn registration_request(device: &TestDevice, capability_id: &str) -> RegisterDeviceRequest {
    let now = now_secs();
    let supervisor = supervisor();
    let supervisor_public_key = hex_encode(&supervisor.verifying_key().to_bytes());
    let mut credential = DeviceCredential {
        account_id: ACCOUNT_ID.into(),
        device_id: device.device_id.clone(),
        device_public_key: hex_encode(&device.key.verifying_key().to_bytes()),
        endpoint_id: format!("endpoint-{}", &device.device_id[..24]),
        credential_version: 1,
        not_before: now.saturating_sub(10),
        expires_at: now + 3_600,
        supervisor_public_key: supervisor_public_key.clone(),
        signature: String::new(),
    };
    let signature: Signature = supervisor.sign(&credential.signing_bytes().expect("credential bytes"));
    credential.signature = hex_encode(&signature.to_bytes());
    let mut capability = RegistrationCapability {
        capability_id: capability_id.to_owned(),
        account_id: ACCOUNT_ID.into(),
        device_id: device.device_id.clone(),
        purpose: REGISTRATION_CAPABILITY_PURPOSE.into(),
        issued_at: now.saturating_sub(10),
        expires_at: now + 600,
        revocation_version: 1,
        supervisor_public_key,
        signature: String::new(),
    };
    let signature: Signature = supervisor.sign(&capability.signing_bytes().expect("capability bytes"));
    capability.signature = hex_encode(&signature.to_bytes());
    RegisterDeviceRequest {
        account_id: ACCOUNT_ID.into(),
        credential,
        capability,
    }
}

fn group_creation(group_id: &str, members: &[(&TestDevice, &str)]) -> GroupCreation {
    GroupCreation {
        group_id: group_id.to_owned(),
        owner_account_id: ACCOUNT_ID.into(),
        initial_control_head: hex_encode(&INITIAL_CONTROL_HEAD),
        policy_version: 1,
        mls_epoch: 0,
        members: members
            .iter()
            .map(|(device, role)| GroupMemberSpec {
                device_id: device.device_id.clone(),
                role: (*role).to_owned(),
            })
            .collect(),
    }
}

fn application_candidate(
    group_id: &str,
    author: &TestDevice,
    submission_seed: u8,
    payload: &[u8],
) -> GroupCandidate {
    GroupCandidate {
        protocol_major: 1,
        protocol_minor: 0,
        submission_id: submission_text(submission_seed),
        group_id: group_id.to_owned(),
        author_device_id: author.device_id.clone(),
        kind: KIND_APPLICATION.to_owned(),
        expected_control_head: hex_encode(&INITIAL_CONTROL_HEAD),
        policy_version: 1,
        mls_epoch: 0,
        encrypted_payload: hex_encode(payload),
        control_metadata: None,
    }
}

fn mailbox_item(
    item_id: &str,
    group_id: &str,
    target: &TestDevice,
    source_seq: u64,
    payload: &[u8],
) -> MailboxItem {
    MailboxItem {
        item_id: item_id.to_owned(),
        group_id: group_id.to_owned(),
        target_device_id: target.device_id.clone(),
        source_seq,
        expires_at: now_secs() + 600,
        ciphertext_envelope: hex_encode(payload),
    }
}

fn ciphertext_digest(chunks: &[&[u8]]) -> String {
    let mut combined = Vec::new();
    for chunk in chunks {
        combined.extend_from_slice(chunk);
    }
    hex_encode(&sha256(&combined))
}

fn write_node_seed(dir: &Path) -> PathBuf {
    let path = dir.join("node-seed.hex");
    let mut file = File::create(&path).expect("seed file");
    file.write_all(NODE_SEED_HEX.as_bytes()).expect("write seed");
    path
}

fn client(node: &NodeProcess, device: &TestDevice) -> NodeClient {
    NodeClient::new(node.endpoint(), device.device_id.clone(), device.key.clone())
        .expect("node client")
}

/// 跨进程完整链路 + 强杀重启后的持久性。
#[tokio::test]
async fn node_process_and_client_survive_a_hard_restart() {
    let binary = locate_node_binary();
    let dir = temp_dir("main");
    let data_dir = dir.join("data");
    let seed_file = write_node_seed(&dir);
    let mut node = NodeProcess::start(&binary, &data_dir, &seed_file, &dir);
    assert_eq!(node.node_id, NODE_ID);
    let startup = node.startup_log();
    assert!(startup.contains("database="), "启动日志应包含库路径：{startup}");
    // 库路径必须落在本测试的临时数据目录里（证明真的用 --data-dir 启动）。
    assert!(
        startup.contains(&data_dir.display().to_string()),
        "启动日志的库路径应指向 {}：{startup}",
        data_dir.display()
    );
    assert!(
        !node.startup_log().to_lowercase().contains(&NODE_SEED_HEX[..8]),
        "启动日志不得包含种子材料"
    );

    let owner = TestDevice::new(21);
    let member = TestDevice::new(22);
    let owner_client = client(&node, &owner);
    let member_client = client(&node, &member);

    // 1) 注册两台设备（真挑战/真签名，节点默认每账户 2 台上限）。
    let owner_registration = owner_client
        .register_device(&registration_request(&owner, "cap-owner"))
        .await
        .expect("owner registers");
    assert_eq!(owner_registration.device_id, owner.device_id);
    member_client
        .register_device(&registration_request(&member, "cap-member"))
        .await
        .expect("member registers");

    // 2) 建群（两成员）。
    let group = group_text(61);
    let creation = group_creation(&group, &[(&owner, OWNER_ROLE), (&member, MEMBER_ROLE)]);
    let registration = owner_client.create_group(&creation).await.expect("group created");
    assert_eq!(registration.group_id, group);

    // 3) append 一条密文，并用同一 submission 重试证明幂等。
    let candidate = application_candidate(&group, &owner, 71, b"cross-process ciphertext");
    let first = owner_client.append_entry(&candidate).await.expect("entry accepted");
    assert_eq!(first.seq, 1);
    let retried = owner_client
        .append_entry(&candidate)
        .await
        .expect("idempotent retry");
    assert_eq!(retried.seq, first.seq);
    assert_eq!(retried.log_hash, first.log_hash);
    assert_eq!(retried.candidate_hash, first.candidate_hash);

    // 4) 日志与 head。
    let head = owner_client.get_head(&group).await.expect("head");
    assert_eq!(head.seq, first.seq);
    let page = owner_client.fetch_log(&group, 0, 50).await.expect("log page");
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.head.seq, head.seq);
    assert_eq!(page.entries[0].receipt.seq, first.seq);
    assert_eq!(page.entries[0].candidate.candidate.submission_id, candidate.submission_id);

    // 5) 信箱：owner 投递两条（item-acked 会被 ACK，item-pending 留到重启后验证）。
    owner_client
        .store_mailbox_item(&mailbox_item("item-acked", &group, &member, first.seq, b"mail-1"))
        .await
        .expect("first mailbox item");
    owner_client
        .store_mailbox_item(&mailbox_item("item-pending", &group, &member, first.seq, b"mail-2"))
        .await
        .expect("second mailbox item");
    let mailbox = member_client.read_mailbox_page(None, 20).await.expect("mailbox page");
    assert_eq!(mailbox.items.len(), 2);
    let acked = member_client
        .ack_mailbox_items(&["item-acked".to_owned()])
        .await
        .expect("ack");
    assert_eq!(acked.acknowledged, vec!["item-acked".to_owned()]);
    let remaining = member_client.read_mailbox_page(None, 20).await.expect("mailbox page");
    assert_eq!(remaining.items.len(), 1);
    assert_eq!(remaining.items[0].item_id, "item-pending");

    // 6) 附件 blob：两块密文的上传/发布/读取往返。
    let chunk_a: &[u8] = b"cross-process-chunk-a";
    let chunk_b: &[u8] = b"chunk-b";
    let total = (chunk_a.len() + chunk_b.len()) as u64;
    let descriptor = PublicBlobDescriptor {
        blob_id: object_text(81),
        group_id: group.clone(),
        source_seq: first.seq,
        ciphertext_bytes: total,
        chunk_bytes: u32::try_from(chunk_a.len()).expect("chunk size"),
        chunk_count: 2,
    };
    let session = owner_client
        .begin_blob_upload(&operation_text(91), &descriptor)
        .await
        .expect("blob upload begins");
    owner_client
        .put_blob_chunk(&session.upload_id, 0, chunk_a)
        .await
        .expect("chunk 0");
    // 同块重传必须幂等（不占第二份、返回同一份回执）。
    let repeated = owner_client
        .put_blob_chunk(&session.upload_id, 0, chunk_a)
        .await
        .expect("chunk 0 retry");
    assert_eq!(repeated.received_chunks, 1);
    owner_client
        .put_blob_chunk(&session.upload_id, 1, chunk_b)
        .await
        .expect("chunk 1");
    let manifest = CipherManifest {
        chunk_count: 2,
        ciphertext_bytes: total,
        ciphertext_digest: ciphertext_digest(&[chunk_a, chunk_b]),
    };
    let receipt = owner_client
        .complete_blob_upload(&session.upload_id, &manifest)
        .await
        .expect("blob published");
    assert_eq!(receipt.blob_id, descriptor.blob_id);
    assert_eq!(
        owner_client
            .read_blob_chunk(&descriptor.blob_id, 0)
            .await
            .expect("read chunk 0"),
        chunk_a
    );
    // 附件存在时已在群内的成员也能读。
    assert_eq!(
        member_client
            .read_blob_chunk(&descriptor.blob_id, 1)
            .await
            .expect("member reads chunk 1"),
        chunk_b
    );

    // 7) 强杀进程（不走优雅关闭），再用同一 data-dir 重启。
    assert!(node.database_path().exists(), "库文件应已落盘");
    node.kill();
    let mut restarted = NodeProcess::start(&binary, &data_dir, &seed_file, &dir);
    assert_eq!(restarted.node_id, NODE_ID);
    assert_eq!(
        restarted.node_public_key, node.node_public_key,
        "同一 data-dir + 同一种子必须恢复同一节点身份"
    );
    let owner_after = client(&restarted, &owner);
    let member_after = client(&restarted, &member);

    // 8) 重启后：head、日志、信箱、已发布 blob 都还在。
    let head_after = owner_after.get_head(&group).await.expect("head after restart");
    assert_eq!(head_after.seq, head.seq);
    assert_eq!(head_after.log_hash, head.log_hash);
    let page_after = owner_after.fetch_log(&group, 0, 50).await.expect("log after restart");
    assert_eq!(page_after.entries.len(), 1);
    assert_eq!(
        page_after.entries[0].candidate.candidate.submission_id,
        candidate.submission_id
    );
    assert_eq!(page_after.entries[0].receipt.seq, first.seq);
    let mailbox_after = member_after
        .read_mailbox_page(None, 20)
        .await
        .expect("mailbox after restart");
    assert_eq!(mailbox_after.items.len(), 1);
    assert_eq!(mailbox_after.items[0].item_id, "item-pending");
    assert_eq!(
        owner_after
            .read_blob_chunk(&descriptor.blob_id, 0)
            .await
            .expect("blob chunk 0 after restart"),
        chunk_a
    );
    assert_eq!(
        member_after
            .read_blob_chunk(&descriptor.blob_id, 1)
            .await
            .expect("blob chunk 1 after restart"),
        chunk_b
    );
    // 重启后仍可继续写（新条目接在原 head 之后，链不断）。
    let follow_up = application_candidate(&group, &owner, 72, b"after restart");
    let appended = owner_after
        .append_entry(&follow_up)
        .await
        .expect("append after restart");
    assert_eq!(appended.seq, head.seq + 1);
    assert_eq!(appended.previous_hash, head.log_hash);

    assert!(restarted.database_path().exists(), "重启后仍是同一个库文件");
    restarted.kill();
    let _ = std::fs::remove_dir_all(&dir);
}
