//! TRANSPORT-002 的纯实现：启动真实 iroh 端点、建立/接受连接。
//!
//! 默认只绑定回环地址（公共 relay、DNS 发现、portmapper 与 NetReport 全部关闭）；
//! 显式 `allow_lan` 后才允许绑定非回环地址，`relay_urls` 非空时启用显式中继列表
//! （https，或本机回环 http）。端点密钥可显式提供以便测试可复现；`node_id`/`endpoint_id`
//! 由 iroh 公钥导出。所有等待都有显式超时预算（1ms–30s），不会无限挂起。
//!
//! 与文档契约的关系：`start_endpoint` 对应 TRANSPORT-001 的端点启动子集；
//! 连接/接受对应 TRANSPORT-003 `connect_peer` 的连接建立子集。真正的直连/穿透/中继
//! 路径与发现记录验证仍未实现。

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use iroh::{
    Endpoint, EndpointAddr, RelayMap, RelayMode, RelayUrl, SecretKey,
    endpoint::{Connection, NetReportConfig, PortmapperConfig, presets},
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::error::{Result, TransportError};
use crate::session::EndpointId;
use crate::wire_io::WireStream;

/// 默认应用层 ALPN。
pub const TRANSPORT_ALPN: &[u8] = b"chat/p1/transport/1";
/// 单次 IO/握手等待的硬上限（30 秒）。
pub const MAX_IO_TIMEOUT_MS: u64 = 30_000;
/// 默认连接/接受超时（5 秒）。
pub const DEFAULT_IO_TIMEOUT_MS: u64 = 5_000;
/// 接受循环排队上限。
pub const MAX_ACCEPT_QUEUE: usize = 256;
/// 显式中继数量上限。
pub const MAX_RELAY_URLS: usize = 8;

/// 端点启动配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointConfig {
    /// 绑定地址；默认回环，非回环必须显式 `allow_lan`。
    pub bind_addr: SocketAddr,
    /// 应用层 ALPN（1–255 字节）。
    pub alpn: Vec<u8>,
    /// 可选的 32 字节端点密钥；`None` 时由 iroh 安全随机生成。
    pub secret_key: Option<[u8; 32]>,
    /// 是否允许绑定非回环地址（默认 `false`）。
    pub allow_lan: bool,
    /// 显式中继 URL 列表；空表示禁用中继。https 任意主机，http 仅本机回环。
    pub relay_urls: Vec<String>,
}

impl EndpointConfig {
    /// 回环默认配置（随机端口、随机密钥、默认 ALPN、禁用中继）。
    pub fn loopback() -> Self {
        Self {
            bind_addr: SocketAddr::new(IpAddr::from([127, 0, 0, 1]), 0),
            alpn: TRANSPORT_ALPN.to_vec(),
            secret_key: None,
            allow_lan: false,
            relay_urls: Vec::new(),
        }
    }

    /// 指定固定密钥（测试/可复现节点身份）。
    pub fn with_secret_key(mut self, secret_key: [u8; 32]) -> Self {
        self.secret_key = Some(secret_key);
        self
    }

    /// 显式允许非回环绑定。
    pub fn with_allow_lan(mut self, allow_lan: bool) -> Self {
        self.allow_lan = allow_lan;
        self
    }

    /// 设置显式中继列表（空 = 禁用）。
    pub fn with_relay_urls(mut self, relay_urls: Vec<String>) -> Self {
        self.relay_urls = relay_urls;
        self
    }

    /// 有界校验：非回环需 `allow_lan`、ALPN 合法、中继列表有界且协议受限。
    pub fn validate(&self) -> Result<()> {
        if !self.bind_addr.ip().is_loopback() && !self.allow_lan {
            return Err(TransportError::InvalidConfig {
                detail: "non-loopback bind requires allowLan",
            });
        }
        if self.alpn.is_empty() || self.alpn.len() > 255 {
            return Err(TransportError::InvalidConfig {
                detail: "alpn must be within 1..=255 bytes",
            });
        }
        self.normalized_relay_urls()?;
        Ok(())
    }

