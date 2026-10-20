//! 传输信任（TLS 固定指纹）的进程级配置：FFI 侧只做透传与校验。
//!
//! 语义：**部署级**配置，一个进程一套指纹集合；未配置时等价明文 HTTP。
//! 配置非法（空集合 / 非 64 位小写十六进制）必须**立刻报错**，不静默忽略。

use chat_node_client::error::NodeClientError;
use chat_node_client::transport_security::TransportSecurity;

/// 传输信任配置错误（FFI 面）。
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum TransportTrustError {
    /// 指纹格式不合法（空集合或非 64 位小写十六进制）。
    #[error("invalid fingerprint: {reason}")]
    InvalidFingerprint { reason: String },
}

fn map_config_error(error: NodeClientError) -> TransportTrustError {
    TransportTrustError::InvalidFingerprint {
        reason: error.to_string(),
    }
}

/// 配置进程级传输信任：只有指纹集合里的证书会被接受（系统根不参与）。
#[uniffi::export]
pub fn configure_transport_trust(fingerprints: Vec<String>) -> Result<(), TransportTrustError> {
    let trust = TransportSecurity::pinned_fingerprints(fingerprints);
    // 用一个 https 端点做自洽校验：空集合/坏格式在这里就会失败。
    trust.validate("https://127.0.0.1:1").map_err(map_config_error)?;
    chat_node_client::transport_security::set_default_trust(trust);
    Ok(())
}

/// 清除传输信任（回到明文 HTTP 语义）。
#[uniffi::export]
pub fn clear_transport_trust() {
    chat_node_client::transport_security::clear_default_trust();
}

/// 当前传输信任的**摘要**（不暴露完整指纹列表以外的任何东西；未配置返回 None）。
#[uniffi::export]
pub fn transport_trust_summary() -> Option<String> {
    match chat_node_client::transport_security::default_trust() {
        TransportSecurity::Plain => None,
        TransportSecurity::PinnedFingerprints(values) => {
            Some(format!("pinned {} certificate(s)", values.len()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FINGERPRINT_A: &str = "aa";

    fn hex(byte: u8) -> String {
        std::iter::repeat(format!("{byte:02x}")).take(32).collect::<String>()
    }

    #[test]
    fn configure_accepts_valid_fingerprints_and_is_visible_in_summary() {
        let _ = FINGERPRINT_A;
        let result = configure_transport_trust(vec![hex(0xab)]);
        assert!(result.is_ok(), "valid fingerprint must be accepted: {result:?}");
        let summary = transport_trust_summary().expect("trust must be configured");
        assert!(summary.contains('1'), "summary should mention the count: {summary}");
        clear_transport_trust();
        assert!(transport_trust_summary().is_none(), "cleared trust must report None");
    }

    #[test]
    fn empty_or_malformed_fingerprints_are_rejected() {
        clear_transport_trust();
        assert!(configure_transport_trust(Vec::new()).is_err(), "empty set must be rejected");
        assert!(configure_transport_trust(vec!["not-hex".to_owned()]).is_err());
        assert!(configure_transport_trust(vec!["zz".repeat(32)]).is_err());
        // 失败不得污染已有配置。
        assert!(transport_trust_summary().is_none());
        clear_transport_trust();
    }

    #[test]
    fn uppercase_fingerprints_are_normalized_not_rejected() {
        clear_transport_trust();
        let upper = "AB".repeat(32);
        assert!(configure_transport_trust(vec![upper]).is_ok(), "uppercase hex is normalized");
        assert!(transport_trust_summary().is_some());
        // 大小写混合的同一指纹应被去重成一张。
        let mixed = configure_transport_trust(vec!["ab".repeat(32), "AB".repeat(32)]);
        assert!(mixed.is_ok());
        let summary = transport_trust_summary().expect("trust configured");
        assert!(summary.contains('1'), "duplicate fingerprints must be deduplicated: {summary}");
        clear_transport_trust();
    }
}
