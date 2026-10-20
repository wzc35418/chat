//! run_node：可部署节点二进制（API-010）。
//!
//! 启动顺序固定为“先持久层、后监听”：
//!
//! 1. 解析并校验配置（bind / 数据目录 / 节点密钥种子文件 / 可选固定公钥）；
//! 2. 读取节点签名种子（32 字节原文或 64 位十六进制），派生公钥与默认 node_id；
//! 3. 打开 SQLite 并执行迁移（NodeService::open 内部完成）；
//! 4. 绑定监听地址并启动 axum 全部端点；
//! 5. 收到 Ctrl+C 后停止接受新请求、等待在途请求结束、关闭数据库。
//!
//! 日志只输出节点 ID、监听地址与库路径；**密钥种子、签名私钥与任何正文都不进日志**。
use std::env;
use std::fmt;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::thread;

use chat_node_api::build_router;
use chat_node_api::state::AppState;
use chat_node_services::error::NodeError;
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};

/// 启动失败原因（全部是显式、可诊断且不含秘密的错误）。
#[derive(Debug)]
pub enum NodeStartError {
    /// 配置不合法（缺失/越界/不安全的默认值）。
    Config(&'static str),
    /// 节点服务初始化失败（迁移、身份固定等）。
    Service(NodeError),
    /// 监听地址无法绑定。
    Bind(String),
    /// 文件系统操作失败（种子文件、数据目录）。
    Io(&'static str),
    /// 运行期失败（serve 循环、信号处理）。
    Runtime(&'static str),
}

impl fmt::Display for NodeStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(detail) => write!(formatter, "INVALID_CONFIG ({detail})"),
            Self::Service(error) => write!(formatter, "{}", error.code()),
            Self::Bind(detail) => write!(formatter, "BIND_FAILED ({detail})"),
            Self::Io(detail) => write!(formatter, "FILE_IO_FAILURE ({detail})"),
            Self::Runtime(detail) => write!(formatter, "NODE_START_FAILED ({detail})"),
        }
    }
}

impl std::error::Error for NodeStartError {}

impl From<NodeError> for NodeStartError {
    fn from(error: NodeError) -> Self {
        Self::Service(error)
    }
}

/// 节点启动配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeConfig {
    /// 监听地址（默认 127.0.0.1:8080；非回环地址需要显式允许）。
    pub bind: SocketAddr,
    /// 数据目录（内含 node.db）。
    pub data_dir: PathBuf,
    /// 节点签名种子文件路径（32 字节原文或 64 位小写十六进制）。
    pub seed_file: PathBuf,
    /// 节点标识；缺省时由公钥派生。
    pub node_id: Option<String>,
    /// 允许绑定非回环地址（默认拒绝：进程内没有 TLS 终止）。
    pub allow_public_bind: bool,
    /// 允许在种子文件缺失时创建它（显式 opt-in，避免静默生成生产密钥）。
    pub init_seed: bool,
    /// 可选的固定公钥：与种子派生出的公钥不一致即拒绝启动。
    pub expect_public_key: Option<[u8; 32]>,
    /// CAP 包：附件块端点是否强制能力令牌（默认关闭以保持旧客户端可用）。
    pub require_capability: bool,
    /// NODE-018：是否记录审计（默认开；`--no-audit` / `CHAT_NODE_AUDIT=0` 关闭）。
    pub audit: bool,
    /// 可选 TLS：给出证书与私钥即用 https 监听（否则明文 HTTP）。
    pub tls: Option<TlsConfig>,
}

/// 节点 TLS 监听配置（PEM 文件路径）。 */
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsConfig {
    /// 证书链（PEM，可含多张）。
    pub cert_file: PathBuf,
    /// 私钥（PEM，PKCS#8 或 RSA）。
    pub key_file: PathBuf,
}

const DEFAULT_BIND: &'static str = "127.0.0.1:8080";
const DEFAULT_DATA_DIR: &'static str = "chat-node-data";
const ENV_BIND: &'static str = "CHAT_NODE_BIND";
const ENV_DATA_DIR: &'static str = "CHAT_NODE_DATA_DIR";
const ENV_SEED_FILE: &'static str = "CHAT_NODE_SEED_FILE";
const ENV_NODE_ID: &'static str = "CHAT_NODE_ID";
const ENV_ALLOW_PUBLIC_BIND: &'static str = "CHAT_NODE_ALLOW_PUBLIC_BIND";
const ENV_INIT_SEED: &'static str = "CHAT_NODE_INIT_SEED";
const ENV_EXPECT_PUBLIC_KEY: &'static str = "CHAT_NODE_EXPECT_PUBLIC_KEY";
const ENV_REQUIRE_CAPABILITY: &'static str = "CHAT_NODE_CAPABILITY_REQUIRED";
const ENV_CAPABILITY_MAX_TTL: &'static str = "CHAT_NODE_CAPABILITY_MAX_TTL_SECONDS";
const ENV_AUDIT: &'static str = "CHAT_NODE_AUDIT";
const ENV_TLS_CERT: &'static str = "CHAT_NODE_TLS_CERT_FILE";
const ENV_TLS_KEY: &'static str = "CHAT_NODE_TLS_KEY_FILE";