    /// 校验并规范化中继 URL（去重保序；不接受凭据/片段；http 仅回环）。
    pub fn normalized_relay_urls(&self) -> Result<Vec<String>> {
        if self.relay_urls.len() > MAX_RELAY_URLS {
            return Err(TransportError::InvalidConfig {
                detail: "relay url count exceeds maxRelayUrls",
            });
        }
        let mut normalized: Vec<String> = Vec::with_capacity(self.relay_urls.len());
        for text in &self.relay_urls {
            let parsed: RelayUrl = text.parse().map_err(|_| TransportError::InvalidConfig {
                detail: "relay url is not a valid URL",
            })?;
            match parsed.scheme() {
                "https" => {}
                "http" => {
                    let host = parsed.host_str().unwrap_or_default();
                    if !is_loopback_host(host) {
                        return Err(TransportError::InvalidConfig {
                            detail: "plain http relays must be loopback",
                        });
                    }
                }
                _ => {
                    return Err(TransportError::InvalidConfig {
                        detail: "relay url scheme must be https or loopback http",
                    });
                }
            }
            if !parsed.username().is_empty() || parsed.password().is_some() {
                return Err(TransportError::InvalidConfig {
                    detail: "relay url must not contain credentials",
                });
            }
            if parsed.fragment().is_some() {
                return Err(TransportError::InvalidConfig {
                    detail: "relay url must not contain a fragment",
                });
            }
            let normalized_url = parsed.to_string();
            if !normalized.contains(&normalized_url) {
                normalized.push(normalized_url);
            }
        }
        Ok(normalized)
    }
}

fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    // URL 的 IPv6 host 可能带方括号。
    let host = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host);
    host.parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// 校验 IO 超时预算（1ms–30s）。
pub fn validate_timeout(timeout: Duration) -> Result<()> {
    let millis = timeout.as_millis();
    if timeout.is_zero() || millis > u128::from(MAX_IO_TIMEOUT_MS) {
        return Err(TransportError::InvalidConfig {
            detail: "io timeout must be within 1ms..=30000ms",
        });
    }
    Ok(())
}

fn to_transport_id(id: iroh::EndpointId) -> EndpointId {
    EndpointId::from_bytes(*id.as_bytes())
}

/// 生效配置快照（只读，不含密钥材料）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointSnapshot {
    /// 节点 ID（iroh 公钥）。
    pub node_id: EndpointId,
    /// 请求的绑定地址。
    pub bind_addr: SocketAddr,
    /// 实际绑定 socket。
    pub local_addr: SocketAddr,
    /// 是否允许非回环绑定。
    pub allow_lan: bool,
    /// 应用层 ALPN。
    pub alpn: Vec<u8>,
    /// 生效的显式中继 URL（规范化、去重保序）。
    pub relay_urls: Vec<String>,
    /// 是否启用中继。
    pub relay_enabled: bool,
    /// 是否启用地址发现（本切片恒为 `false`）。
    pub discovery_enabled: bool,
    /// 是否配置了固定端点密钥（不暴露密钥本身）。
    pub secret_key_configured: bool,
}

/// 已启动的 iroh 端点。
#[derive(Clone)]
pub struct TransportEndpoint {
    endpoint: Endpoint,
    snapshot: Arc<EndpointSnapshot>,
}

impl TransportEndpoint {
    /// TRANSPORT-002：启动真实 iroh 端点（默认回环；显式 allow_lan/relay）。
    pub async fn start(config: EndpointConfig) -> Result<Self> {
        config.validate()?;
        let relay_urls = config.normalized_relay_urls()?;
        let relay_enabled = !relay_urls.is_empty();
        let relay_mode = if relay_enabled {
            let map =
                RelayMap::try_from_iter(relay_urls.iter().map(String::as_str)).map_err(|_| {
                    TransportError::InvalidConfig {
                        detail: "relay url is not a valid URL",
                    }
                })?;
            RelayMode::Custom(map)
        } else {
            RelayMode::Disabled
        };
        let mut builder = Endpoint::builder(presets::Minimal)
            .clear_ip_transports()
            .bind_addr(config.bind_addr)
            .map_err(|_| TransportError::EndpointStartFailed {
                detail: "invalid bind address",
            })?
            .relay_mode(relay_mode)
            .portmapper_config(PortmapperConfig::Disabled)
            .net_report_config(NetReportConfig::minimal())
            .clear_address_lookup()
            .alpns(vec![config.alpn.clone()]);
        if let Some(seed) = config.secret_key {
            builder = builder.secret_key(SecretKey::from_bytes(&seed));
        }
        let endpoint = builder
            .bind()
            .await
            .map_err(|_| TransportError::EndpointStartFailed {
                detail: "iroh endpoint bind failed",
            })?;
        let local_addr = match endpoint.bound_sockets().as_slice() {
            [socket] => *socket,
            _ => {
                endpoint.close().await;
                return Err(TransportError::EndpointStartFailed {
                    detail: "expected exactly one bound socket",
                });
            }
        };
        let snapshot = EndpointSnapshot {
            node_id: to_transport_id(endpoint.id()),
            bind_addr: config.bind_addr,
            local_addr,
            allow_lan: config.allow_lan,
            alpn: config.alpn,
            relay_urls,
            relay_enabled,
            discovery_enabled: false,
            secret_key_configured: config.secret_key.is_some(),
        };
        Ok(Self {
            endpoint,
            snapshot: Arc::new(snapshot),
        })
    }

