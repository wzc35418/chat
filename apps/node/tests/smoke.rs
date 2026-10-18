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
