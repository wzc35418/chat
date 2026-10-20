//! 传输安全（https + 固定证书）真握手测试：用**自签证书**起一个本地 TLS 服务端。
//!
//! 断言：
//! 1) 给了对应证书 -> TLS 握手成功、HTTP 请求成功；
//! 2) https 但没给信任 -> 配置期显式拒绝（`TLS_TRUST_REQUIRED`），**不静默降级**；
//! 3) http 却给了固定证书 -> 配置期显式拒绝（`INSECURE_TRANSPORT`）；
//! 4) 给的是**另一张**自签证书 -> 握手失败（客户端不会接受未知证书）。
//!
//! 诚实边界：本实现是"加信任锚"，系统根仍启用（reqwest 0.13 该 API 未暴露关闭开关），
//! 因此**不是严格 pinning**；严格 pinning 需要 rustls 自定义验证器（后续）。

use std::sync::Arc;

use chat_node_client::transport_security::TransportSecurity;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CERT_PEM: &[u8] = include_bytes!("fixtures/tls/test-cert.pem");
const KEY_PEM: &[u8] = include_bytes!("fixtures/tls/test-key.pem");
/// 另一张自签证书：用于"客户端不认识这张证书"的负例。
const OTHER_CERT_PEM: &[u8] = include_bytes!("fixtures/tls/test-other-cert.pem");

fn der_from_pem(pem: &[u8], label: &str) -> Vec<u8> {
    let text = std::str::from_utf8(pem).expect("pem is utf8");
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let start = text.find(&begin).expect("begin marker") + begin.len();
    let stop = text.find(&end).expect("end marker");
    let body: String = text[start..stop].chars().filter(|c| !c.is_whitespace()).collect();
    base64_decode(&body)
}

fn base64_decode(input: &str) -> Vec<u8> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buffer: u32 = 0;
    let mut bits = 0u32;
    for byte in input.bytes() {
        if byte == b'=' {
            break;
        }
        let Some(index) = TABLE.iter().position(|candidate| *candidate == byte) else {
            continue;
        };
        buffer = (buffer << 6) | index as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    out
}

/// 起一个只回一句 HTTP 200 的 TLS 服务端，返回 (地址, 关闭句柄)。
async fn spawn_tls_server() -> (String, tokio::task::JoinHandle<()>) {
    // 测试进程里显式装 ring provider（我们的 rustls 依赖同时可能引入多个 provider）。
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certs = vec![rustls::pki_types::CertificateDer::from(der_from_pem(CERT_PEM, "CERTIFICATE"))];
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(der_from_pem(KEY_PEM, "PRIVATE KEY").into());
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .expect("server config");
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind tls server");
    let address = listener.local_addr().expect("local addr");
    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { return };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(stream).await else { return };
                let mut buffer = [0u8; 2048];
                let _ = tls.read(&mut buffer).await;
                let body = "{}";
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = tls.write_all(response.as_bytes()).await;
                let _ = tls.flush().await;
            });
        }
    });
    (format!("https://{address}"), handle)
}

fn config_error(result: Result<reqwest::Client, chat_node_client::error::NodeClientError>) -> String {
    match result {
        Ok(_) => panic!("expected configuration error"),
        Err(error) => format!("{error:?}"),
    }
}

#[tokio::test]
async fn pinned_self_signed_certificate_goes_through() {
    let (endpoint, handle) = spawn_tls_server().await;
    let trust = TransportSecurity::pinned_pem(CERT_PEM.to_vec());
    trust.validate(&endpoint).expect("endpoint and trust agree");
    let client = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(CERT_PEM).expect("pem accepted"))
        .build()
        .expect("client builds");
    let response = client
        .get(endpoint + "/v1/health")
        .send()
        .await
        .expect("TLS handshake must succeed with the pinned certificate");
    assert_eq!(response.status().as_u16(), 200);
    handle.abort();
}

#[tokio::test]
async fn https_without_trust_is_rejected_before_any_request() {
    let trust = TransportSecurity::Plain;
    let error = trust.validate("https://127.0.0.1:9").expect_err("must reject");
    assert!(format!("{error:?}").contains("TLS_TRUST_REQUIRED"), "unexpected: {error:?}");
}

#[tokio::test]
async fn pinned_certificate_with_plain_http_is_rejected() {
    let trust = TransportSecurity::pinned_pem(CERT_PEM.to_vec());
    let error = trust.validate("http://127.0.0.1:9").expect_err("must reject");
    assert!(format!("{error:?}").contains("INSECURE_TRANSPORT"), "unexpected: {error:?}");
}

#[tokio::test]
async fn unknown_certificate_fails_the_handshake() {
    let (endpoint, handle) = spawn_tls_server().await;
    // 客户端只信任**另一张**自签证书：与服务端那张不匹配，握手必须失败。
    let client = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(OTHER_CERT_PEM).expect("pem accepted"))
        .build()
        .expect("client builds");
    let result = client.get(endpoint + "/v1/health").send().await;
    assert!(result.is_err(), "unknown certificate must not be accepted");
    handle.abort();
}