    /// 节点 ID（iroh 公钥）。
    pub fn node_id(&self) -> EndpointId {
        to_transport_id(self.endpoint.id())
    }

    /// 端点 ID（与节点 ID 相同；保留文档中的两个称呼）。
    pub fn endpoint_id(&self) -> EndpointId {
        self.node_id()
    }

    /// 应用层 ALPN。
    pub fn alpn(&self) -> &[u8] {
        &self.snapshot.alpn
    }

    /// 生效配置快照（只读，不含密钥）。
    pub fn snapshot(&self) -> &EndpointSnapshot {
        &self.snapshot
    }

    /// 实际绑定地址。
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.snapshot.local_addr)
    }

    /// 可用于对端 dial 的地址（端点 ID + 已绑定 socket）。
    ///
    /// 通配绑定（如 0.0.0.0）无法直接 dial，需调用方替换为具体地址。
    pub fn address(&self) -> Result<EndpointAddr> {
        if self.snapshot.local_addr.ip().is_unspecified() {
            return Err(TransportError::EndpointStartFailed {
                detail: "cannot dial an unspecified bind address",
            });
        }
        Ok(EndpointAddr::new(self.endpoint.id()).with_ip_addr(self.snapshot.local_addr))
    }

    /// 连接到对端；超时/不可达返回 `PEER_UNREACHABLE`，绝不挂起。
    pub async fn connect(&self, peer: EndpointAddr, timeout: Duration) -> Result<PeerConnection> {
        validate_timeout(timeout)?;
        let expected = to_transport_id(peer.id);
        let connecting = self.endpoint.connect(peer, &self.snapshot.alpn);
        let connection = match tokio::time::timeout(timeout, connecting).await {
            Err(_) => {
                return Err(TransportError::PeerUnreachable {
                    detail: "connect timed out",
                });
            }
            Ok(Err(_)) => {
                return Err(TransportError::PeerUnreachable {
                    detail: "connect failed",
                });
            }
            Ok(Ok(connection)) => connection,
        };
        if to_transport_id(connection.remote_id()) != expected {
            connection.close(0u32.into(), b"endpoint mismatch");
            return Err(TransportError::SessionMismatch {
                detail: "connected endpoint does not match the requested peer",
            });
        }
        Ok(PeerConnection::new(connection))
    }

    /// 接受一个入站连接；超时返回 `TRANSPORT_TIMEOUT`。
    pub async fn accept(&self, timeout: Duration) -> Result<PeerConnection> {
        validate_timeout(timeout)?;
        let incoming = match tokio::time::timeout(timeout, self.endpoint.accept()).await {
            Err(_) => {
                return Err(TransportError::TransportTimeout {
                    detail: "accept timed out",
                });
            }
            Ok(None) => {
                return Err(TransportError::ConnectionClosed {
                    detail: "endpoint closed",
                });
            }
            Ok(Some(incoming)) => incoming,
        };
        let connection = incoming
            .await
            .map_err(|_| TransportError::PeerUnreachable {
                detail: "incoming handshake failed",
            })?;
        Ok(PeerConnection::new(connection))
    }

    /// 启动接受循环（有界队列），返回句柄；句柄 drop 时取消任务。
    pub fn accept_loop(&self, queue: usize) -> Result<AcceptLoopHandle> {
        if queue == 0 || queue > MAX_ACCEPT_QUEUE {
            return Err(TransportError::InvalidConfig {
                detail: "accept queue must be within 1..=256",
            });
        }
        let endpoint = self.endpoint.clone();
        let (tx, rx) = mpsc::channel(queue);
        let task = tokio::spawn(async move {
            loop {
                let Some(incoming) = endpoint.accept().await else {
                    break;
                };
                match incoming.await {
                    Ok(connection) => {
                        if tx.send(PeerConnection::new(connection)).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => continue,
                }
            }
        });
        Ok(AcceptLoopHandle { rx, task })
    }

    /// 关闭端点（可重复调用）。
    pub async fn close(&self) {
        self.endpoint.close().await;
    }
}