impl NodeConfig {
    /// 解析命令行参数与环境变量（命令行优先）。
    ///
    /// 只接受显式开关，不做“尽力猜测”；未知参数直接拒绝。
    pub fn from_args_and_env(
        args: impl IntoIterator<Item = String>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, NodeStartError> {
        let mut bind = env(ENV_BIND).unwrap_or_else(|| DEFAULT_BIND.to_owned());
        let mut data_dir = env(ENV_DATA_DIR).unwrap_or_else(|| DEFAULT_DATA_DIR.to_owned());
        let mut seed_file = env(ENV_SEED_FILE);
        let mut node_id = env(ENV_NODE_ID);
        let mut allow_public_bind = env(ENV_ALLOW_PUBLIC_BIND).as_deref() == Some("1");
        let mut init_seed = env(ENV_INIT_SEED).as_deref() == Some("1");
        let mut expect_public_key = env(ENV_EXPECT_PUBLIC_KEY);
        // CAP 包：能力令牌强制开关。环境变量只接受显式真/假文本，其它一律启动失败
        // （不接受“拼写错误 = 关闭”，否则会以为已经强制而实际没开）。
        let mut require_capability = match env(ENV_REQUIRE_CAPABILITY).as_deref() {
            None => false,
            Some(value) => parse_flag(value).ok_or(NodeStartError::Config(
                "CHAT_NODE_CAPABILITY_REQUIRED must be one of 1/true/yes/on/0/false/no/off",
            ))?,
        };

        // NODE-018：审计默认开启，只有显式关闭才关闭（与能力令牌开关一样不做“尽力猜测”）。
        let mut audit = match env(ENV_AUDIT).as_deref() {
            None => true,
            Some(value) => parse_flag(value).ok_or(NodeStartError::Config(
                "CHAT_NODE_AUDIT must be one of 1/true/yes/on/0/false/no/off",
            ))?,
        };

        let mut tls_cert = env(ENV_TLS_CERT);
        let mut tls_key = env(ENV_TLS_KEY);

        let mut arguments = args.into_iter();
        while let Some(argument) = arguments.next() {
            let mut take_value = |name: &'static str| -> Result<String, NodeStartError> {
                arguments
                    .next()
                    .ok_or(NodeStartError::Config(name))
            };
            match argument.as_str() {
                "--bind" => bind = take_value("--bind needs a value")?,
                "--data-dir" => data_dir = take_value("--data-dir needs a value")?,
                "--seed-file" => seed_file = Some(take_value("--seed-file needs a value")?),
                "--node-id" => node_id = Some(take_value("--node-id needs a value")?),
                "--expect-public-key" => {
                    expect_public_key = Some(take_value("--expect-public-key needs a value")?)
                }
                "--allow-public-bind" => allow_public_bind = true,
                "--require-capability" => require_capability = true,
                "--no-audit" => audit = false,
                "--init-seed" => init_seed = true,
                "--tls-cert" => tls_cert = Some(take_value("--tls-cert needs a value")?),
                "--tls-key" => tls_key = Some(take_value("--tls-key needs a value")?),
                "--help" | "-h" => return Err(NodeStartError::Config("usage: chat-node --bind <addr> --data-dir <dir> --seed-file <file> [--node-id <id>] [--expect-public-key <hex>] [--allow-public-bind] [--init-seed] [--require-capability] [--tls-cert <pem> --tls-key <pem>]")),
                other => {
                    let _ = other;
                    return Err(NodeStartError::Config("unknown command line argument"));
                }
            }
        }

        let bind: SocketAddr = bind
            .parse()
            .map_err(|_| NodeStartError::Config("--bind must be an ip:port socket address"))?;
        if !bind.ip().is_loopback() && !allow_public_bind {
            // 进程内没有 TLS 终止：默认只监听回环，公开暴露必须显式确认。
            return Err(NodeStartError::Config(
                "refusing a non-loopback bind without --allow-public-bind (no in-process TLS)",
            ));
        }
        if data_dir.is_empty() {
            return Err(NodeStartError::Config("--data-dir must not be empty"));
        }
        let seed_file = seed_file.ok_or(NodeStartError::Config(
            "--seed-file (or CHAT_NODE_SEED_FILE) is required",
        ))?;
        if seed_file.is_empty() {
            return Err(NodeStartError::Config("--seed-file must not be empty"));
        }
        if let Some(node_id) = &node_id {
            if node_id.is_empty() || node_id.len() > 128 {
                return Err(NodeStartError::Config("--node-id must be 1..=128 bytes"));
            }
            if !node_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            {
                return Err(NodeStartError::Config("--node-id has unsupported characters"));
            }
        }
        let expect_public_key = match expect_public_key {
            Some(text) => Some(chat_node_services::canonical::hex_decode::<32>(
                "expected public key",
                text.trim(),
            )
            .map_err(|_| NodeStartError::Config("--expect-public-key must be 64 hex characters"))?),
            None => None,
        };
        // TLS 必须成对给出：只给一半属于配置错误（绝不静默降级成明文）。
        let tls = match (tls_cert, tls_key) {
            (None, None) => None,
            (Some(_), None) => {
                return Err(NodeStartError::Config("--tls-cert requires --tls-key"));
            }
            (None, Some(_)) => {
                return Err(NodeStartError::Config("--tls-key requires --tls-cert"));
            }
            (Some(cert), Some(key)) => {
                if cert.trim().is_empty() || key.trim().is_empty() {
                    return Err(NodeStartError::Config("--tls-cert / --tls-key must not be empty"));
                }
                Some(TlsConfig {
                    cert_file: PathBuf::from(cert),
                    key_file: PathBuf::from(key),
                })
            }
        };
        if tls.is_some() && !bind.ip().is_loopback() && !allow_public_bind {
            // 有 TLS 也不能静默公开暴露：公开绑定依然要显式确认。
            return Err(NodeStartError::Config(
                "refusing a non-loopback bind without --allow-public-bind",
            ));
        }
        Ok(Self {
            bind,
            data_dir: PathBuf::from(data_dir),
            seed_file: PathBuf::from(seed_file),
            node_id,
            allow_public_bind,
            init_seed,
            expect_public_key,
            require_capability,
            audit,
            tls,
        })
    }

