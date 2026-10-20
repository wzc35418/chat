//! 传输安全（https + 证书指纹固定 / 严格 pinning）真握手测试。
//!
//! 断言：
//! 1) 指纹匹配 -> TLS 握手成功、HTTP 200（生产代码路径 `rustls_client_config`）；
//! 2) https 但没给信任 -> 配置期显式拒绝（`TLS_TRUST_REQUIRED`）；
//! 3) http 却给了指纹 -> 配置期显式拒绝（`INSECURE_TRANSPORT`）；
//! 4) 指纹不匹配（换成另一张证书的指纹）-> 握手失败，**不是**靠系统根放过的。

use std::sync::Arc;

use chat_node_client::transport_security::TransportSecurity;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CERT_PEM: &[u8] = include_bytes!("fixtures/tls/test-cert.pem");
const KEY_PEM: &[u8] = include_bytes!("fixtures/tls/test-key.pem");
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

/// 起一个只回一句 HTTP 200 的 TLS 服务端，返回 (端点, 任务句柄)。
async fn spawn_tls_server() -> (String, tokio::task::JoinHandle<()>) {
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

fn client_for(trust: &TransportSecurity) -> reqwest::Client {
    let mut builder = reqwest::Client::builder();
    if let Some(config) = trust.rustls_client_config().expect("config builds") {
        builder = builder.use_preconfigured_tls(config);
    }
    builder.build().expect("client builds")
}

#[tokio::test]
async fn pinned_fingerprint_goes_through() {
    let (endpoint, handle) = spawn_tls_server().await;
    let trust = TransportSecurity::pinned_pem(CERT_PEM).expect("pem gives fingerprint");
    trust.validate(&endpoint).expect("endpoint and trust agree");
    let response = client_for(&trust)
        .get(endpoint + "/v1/health")
        .send()
        .await
        .expect("TLS handshake must succeed with the pinned fingerprint");
    assert_eq!(response.status().as_u16(), 200);
    handle.abort();
}

#[tokio::test]
async fn https_without_trust_is_rejected_before_any_request() {
    let error = TransportSecurity::Plain
        .validate("https://127.0.0.1:9")
        .expect_err("must reject");
    assert!(format!("{error:?}").contains("TLS_TRUST_REQUIRED"), "unexpected: {error:?}");
}

#[tokio::test]
async fn pinned_fingerprint_with_plain_http_is_rejected() {
    let trust = TransportSecurity::pinned_pem(CERT_PEM).expect("pem gives fingerprint");
    let error = trust.validate("http://127.0.0.1:9").expect_err("must reject");
    assert!(format!("{error:?}").contains("INSECURE_TRANSPORT"), "unexpected: {error:?}");
}

#[tokio::test]
async fn mismatched_fingerprint_fails_the_handshake() {
    let (endpoint, handle) = spawn_tls_server().await;
    // 用**另一张**证书的指纹：握手必须失败（不靠系统根放过）。
    let trust = TransportSecurity::pinned_pem(OTHER_CERT_PEM).expect("pem gives fingerprint");
    let result = client_for(&trust).get(endpoint + "/v1/health").send().await;
    assert!(result.is_err(), "mismatched fingerprint must fail the handshake");
    let text = format!("{:?}", result.expect_err("must fail"));
    assert!(
        text.contains("PIN_MISMATCH") || text.to_lowercase().contains("certificate"),
        "error should mention the certificate mismatch, got: {text}"
    );
    handle.abort();
}

/// 轮换窗口：客户端同时信任**新旧两张**证书，服务端换成任一张都能连上。
#[tokio::test]
async fn rotation_window_accepts_any_pinned_certificate() {
    let (endpoint, handle) = spawn_tls_server().await;
    let current = TransportSecurity::pinned_pem(CERT_PEM).expect("pem gives fingerprint");
    let next = TransportSecurity::pinned_pem(OTHER_CERT_PEM).expect("pem gives fingerprint");
    let (TransportSecurity::PinnedFingerprints(current_set), TransportSecurity::PinnedFingerprints(next_set)) =
        (current, next)
    else {
        panic!("expected pinned sets")
    };
    let mut combined = current_set.clone();
    combined.extend(next_set);
    let trust = TransportSecurity::pinned_fingerprints(combined.clone());
    trust.validate(&endpoint).expect("endpoint and trust agree");
    let response = client_for(&trust)
        .get(endpoint + "/v1/health")
        .send()
        .await
        .expect("server certificate only needs to match one pinned fingerprint");
    assert_eq!(response.status().as_u16(), 200);

    // 集合去重：重复给同一个指纹必须收敛成一个（顺序无关）。
    let duplicated = TransportSecurity::pinned_fingerprints(vec![
        combined[0].clone(),
        combined[0].clone(),
    ]);
    match duplicated {
        TransportSecurity::PinnedFingerprints(values) => {
            assert_eq!(values.len(), 1, "identical fingerprints must be deduplicated")
        }
        _ => panic!("expected pinned set"),
    }
    handle.abort();
}

/// 空集合 = 配置错误（不能既说 https 又不给任何可信任证书）。
#[tokio::test]
async fn empty_fingerprint_set_is_rejected() {
    let trust = TransportSecurity::pinned_fingerprints(Vec::<String>::new());
    let error = trust.validate("https://127.0.0.1:9").expect_err("must reject");
    assert!(format!("{error:?}").contains("TLS_CERT_INVALID"), "unexpected: {error:?}");
}
