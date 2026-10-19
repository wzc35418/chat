//! apps/node 冒烟测试：证明 run_node 真的能把服务起起来并应答 /v1/health。
//!
//! 两层证据：
//! 1. 进程内 `spawn_node`：绑定真端口 → 真 TCP 健康检查 → 优雅关闭 → 库文件耐久存在；
//! 2. 真实二进制：用 `CARGO_BIN_EXE_chat-node` 起子进程，解析启动日志里的地址并做健康检查，
//!    同时证明启动日志**不含**密钥材料。
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use chat_node::{NodeConfig, spawn_node};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chat-node-smoke-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn write_seed(dir: &Path, seed: [u8; 32]) -> PathBuf {
    let path = dir.join("node-seed.bin");
    std::fs::write(&path, seed).expect("seed file");
    path
}

/// 一次最小 HTTP/1.1 GET，返回 (status, body)。
fn http_get(address: &str, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(address).expect("connect node");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    let request = format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).expect("write request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    let text = String::from_utf8_lossy(&raw).to_string();
    let header_end = text.find("\r\n\r\n").expect("response header terminator");
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .expect("status code");
    (status, text[header_end + 4..].to_owned())
}

#[test]
fn in_process_node_serves_health_and_shuts_down_cleanly() {
    let dir = temp_dir("in-process");
    let seed_file = write_seed(&dir, [0x5a; 32]);
    let config = NodeConfig {
        bind: "127.0.0.1:0".parse().expect("addr"),
        data_dir: dir.join("data"),
        seed_file,
        node_id: Some("node-smoke".into()),
        allow_public_bind: false,
        init_seed: false,
        expect_public_key: None,
        require_capability: false,
        audit: true,
    };
    let running = spawn_node(config).expect("node starts");
    let address = running.address.to_string();
    assert_eq!(running.node_id, "node-smoke");
    assert!(running.database_path.exists(), "database is created before listening");

    // /v1/health：匿名、无秘密、返回节点 ID + schema 版本。
    let (status, body) = http_get(&address, "/v1/health");
    assert_eq!(status, 200, "health body={body}");
    assert!(body.contains("\"status\":\"ok\""), "body={body}");
    assert!(body.contains("\"node_id\":\"node-smoke\""), "body={body}");
    assert!(body.contains("\"schema_version\":1"), "body={body}");
    // 节点公钥是公开材料，但仍不能出现任何“密钥/种子/密码”字样。
    assert!(!body.contains("signing"), "body={body}");
    assert!(!body.contains("secret"), "body={body}");
    assert!(!body.contains("seed"), "body={body}");

    // 健康检查之外的端点仍然需要鉴权（缺头 → 400，不是 500/放行）。
    let (status, body) = http_get(&address, "/v1/groups/grp:1:00/head");
    assert_eq!(status, 400, "body={body}");
    let (status, _) = http_get(&address, "/v1/nothing");
    assert_eq!(status, 404);

    running.shutdown().expect("clean shutdown");
    // 关闭后端口不再接受连接。
    assert!(TcpStream::connect(&address).is_err(), "listener is closed");
    // 数据库耐久存在，说明启动顺序是“先持久层、后监听”。
    assert!(dir.join("data").join("node.db").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// 从子进程 stdout 里读一行包含 marker 的输出（带超时）。
fn wait_for_line(reader: BufReader<std::process::ChildStdout>, marker: &str) -> String {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    loop {
        match receiver.recv_timeout(Duration::from_secs(20)) {
            Ok(line) => {
                if line.contains(marker) {
                    return line;
                }
            }
            Err(_) => panic!("node did not log {marker} within the deadline"),
        }
    }
}

#[test]
fn binary_starts_logs_only_public_identity_and_serves_health() {
    let dir = temp_dir("binary");
    let seed = [0xa5_u8; 32];
    let seed_file = write_seed(&dir, seed);
    let data_dir = dir.join("data");
    let child = Command::new(env!("CARGO_BIN_EXE_chat-node"))
        .arg("--bind")
        .arg("127.0.0.1:0")
        .arg("--data-dir")
        .arg(&data_dir)
        .arg("--seed-file")
        .arg(&seed_file)
        .arg("--node-id")
        .arg("node-binary")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn chat-node");
    let mut child = child;
    let stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");
    let started = wait_for_line(BufReader::new(stdout), "chat-node started");
    // 启动日志只打节点 ID / 地址 / 库路径。
    assert!(started.contains("node_id=node-binary"), "line={started}");
    let address = started
        .split("address=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .expect("address in startup line")
        .to_owned();
    assert!(started.contains("database="), "line={started}");
    // 绝不打密钥种子（十六进制形式）。
    let seed_hex: String = seed.iter().map(|byte| format!("{byte:02x}")).collect();
    assert!(!started.contains(&seed_hex), "startup line leaked the seed");
    assert!(!started.to_lowercase().contains("secret"));

    let (status, body) = http_get(&address, "/v1/health");
    assert_eq!(status, 200, "body={body}");
    assert!(body.contains("\"node_id\":\"node-binary\""), "body={body}");

    // 关闭子进程并收集 stderr，确认没有异常/泄密输出。
    child.kill().expect("kill node");
    let _ = child.wait();
    let mut errors = String::new();
    let _ = stderr.read_to_string(&mut errors);
    assert!(!errors.contains(&seed_hex), "stderr leaked the seed: {errors}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- CAP 包：强制模式冒烟

/// 冒烟用的设备材料（设备 ID + 签名钥 + 主管钥签发的凭据/注册能力）。
struct SmokeDevice {
    device_id: String,
    key: ed25519_dalek::SigningKey,
}

impl SmokeDevice {
    fn new(seed: u8) -> Self {
        let device_id = chat_protocol::DeviceId::from_bytes([seed; 32]).encode();
        Self {
            device_id,
            key: ed25519_dalek::SigningKey::from_bytes(&[seed; 32]),
        }
    }
}

fn supervisor() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[11_u8; 32])
}

fn device_registration(device: &SmokeDevice, capability_id: &str, now: u64) -> chat_node_services::registration::RegisterDeviceRequest {
    use chat_node_services::canonical::hex_encode;
    use chat_node_services::registration::{
        DeviceCredential, RegisterDeviceRequest, RegistrationCapability,
        REGISTRATION_CAPABILITY_PURPOSE,
    };
    use ed25519_dalek::Signer;

    let supervisor = supervisor();
    let supervisor_public_key = hex_encode(&supervisor.verifying_key().to_bytes());
    let account_id = "acct-smoke";
    let mut credential = DeviceCredential {
        account_id: account_id.to_owned(),
        device_id: device.device_id.clone(),
        device_public_key: hex_encode(&device.key.verifying_key().to_bytes()),
        endpoint_id: "endpoint-smoke".to_owned(),
        credential_version: 1,
        not_before: now.saturating_sub(10),
        expires_at: now + 3_600,
        supervisor_public_key: supervisor_public_key.clone(),
        signature: String::new(),
    };
    credential.signature = hex_encode(&supervisor.sign(&credential.signing_bytes().expect("credential bytes")).to_bytes());
    let mut capability = RegistrationCapability {
        capability_id: capability_id.to_owned(),
        account_id: account_id.to_owned(),
        device_id: device.device_id.clone(),
        purpose: REGISTRATION_CAPABILITY_PURPOSE.into(),
        issued_at: now.saturating_sub(10),
        expires_at: now + 600,
        revocation_version: 1,
        supervisor_public_key,
        signature: String::new(),
    };
    capability.signature = hex_encode(&supervisor.sign(&capability.signing_bytes().expect("capability bytes")).to_bytes());
    RegisterDeviceRequest {
        account_id: account_id.to_owned(),
        credential,
        capability,
    }
}

#[test]
fn capability_required_mode_serves_401_without_token_and_200_with_one() {
    use chat_node_client::{NodeClient, NodeEndpoint};
    use chat_node_services::auth::ChallengePurpose;
    use chat_node_services::blob::PublicBlobDescriptor;
    use chat_node_services::group::{
        GroupCandidate, GroupCreation, GroupMemberSpec, KIND_APPLICATION,
    };
    use chat_node_services::canonical::hex_encode;
    use ed25519_dalek::Signer;

    let dir = temp_dir("capability");
    let seed_file = write_seed(&dir, [0x7c; 32]);
    let config = NodeConfig {
        bind: "127.0.0.1:0".parse().expect("addr"),
        data_dir: dir.join("data"),
        seed_file,
        node_id: Some("node-capability-smoke".into()),
        allow_public_bind: false,
        init_seed: false,
        expect_public_key: None,
        require_capability: true,
        audit: true,
    };
    let running = spawn_node(config).expect("node starts with capability enforcement");
    assert!(running.require_capability, "the handle reports the effective mode");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let address = running.address.to_string();
    runtime.block_on(async move {
        let endpoint = NodeEndpoint::new(&format!("http://{address}"), "node-capability-smoke")
            .expect("endpoint")
            .with_node_public_key(running.node_public_key);
        let device = SmokeDevice::new(0x31);
        let client = NodeClient::new(endpoint, device.device_id.clone(), device.key.clone())
            .expect("client");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        client
            .register_device(&device_registration(&device, "cap-smoke-device", now))
            .await
            .expect("device registers against the real process");

        // 单成员群 + 一条应用条目（附件会话必须绑定已接受条目）。
        let group_id = chat_protocol::GroupId::from_bytes([0x41; 32]).encode();
        let creation = GroupCreation {
            group_id: group_id.clone(),
            owner_account_id: "acct-smoke".into(),
            initial_control_head: hex_encode(&[0x11_u8; 32]),
            policy_version: 1,
            mls_epoch: 0,
            members: vec![GroupMemberSpec {
                device_id: device.device_id.clone(),
                role: "owner".to_owned(),
            }],
        };
        client.create_group(&creation).await.expect("group created");
        let blur = b"attachment message";
        let candidate = GroupCandidate {
            protocol_major: 1,
            protocol_minor: 0,
            submission_id: chat_protocol::SubmissionId::from_bytes([0x42; 32]).encode(),
            group_id: group_id.clone(),
            author_device_id: device.device_id.clone(),
            kind: KIND_APPLICATION.to_owned(),
            expected_control_head: hex_encode(&[0x11_u8; 32]),
            policy_version: 1,
            mls_epoch: 0,
            encrypted_payload: hex_encode(blur),
            control_metadata: None,
        };
        let unsigned = candidate
            .to_unsigned(chat_protocol::MAX_CIPHERTEXT_BYTES)
            .expect("candidate converts");
        let signing = chat_protocol::candidate_signing_bytes(&unsigned).expect("signing bytes");
        // 自签校验（candidate_signing_bytes 覆盖全部公开字段）后才提交。
        let signature = device.key.sign(signing.as_bytes());
        assert!(!signature.to_bytes().is_empty());
        let receipt = client
            .append_entry(&candidate)
            .await
            .expect("entry accepted");

        let blob_id = chat_protocol::ObjectId::from_bytes([0x51; 32]).encode();
        let descriptor = PublicBlobDescriptor {
            blob_id: blob_id.clone(),
            group_id: group_id.clone(),
            source_seq: receipt.seq,
            ciphertext_bytes: 4,
            chunk_bytes: 4,
            chunk_count: 1,
        };
        let session = client
            .begin_blob_upload(
                &chat_protocol::OperationId::from_bytes([0x52; 32]).encode(),
                &descriptor,
            )
            .await
            .expect("upload session");

        // ① 不带能力令牌的**已认证**请求：401 AUTH_REQUIRED。
        let challenge = client
            .fetch_challenge(ChallengePurpose::RequestAuth)
            .await
            .expect("challenge");
        let path = format!("/v1/blobs/{}/chunks/0", session.upload_id);
        let error = client
            .send_with_challenge(&challenge, "PUT", &path, "", b"head")
            .await
            .expect_err("the node must reject a token-less chunk write");
        assert_eq!(
            error.node_code(),
            Some("AUTH_REQUIRED"),
            "token-less write must be AUTH_REQUIRED, got {error:?}"
        );

        // ② 带令牌：200（客户端先申请令牌，再上传）。
        let token = client
            .issue_capability(&session.upload_id, "blob:upload", None)
            .await
            .expect("capability issued");
        assert!(token.starts_with("cap1."));
        let chunk_receipt = client
            .put_blob_chunk(&session.upload_id, 0, b"head")
            .await
            .expect("chunk accepted with a capability token");
        assert_eq!(chunk_receipt.chunk_index, 0);
    });
    running.shutdown().expect("clean shutdown");
}