    /// 从进程环境读取（生产路径）。
    pub fn from_process() -> Result<Self, NodeStartError> {
        Self::from_args_and_env(env::args().skip(1), |name| env::var(name).ok())
    }
}
/// 解析显式布尔开关文本（1/true/yes/on 与 0/false/no/off，大小写不敏感）。
///
/// 其它任何取值都返回 `None`，由调用方转成启动失败——绝不把拼写错误当成“关闭”。
fn parse_flag(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" | "" => Some(false),
        _ => None,
    }
}

/// 读取节点签名种子：32 字节原文，或允许尾部换行的 64 位小写十六进制。
pub fn load_signing_seed(path: &Path, init_seed: bool) -> Result<[u8; 32], NodeStartError> {
    if !path.exists() {
        if !init_seed {
            return Err(NodeStartError::Config(
                "seed file does not exist (pass --init-seed once to create it)",
            ));
        }
        let mut seed = [0_u8; 32];
        rand::fill(&mut seed);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|_| NodeStartError::Io("seed directory"))?;
            }
        }
        fs::write(path, seed).map_err(|_| NodeStartError::Io("seed file could not be created"))?;
        // 只报告路径，不报告内容。
        println!("chat-node created a new node seed at {}", path.display());
        return Ok(seed);
    }
    let bytes = fs::read(path).map_err(|_| NodeStartError::Io("seed file could not be read"))?;
    if bytes.len() == 32 {
        let mut seed = [0_u8; 32];
        seed.copy_from_slice(&bytes);
        return Ok(seed);
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| NodeStartError::Config("seed file must be 32 raw bytes or 64 hex characters"))?;
    chat_node_services::canonical::hex_decode::<32>("node signing seed", text.trim())
        .map_err(|_| NodeStartError::Config("seed file must be 32 raw bytes or 64 hex characters"))
}

