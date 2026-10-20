//! TLS 证书状态（公开信息）：启动时由节点进程写入，客户端可匿名读取。
//!
//! 只放**公开**字段：证书到期时间、剩余天数、证书 SHA-256。不含私钥、路径、配置。

use std::sync::OnceLock;

/// 证书状态快照。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsInfo {
    /// notAfter（Unix 秒）。
    pub not_after_unix: i64,
    /// 证书 DER 的 SHA-256（小写 hex）。
    pub cert_sha256: String,
}

static TLS_INFO: OnceLock<TlsInfo> = OnceLock::new();

/// 由节点启动流程写入（只允许写一次；重复写入被忽略，避免运行期被改）。
pub fn set_tls_info(info: TlsInfo) {
    let _ = TLS_INFO.set(info);
}

/// 当前证书状态（未启用 TLS 时为 None）。
pub fn tls_info() -> Option<&'static TlsInfo> {
    TLS_INFO.get()
}

/// 剩余天数（向下取整，可为负）。
pub fn days_remaining(info: &TlsInfo, now_unix: i64) -> i64 {
    info.not_after_unix.saturating_sub(now_unix).div_euclid(86_400)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_remaining_rounds_down_and_goes_negative() {
        let info = TlsInfo {
            not_after_unix: 1_000_000 + 86_400 * 10,
            cert_sha256: "0".repeat(64),
        };
        assert_eq!(days_remaining(&info, 1_000_000), 10);
        assert_eq!(days_remaining(&info, 1_000_000 + 86_400 * 9), 1);
        assert_eq!(days_remaining(&info, 1_000_000 + 86_400 * 11), -1);
    }
}