impl core::fmt::Debug for TransportEndpoint {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "TransportEndpoint(node={}, bind={}, lan={}, relays={}, alpn={}B)",
            self.node_id(),
            self.snapshot.local_addr,
            self.snapshot.allow_lan,
            self.snapshot.relay_urls.len(),
            self.snapshot.alpn.len()
        )
    }
}

/// TRANSPORT-002：启动端点（自由函数形式，等价于 [`TransportEndpoint::start`]）。
pub async fn start_endpoint(config: EndpointConfig) -> Result<TransportEndpoint> {
    TransportEndpoint::start(config).await
}

/// 接受循环句柄。
pub struct AcceptLoopHandle {
    rx: mpsc::Receiver<PeerConnection>,
    task: JoinHandle<()>,
}

impl AcceptLoopHandle {
    /// 等待下一个入站连接；超时 `TRANSPORT_TIMEOUT`，循环结束 `CONNECTION_CLOSED`。
    pub async fn next(&mut self, timeout: Duration) -> Result<PeerConnection> {
        validate_timeout(timeout)?;
        match tokio::time::timeout(timeout, self.rx.recv()).await {
            Err(_) => Err(TransportError::TransportTimeout {
                detail: "accept loop timed out",
            }),
            Ok(Some(connection)) => Ok(connection),
            Ok(None) => Err(TransportError::ConnectionClosed {
                detail: "accept loop stopped",
            }),
        }
    }

    /// 取消接受循环。
    pub fn abort(&self) {
        self.task.abort();
    }
}

impl core::fmt::Debug for AcceptLoopHandle {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("AcceptLoopHandle(..)")
    }
}

impl Drop for AcceptLoopHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// 一条已建立的 iroh 连接。
pub struct PeerConnection {
    connection: Connection,
}

impl core::fmt::Debug for PeerConnection {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "PeerConnection(remote={})",
            self.remote_endpoint()
        )
    }
}

impl PeerConnection {
    fn new(connection: Connection) -> Self {
        Self { connection }
    }

    /// 对端节点 ID（握手认证，不是自报字段）。
    pub fn remote_endpoint(&self) -> EndpointId {
        to_transport_id(self.connection.remote_id())
    }

    /// 打开双向流的发送端与接收端（单流内按记录顺序承载分片）。
    pub async fn open_stream(&self, timeout: Duration) -> Result<WireStream> {
        validate_timeout(timeout)?;
        let opening = self.connection.open_bi();
        match tokio::time::timeout(timeout, opening).await {
            Err(_) => Err(TransportError::TransportTimeout {
                detail: "open stream timed out",
            }),
            Ok(Err(_)) => Err(TransportError::ConnectionClosed {
                detail: "open stream failed",
            }),
            Ok(Ok((send, recv))) => Ok(WireStream::new(send, recv, self.remote_endpoint())),
        }
    }

    /// 接受对端打开的双向流。
    pub async fn accept_stream(&self, timeout: Duration) -> Result<WireStream> {
        validate_timeout(timeout)?;
        let accepting = self.connection.accept_bi();
        match tokio::time::timeout(timeout, accepting).await {
            Err(_) => Err(TransportError::TransportTimeout {
                detail: "accept stream timed out",
            }),
            Ok(Err(_)) => Err(TransportError::ConnectionClosed {
                detail: "accept stream failed",
            }),
            Ok(Ok((send, recv))) => Ok(WireStream::new(send, recv, self.remote_endpoint())),
        }
    }

    /// 主动关闭连接。
    pub fn close(&self, reason: &[u8]) {
        self.connection.close(0u32.into(), reason);
    }