/// 运行中的节点句柄：地址与身份已知，关闭时停止接受新请求并等待服务线程退出。
pub struct RunningNode {
    /// 实际绑定的地址（`{bind}:0` 会得到操作系统分配的端口）。
    pub address: SocketAddr,
    /// 节点标识（配置值或由公钥派生）。
    pub node_id: String,
    /// 节点签名公钥（公开材料）。
    pub node_public_key: [u8; 32],
    /// SQLite 库路径。
    pub database_path: PathBuf,
    /// CAP 包：本进程是否强制附件块端点携带能力令牌。
    pub require_capability: bool,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<Result<(), NodeStartError>>>,
}

impl fmt::Debug for RunningNode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RunningNode")
            .field("address", &self.address)
            .field("node_id", &self.node_id)
            .field("database_path", &self.database_path)
            .finish_non_exhaustive()
    }
}

impl RunningNode {
    /// 请求优雅关闭：停止接受新请求、等在途请求结束、关闭数据库。
    pub fn shutdown(mut self) -> Result<(), NodeStartError> {
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(());
        }
        match self.thread.take() {
            Some(thread) => thread
                .join()
                .map_err(|_| NodeStartError::Runtime("node thread panicked"))?,
            None => Ok(()),
        }
    }
}

impl Drop for RunningNode {
    fn drop(&mut self) {
        // 兜底：即使调用方忘了 shutdown 也不会泄漏监听端口（不强等，避免在信号处理里阻塞）。
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(());
        }
    }
}

/// API-010：先持久层、后监听，返回已就绪的节点句柄。
pub fn spawn_node(config: NodeConfig) -> Result<RunningNode, NodeStartError> {
    // 1. 数据目录必须可创建（生产部署应挂持久卷）。
    fs::create_dir_all(&config.data_dir)
        .map_err(|_| NodeStartError::Io("data directory could not be created"))?;
    let database_path = config.data_dir.join("node.db");

    // 2. 种子与公钥：在打开数据库之前校验固定公钥，避免把错误身份写进库。
    let mut seed = load_signing_seed(&config.seed_file, config.init_seed)?;
    let public_key = ed25519_dalek::SigningKey::from_bytes(&seed)
        .verifying_key()
        .to_bytes();
    if let Some(expected) = config.expect_public_key {
        if expected != public_key {
            seed.fill(0);
            return Err(NodeStartError::Config(
                "seed file does not match --expect-public-key (refusing to start with another identity)",
            ));
        }
    }
    let node_id = config.node_id.clone().unwrap_or_else(|| {
        let hex = chat_node_services::canonical::hex_encode(&public_key);
        format!("node-{}", &hex[..16])
    });

    // 3. 打开服务：迁移、节点身份固定、策略校验都在这里完成。
    let policy = config.rate_limit_policy()?;
    let service_config = NodeServiceConfig {
        node_id: node_id.clone(),
        signing_seed: seed,
        policy,
    };
    // NODE-018：建库之前先做完整配置校验，非法配置不留下任何文件。
    chat_node_services::config_validation::NodeConfigValidation::validate(&service_config)?;
    let mut service = NodeService::open(&database_path, service_config)?;
    // 审计开关：默认开；--no-audit 关闭（放宽撤销策略下关闭会被服务层拒绝）。
    service.set_audit_enabled(config.audit)?;
    // 种子已经进入服务内部，清零本地副本。
    seed.fill(0);

    // 4. 先绑定监听（同步），失败时立即返回，绝不留下“看似已启动”的进程。
    let listener = std::net::TcpListener::bind(config.bind)
        .map_err(|error| NodeStartError::Bind(error.to_string()))?;
    listener
        .set_nonblocking(true)
        .map_err(|_| NodeStartError::Bind("listener could not be set non-blocking".to_owned()))?;
    let address = listener
        .local_addr()
        .map_err(|_| NodeStartError::Bind("listener has no local address".to_owned()))?;

    // TLS 配置在起线程之前装载：证书/私钥有问题要立刻失败，不能"启动了但握手全挂"。
    let tls_acceptor = match config.tls.as_ref() {
        Some(tls) => Some(load_tls_acceptor(tls)?),
        None => None,
    };
    let state = AppState::with_system_clock(service);
    let router = build_router(state);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let thread = thread::Builder::new()
        .name("chat-node-serve".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| NodeStartError::Runtime("tokio runtime could not be built"))?;
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener)
                    .map_err(|_| NodeStartError::Bind("listener could not be registered".to_owned()))?;
                match tls_acceptor {
                    None => axum::serve(listener, router)
                        .with_graceful_shutdown(async move {
                            let _ = shutdown_rx.await;
                        })
                        .await
                        .map_err(|_| NodeStartError::Runtime("http server stopped unexpectedly")),
                    Some(acceptor) => {
                        serve_tls(listener, router, acceptor, shutdown_rx).await
                    }
                }
            })
        })
        .map_err(|_| NodeStartError::Runtime("node thread could not be started"))?;

    Ok(RunningNode {
        address,
        node_id,
        node_public_key: public_key,
        database_path,
        require_capability: config.require_capability,
        shutdown: Some(shutdown_tx),
        thread: Some(thread),
    })
}

