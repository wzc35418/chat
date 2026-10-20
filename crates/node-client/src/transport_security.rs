//! 传输安全：**https + 证书指纹固定（严格 pinning）**。
//!
//! 语义：
//! - `http://` 端点必须用 [TransportSecurity::Plain]（本地/可信网络）；
//! - `https://` 端点必须给 [TransportSecurity::PinnedFingerprint]（或由 PEM 推导的指纹）；
//! - **系统根完全不参与**：自定义 verifier 只接受 SHA-256 指纹匹配的那张证书；
//! - 配了固定指纹却用 `http://`、或用 `https://` 却不给指纹 -> 显式拒绝（绝不静默降级）。

use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::error::NodeClientError;

/// 传输安全配置。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TransportSecurity {
    /// 明文 HTTP：只允许与 `http://` 端点搭配。
    #[default]
    Plain,
    /// 证书 SHA-256 指纹（64 位小写十六进制）：**只**接受这张证书。
    PinnedFingerprint(String),
}

impl TransportSecurity {
    /// 用自签证书 PEM 推导指纹（取第一张证书的 DER）。
    pub fn pinned_pem(pem: impl AsRef<[u8]>) -> Result<Self, NodeClientError> {
        let der = first_certificate_der(pem.as_ref())
            .ok_or_else(|| NodeClientError::config("TLS_CERT_INVALID: PEM 里找不到证书"))?;
        Ok(TransportSecurity::PinnedFingerprint(hex_lower(&Sha256::digest(&der))))
    }

    pub fn pinned_fingerprint(hex: impl Into<String>) -> Self {
        TransportSecurity::PinnedFingerprint(hex.into().trim().to_ascii_lowercase())
    }

    pub fn is_plain(&self) -> bool {
        matches!(self, TransportSecurity::Plain)
    }

    /// 端点与信任配置必须自洽；不自洽就报错，不猜。
    pub fn validate(&self, endpoint: &str) -> Result<(), NodeClientError> {
        let https = endpoint.starts_with("https://");
        match (https, self) {
            (true, TransportSecurity::Plain) => Err(NodeClientError::config(
                "TLS_TRUST_REQUIRED: https 端点必须提供证书指纹（系统根不参与）",
            )),
            (false, TransportSecurity::PinnedFingerprint(_)) => Err(NodeClientError::config(
                "INSECURE_TRANSPORT: 固定指纹只能配 https 端点，http 会被拒绝",
            )),
            (true, TransportSecurity::PinnedFingerprint(value)) if !is_hex_sha256(value) => Err(
                NodeClientError::config("TLS_CERT_INVALID: 指纹必须是 64 位小写十六进制"),
            ),
            _ => Ok(()),
        }
    }
}

/// 只接受指纹匹配证书的 rustls verifier（**不查系统根**）。
#[derive(Debug)]
struct PinnedVerifier {
    expected: [u8; 32],
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let actual: [u8; 32] = Sha256::digest(end_entity.as_ref()).into();
        if actual == self.expected {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "PIN_MISMATCH: server certificate fingerprint {} does not match the pinned one",
                hex_lower(&actual)
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// 按指纹固定配置构造 rustls 客户端配置（不使用系统根）。
pub fn pinned_client_config(fingerprint_hex: &str) -> Result<rustls::ClientConfig, NodeClientError> {
    let expected = decode_hex_sha256(fingerprint_hex)
        .ok_or_else(|| NodeClientError::config("TLS_CERT_INVALID: 指纹必须是 64 位小写十六进制"))?;
    // rustls 需要进程级 provider：先尝试安装；已安装过则沿用已有的。
    let _ = rustls::crypto::ring::default_provider().install_default();
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()));
    let verifier = Arc::new(PinnedVerifier {
        expected,
        provider: provider.clone(),
    });
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| NodeClientError::config("TLS_CERT_INVALID: rustls 协议版本不可用"))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth())
}

fn is_hex_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn decode_hex_sha256(value: &str) -> Option<[u8; 32]> {
    if !is_hex_sha256(value) {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, chunk) in value.as_bytes().chunks(2).enumerate() {
        let text = std::str::from_utf8(chunk).ok()?;
        out[index] = u8::from_str_radix(text, 16).ok()?;
    }
    Some(out)
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 取 PEM 里第一张证书的 DER。
fn first_certificate_der(pem: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(pem).ok()?;
    let begin = "-----BEGIN CERTIFICATE-----";
    let end = "-----END CERTIFICATE-----";
    let start = text.find(begin)? + begin.len();
    let stop = text[start..].find(end)? + start;
    decode_base64(&text[start..stop])
}

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

impl TransportSecurity {
    /// 为 reqwest 的 `use_preconfigured_tls` 构造配置；Plain 返回 None。
    pub fn rustls_client_config(&self) -> Result<Option<rustls::ClientConfig>, NodeClientError> {
        match self {
            TransportSecurity::Plain => Ok(None),
            TransportSecurity::PinnedFingerprint(fingerprint) => {
                Ok(Some(pinned_client_config(fingerprint)?))
            }
        }
    }
}