    /// 等待对端/本端关闭；用于测试与上层诊断。
    pub async fn closed(&self) {
        self.connection.closed().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn default_config_is_loopback_and_valid() {
        let config = EndpointConfig::loopback();
        assert!(config.bind_addr.ip().is_loopback());
        assert_eq!(config.alpn, TRANSPORT_ALPN);
        assert!(!config.allow_lan);
        assert!(config.relay_urls.is_empty());
        config.validate().unwrap();
        assert!(config.secret_key.is_none());
        let seeded = config
            .clone()
            .with_secret_key([7u8; 32])
            .with_allow_lan(true)
            .with_relay_urls(vec!["https://relay.example.org".to_owned()]);
        assert_eq!(seeded.secret_key, Some([7u8; 32]));
        assert!(seeded.allow_lan);
        seeded.validate().unwrap();
    }

    #[test]
    fn config_rejects_non_loopback_without_allow_lan() {
        let mut config = EndpointConfig::loopback();
        config.bind_addr = SocketAddr::from((Ipv4Addr::new(192, 168, 1, 10), 0));
        assert_eq!(
            config.validate().unwrap_err().code(),
            "INVALID_TRANSPORT_CONFIG"
        );
        // 通配地址同样属于非回环。
        let mut wildcard = EndpointConfig::loopback();
        wildcard.bind_addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0));
        assert!(wildcard.validate().is_err());
        // 显式 allow_lan 后允许。
        wildcard.allow_lan = true;
        wildcard.validate().unwrap();
        let lan = EndpointConfig::loopback().with_allow_lan(true);
        assert!(lan.allow_lan);
        let mut lan_config = lan.clone();
        lan_config.bind_addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0));
        lan_config.validate().unwrap();
        let mut config = EndpointConfig::loopback();
        config.alpn.clear();
        assert!(config.validate().is_err());
        let mut config = EndpointConfig::loopback();
        config.alpn = vec![1u8; 256];
        assert!(config.validate().is_err());
        const { assert!(MAX_ACCEPT_QUEUE > 0) };
        const { assert!(MAX_RELAY_URLS >= 1) };
    }

    #[test]
    fn relay_urls_are_validated_and_normalized() {
        // https 任意主机；http 仅本机回环；规范化补尾斜杠并去重保序。
        let config = EndpointConfig::loopback().with_relay_urls(vec![
            "https://relay.example.org".to_owned(),
            "https://relay.example.org/".to_owned(),
            "http://127.0.0.1:8080".to_owned(),
            "http://localhost:8081/".to_owned(),
            "http://[::1]:8082".to_owned(),
        ]);
        config.validate().unwrap();
        let normalized = config.normalized_relay_urls().unwrap();
        assert_eq!(
            normalized,
            vec![
                "https://relay.example.org/".to_owned(),
                "http://127.0.0.1:8080/".to_owned(),
                "http://localhost:8081/".to_owned(),
                "http://[::1]:8082/".to_owned(),
            ]
        );
        // 非法：协议、非回环 http、凭据、片段、不可解析、数量超限。
        for urls in [
            vec!["ftp://relay.example.org/".to_owned()],
            vec!["http://relay.example.org/".to_owned()],
            vec!["https://user:pass@relay.example.org/".to_owned()],
            vec!["https://relay.example.org/#frag".to_owned()],
            vec!["not a url".to_owned()],
        ] {
            let config = EndpointConfig::loopback().with_relay_urls(urls);
            assert_eq!(
                config.validate().unwrap_err().code(),
                "INVALID_TRANSPORT_CONFIG"
            );
        }
        let too_many: Vec<String> = (0..=MAX_RELAY_URLS)
            .map(|index| format!("https://relay{index}.example.org/"))
            .collect();
        let config = EndpointConfig::loopback().with_relay_urls(too_many);
        assert!(config.validate().is_err());
        // 空列表 = 禁用中继，合法。
        let empty = EndpointConfig::loopback();
        assert!(empty.normalized_relay_urls().unwrap().is_empty());
    }

    #[test]
    fn io_timeouts_are_bounded() {
        assert!(validate_timeout(Duration::from_millis(1)).is_ok());
        assert!(validate_timeout(Duration::from_millis(MAX_IO_TIMEOUT_MS)).is_ok());
        assert_eq!(
            validate_timeout(Duration::ZERO).unwrap_err().code(),
            "INVALID_TRANSPORT_CONFIG"
        );
        assert!(validate_timeout(Duration::from_millis(MAX_IO_TIMEOUT_MS + 1)).is_err());
        assert!(!TRANSPORT_ALPN.is_empty());
        const { assert!(DEFAULT_IO_TIMEOUT_MS <= MAX_IO_TIMEOUT_MS) };
    }
}