impl NodeConfig {
    /// 由配置构造节点策略：限流参数可用环境变量收紧/放宽；能力令牌强制走显式开关。
    fn rate_limit_policy(&self) -> Result<NodeAuthPolicy, NodeStartError> {
        let mut policy = NodeAuthPolicy::default();
        // CAP 包：强制开关来自 CLI/环境变量，令牌有效期上限可选收紧。
        policy.capability.required = self.require_capability;
        if let Ok(ttl) = env::var(ENV_CAPABILITY_MAX_TTL) {
            let ttl = ttl.trim().parse::<u64>().map_err(|_| {
                NodeStartError::Config(
                    "CHAT_NODE_CAPABILITY_MAX_TTL_SECONDS must be a positive integer",
                )
            })?;
            policy.capability.max_ttl_seconds = ttl;
        }
        if let Ok(window) = env::var("CHAT_NODE_RATE_LIMIT_WINDOW_SECONDS") {
            if let Ok(window) = window.trim().parse::<u64>() {
                policy.rate_limit.window_seconds = window;
            }
        }
        if let Ok(requests) = env::var("CHAT_NODE_RATE_LIMIT_REQUESTS_PER_DEVICE") {
            if let Ok(requests) = requests.trim().parse::<u32>() {
                policy.rate_limit.max_requests_per_device = requests;
                // 账户预算是多设备上限：至少是单设备预算的两倍（不足时抬高，保证策略自洽）。
                policy.rate_limit.max_requests_per_account =
                    requests.saturating_mul(2).max(policy.rate_limit.max_requests_per_account);
            }
        }
        Ok(policy)
    }
}

/// 生产入口（API-010）：起点即校验配置，失败时不监听、不建库，成功后阻塞到 Ctrl+C。
pub fn run_from_env() -> Result<(), NodeStartError> {
    let config = NodeConfig::from_process()?;
    let running = spawn_node(config)?;
    // 启动日志只输出节点 ID、监听地址与库路径（没有密钥、没有配置全文）。
    // 启动日志同时说明能力令牌模式（运维需要一眼看出端点是否强制令牌）。
    println!(
        "chat-node started node_id={} address={} database={} capability={}",
        running.node_id,
        running.address,
        running.database_path.display(),
        if running.require_capability { "required" } else { "off" }
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| NodeStartError::Runtime("signal runtime could not be built"))?;
    runtime.block_on(async {
        tokio::signal::ctrl_c()
            .await
            .map_err(|_| NodeStartError::Runtime("ctrl-c handler could not be installed"))
    })?;
    println!("chat-node stopping: no new requests accepted");
    running.shutdown()?;
    println!("chat-node stopped");
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn args(values: &[&'static str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn no_env(_name: &str) -> Option<String> {
        None
    }

    /// 只让给定变量返回固定值，其余为缺失。
    fn env_with(name: &'static str, value: &'static str) -> impl Fn(&str) -> Option<String> {
        move |requested: &str| {
            if requested == name {
                Some(value.to_owned())
            } else {
                None
            }
        }
    }

    #[test]
    fn config_requires_a_seed_file_and_valid_bind() {
        let error = NodeConfig::from_args_and_env(args(&[]), no_env)
            .expect_err("seed file is mandatory");
        assert!(matches!(error, NodeStartError::Config(_)));

        let error = NodeConfig::from_args_and_env(
            args(&["--bind", "not-an-address", "--seed-file", "seed.bin"]),
            no_env,
        )
        .expect_err("bind must parse");
        assert!(matches!(error, NodeStartError::Config(_)));

        let error = NodeConfig::from_args_and_env(args(&["--unknown"]), no_env)
            .expect_err("unknown flags are rejected");
        assert!(matches!(error, NodeStartError::Config(_)));

        let config = NodeConfig::from_args_and_env(args(&["--seed-file", "seed.bin"]), no_env)
            .expect("minimal config");
        assert_eq!(config.bind.to_string(), DEFAULT_BIND);
        assert_eq!(config.data_dir, PathBuf::from(DEFAULT_DATA_DIR));
        assert!(config.node_id.is_none());
        assert!(config.expect_public_key.is_none());
    }

    #[test]
    fn capability_flag_comes_from_cli_or_env_and_bad_values_are_rejected() {
        let env_with = |name: &'static str, value: &'static str| {
            move |key: &str| {
                if key == name {
                    Some(value.to_owned())
                } else {
                    None
                }
            }
        };
        // 默认关闭（旧客户端零改动）。
        let config = NodeConfig::from_args_and_env(args(&["--seed-file", "seed.bin"]), no_env)
            .expect("minimal config");
        assert!(!config.require_capability);
        // CLI 开关。
        let config = NodeConfig::from_args_and_env(
            args(&["--seed-file", "seed.bin", "--require-capability"]),
            no_env,
        )
        .expect("cli flag");
        assert!(config.require_capability);
        // 环境变量（多种真值写法）。
        for value in ["1", "true", "YES", "on"] {
            let config = NodeConfig::from_args_and_env(
                args(&["--seed-file", "seed.bin"]),
                env_with(ENV_REQUIRE_CAPABILITY, value),
            )
            .expect("env true");
            assert!(config.require_capability, "value {value} must enable enforcement");
        }
        for value in ["0", "false", "no", "off", ""] {
            let config = NodeConfig::from_args_and_env(
                args(&["--seed-file", "seed.bin"]),
                env_with(ENV_REQUIRE_CAPABILITY, value),
            )
            .expect("env false");
            assert!(!config.require_capability, "value {value} must disable enforcement");
        }
        // 拼写错误/未知取值 → 显式启动失败（不是“当成关闭”）。
        let error = NodeConfig::from_args_and_env(
            args(&["--seed-file", "seed.bin"]),
            env_with(ENV_REQUIRE_CAPABILITY, "maybe"),
        )
        .expect_err("bad flag value must fail startup");
        assert!(matches!(error, NodeStartError::Config(_)));
        assert_eq!(parse_flag("On"), Some(true));
        assert_eq!(parse_flag("OFF"), Some(false));
        assert_eq!(parse_flag("2"), None);
    }

    /// NODE-018：审计默认开启，只有显式 `--no-audit` / `CHAT_NODE_AUDIT=0` 才关闭。
    #[test]
    fn audit_is_enabled_by_default_and_can_be_turned_off_explicitly() {
        let no_env = |_: &str| None;
        let config = NodeConfig::from_args_and_env(args(&["--seed-file", "seed.bin"]), no_env)
            .expect("minimal config");
        assert!(config.audit, "audit is on by default");
        let config = NodeConfig::from_args_and_env(
            args(&["--seed-file", "seed.bin", "--no-audit"]),
            no_env,
        )
        .expect("--no-audit");
        assert!(!config.audit);
        let config = NodeConfig::from_args_and_env(
            args(&["--seed-file", "seed.bin"]),
            env_with(ENV_AUDIT, "0"),
        )
        .expect("env off");
        assert!(!config.audit);
        let config = NodeConfig::from_args_and_env(
            args(&["--seed-file", "seed.bin"]),
            env_with(ENV_AUDIT, "1"),
        )
        .expect("env on");
        assert!(config.audit);
        // 拼写错误不当成“关闭”。
        let error = NodeConfig::from_args_and_env(
            args(&["--seed-file", "seed.bin"]),
            env_with(ENV_AUDIT, "maybe"),
        )
        .expect_err("bad audit flag value must fail startup");
        assert!(matches!(error, NodeStartError::Config(_)));
    }

    #[test]
    fn public_bind_requires_an_explicit_opt_in() {
        let error = NodeConfig::from_args_and_env(
            args(&["--bind", "0.0.0.0:8080", "--seed-file", "seed.bin"]),
            no_env,
        )
        .expect_err("public bind must be explicit");
        assert!(matches!(error, NodeStartError::Config(_)));
        let config = NodeConfig::from_args_and_env(
            args(&[
                "--bind",
                "0.0.0.0:8080",
                "--seed-file",
                "seed.bin",
                "--allow-public-bind",
            ]),
            no_env,
        )
        .expect("explicit opt-in");
        assert!(config.allow_public_bind);
    }

    #[test]
    fn environment_is_used_and_command_line_wins() {
        let env = |name: &str| match name {
            "CHAT_NODE_BIND" => Some("127.0.0.1:9100".to_owned()),
            "CHAT_NODE_SEED_FILE" => Some("from-env.bin".to_owned()),
            "CHAT_NODE_ID" => Some("node-env".to_owned()),
            _ => None,
        };
        let config = NodeConfig::from_args_and_env(args(&[]), env).expect("env config");
        assert_eq!(config.bind.to_string(), "127.0.0.1:9100");
        assert_eq!(config.seed_file, PathBuf::from("from-env.bin"));
        assert_eq!(config.node_id.as_deref(), Some("node-env"));
        let config = NodeConfig::from_args_and_env(
            args(&["--node-id", "node-cli", "--seed-file", "cli.bin"]),
            env,
        )
        .expect("cli overrides env");
        assert_eq!(config.node_id.as_deref(), Some("node-cli"));
        assert_eq!(config.seed_file, PathBuf::from("cli.bin"));
    }

    #[test]
    fn expected_public_key_must_be_canonical_hex() {
        let error = NodeConfig::from_args_and_env(
            args(&["--seed-file", "seed.bin", "--expect-public-key", "zz"]),
            no_env,
        )
        .expect_err("bad key rejected");
        assert!(matches!(error, NodeStartError::Config(_)));
        let key: &'static str = Box::leak("ab".repeat(32).into_boxed_str());
        let config = NodeConfig::from_args_and_env(
            args(&["--seed-file", "seed.bin", "--expect-public-key", key]),
            no_env,
        )
        .expect("canonical key accepted");
        assert_eq!(config.expect_public_key, Some([0xab_u8; 32]));
    }

    #[test]
    fn seed_file_accepts_raw_bytes_and_hex() {
        let dir = std::env::temp_dir().join(format!("chat-node-seed-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        let raw = dir.join("raw.bin");
        let mut file = fs::File::create(&raw).expect("raw seed");
        file.write_all(&[7_u8; 32]).expect("write raw");
        drop(file);
        assert_eq!(load_signing_seed(&raw, false).expect("raw seed"), [7_u8; 32]);
        let hex = dir.join("hex.txt");
        fs::write(&hex, format!("{}\n", "0a".repeat(32))).expect("hex seed");
        assert_eq!(load_signing_seed(&hex, false).expect("hex seed"), [0x0a_u8; 32]);
        let broken = dir.join("broken.bin");
        fs::write(&broken, [1_u8; 16]).expect("short seed");
        assert!(matches!(
            load_signing_seed(&broken, false),
            Err(NodeStartError::Config(_))
        ));
        let missing = dir.join("missing.bin");
        assert!(matches!(
            load_signing_seed(&missing, false),
            Err(NodeStartError::Config(_))
        ));
        // 显式 --init-seed 才创建。
        let created = load_signing_seed(&missing, true).expect("created seed");
        assert_eq!(fs::read(&missing).expect("seed readable"), created);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn node_id_is_derived_from_the_public_key_when_absent() {
        let dir = std::env::temp_dir().join(format!("chat-node-start-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        let seed_file = dir.join("seed.bin");
        fs::write(&seed_file, [3_u8; 32]).expect("seed");
        let config = NodeConfig {
            bind: "127.0.0.1:0".parse().expect("addr"),
            data_dir: dir.join("data"),
            seed_file,
            node_id: None,
            allow_public_bind: false,
            init_seed: false,
            expect_public_key: None,
            require_capability: false,
            audit: true,
            tls: None,
        };
        let running = spawn_node(config.clone()).expect("node starts");
        assert!(running.node_id.starts_with("node-"));
        assert_eq!(running.node_id.len(), "node-".len() + 16);
        assert!(running.database_path.exists());
        running.shutdown().expect("clean shutdown");

        // 固定公钥不一致时必须拒绝启动（绝不把错误身份写进库）。
        let wrong = NodeConfig {
            expect_public_key: Some([9_u8; 32]),
            node_id: Some("node-pinned".into()),
            data_dir: dir.join("data2"),
            ..config
        };
        let error = spawn_node(wrong).expect_err("mismatched pinned key");
        assert!(matches!(error, NodeStartError::Config(_)));
        assert!(!dir.join("data2").join("node.db").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}

/// 从 PEM 文件装载 TLS acceptor。
///
/// 失败一律**启动即报错**（缺文件、空文件、无证书、无可用私钥、配置异常），
/// 因为"TLS 起不来却照常开明文端口"是最危险的行为。
fn load_tls_acceptor(tls: &TlsConfig) -> Result<tokio_rustls::TlsAcceptor, NodeStartError> {
    let cert_pem = fs::read(&tls.cert_file)
        .map_err(|_| NodeStartError::Config("TLS certificate file could not be read"))?;
    let key_pem = fs::read(&tls.key_file)
        .map_err(|_| NodeStartError::Config("TLS private key file could not be read"))?;
    let certs = parse_pem_certificates(&cert_pem)
        .ok_or(NodeStartError::Config("TLS certificate PEM contains no certificate"))?;

    // 证书有效期：启动就算一次并打出来（到期前 30 天显式告警）。
    if let Some(first) = certs.first() {
        match chat_node_services::cert_validity::parse_der(first.as_ref()) {
            Some(validity) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_secs() as i64)
                    .unwrap_or(0);
                let days = validity.days_remaining(now);
                if validity.is_expired(now) {
                    eprintln!(
                        "chat-node tls certificate EXPIRED {} days ago; clients that pin it will fail",
                        -days
                    );
                } else if days < 30 {
                    eprintln!("chat-node WARN tls certificate expires in {days} days (rotate soon)");
                }
                println!("chat-node tls certificate expires in {days} days");
            }
            None => {
                eprintln!("chat-node WARN tls certificate validity could not be parsed (unknown expiry)");
            }
        }
    }
    let key = parse_pem_private_key(&key_pem)
        .ok_or(NodeStartError::Config("TLS private key PEM is not supported (PKCS#8 or RSA expected)"))?;
    // rustls 需要显式安装 CryptoProvider（本项目统一用 ring）。
    if rustls::crypto::ring::default_provider()
        .install_default()
        .is_err()
    {
        // 已安装过不是错误：说明本进程其它组件先装了同一个 provider。
    }
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|_| NodeStartError::Config("TLS certificate and private key do not match"))?;
    Ok(tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(config)))
}

/// 极简 PEM 解析：只取第一个 `-----BEGIN CERTIFICATE-----` 块（支持多张证书的链）。
fn parse_pem_certificates(pem: &[u8]) -> Option<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let text = std::str::from_utf8(pem).ok()?;
    let mut certs = Vec::new();
    let mut rest = text;
    while let Some(begin) = rest.find("-----BEGIN CERTIFICATE-----") {
        let after = &rest[begin + "-----BEGIN CERTIFICATE-----".len()..];
        let end = after.find("-----END CERTIFICATE-----")?;
        let der = decode_base64(&after[..end])?;
        certs.push(rustls::pki_types::CertificateDer::from(der));
        rest = &after[end..];
    }
    if certs.is_empty() { None } else { Some(certs) }
}

/// 极简 PEM 解析：`PRIVATE KEY`（PKCS#8）或 `RSA PRIVATE KEY`。
fn parse_pem_private_key(pem: &[u8]) -> Option<rustls::pki_types::PrivateKeyDer<'static>> {
    let text = std::str::from_utf8(pem).ok()?;
    if let Some(der) = pem_block(text, "PRIVATE KEY") {
        return Some(rustls::pki_types::PrivateKeyDer::Pkcs8(der.into()));
    }
    pem_block(text, "RSA PRIVATE KEY").map(|der| rustls::pki_types::PrivateKeyDer::Pkcs1(der.into()))
}

fn pem_block(text: &str, label: &str) -> Option<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let start = text.find(&begin)? + begin.len();
    let stop = text[start..].find(&end)? + start;
    decode_base64(&text[start..stop])
}

/// 宽松的 base64 解码（忽略空白；遇到非法字符返回 None）。
fn decode_base64(input: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buffer: u32 = 0;
    let mut bits = 0u32;
    for byte in input.bytes() {
        if byte == b'=' {
            break;
        }
        if byte.is_ascii_whitespace() {
            continue;
        }
        let index = TABLE.iter().position(|candidate| *candidate == byte)?;
        buffer = (buffer << 6) | index as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

/// TLS 监听循环：每条连接独立握手，握手失败只影响该连接（不拖垮节点）。
async fn serve_tls(
    listener: tokio::net::TcpListener,
    router: axum::Router,
    acceptor: tokio_rustls::TlsAcceptor,
    shutdown: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), NodeStartError> {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::server::conn::auto::Builder as AutoBuilder;
    use tower::ServiceExt;

    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            accepted = listener.accept() => {
                let Ok((stream, _peer)) = accepted else { continue };
                let acceptor = acceptor.clone();
                let service = router.clone();
                tokio::spawn(async move {
                    let Ok(tls_stream) = acceptor.accept(stream).await else { return };
                    let io = TokioIo::new(tls_stream);
                    let hyper_service = hyper::service::service_fn(move |request| {
                        service.clone().oneshot(request)
                    });
                    let builder = AutoBuilder::new(TokioExecutor::new());
                    let _ = builder.serve_connection(io, hyper_service).await;
                });
            }
        }
    }
}
