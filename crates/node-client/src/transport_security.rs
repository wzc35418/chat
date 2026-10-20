//! 传输安全：**https + 固定证书**（不信任系统根，只信调用方显式给的那张证书）。
//!
//! 语义：
//! - `http://` 端点必须显式选择 [TransportSecurity::Plain]（默认），用于本地/可信网络；
//! - `https://` 端点必须给 [TransportSecurity::PinnedCertificate]（自签场景没有 CA 链可依赖）；
//! - 配了固定证书却用 `http://` -> 显式拒绝（否则是**假安全**）；
//! - 不给信任却用 `https://` -> 显式拒绝（绝不静默降级成不校验）。

use crate::error::NodeClientError;

/// 传输安全配置。
#[derive(Clone, Debug, Default)]
pub enum TransportSecurity {
    /// 明文 HTTP：只允许与 `http://` 端点搭配。
    #[default]
    Plain,
    /// 固定证书（PEM）：只信任这一张；不加载系统根。
    PinnedCertificate(Vec<u8>),
}

impl TransportSecurity {
    pub fn pinned_pem(pem: impl Into<Vec<u8>>) -> Self {
        TransportSecurity::PinnedCertificate(pem.into())
    }

    pub fn is_plain(&self) -> bool {
        matches!(self, TransportSecurity::Plain)
    }

    /// 端点与信任配置必须自洽；不自洽就报错，不猜。
    pub fn validate(&self, endpoint: &str) -> Result<(), NodeClientError> {
        let https = endpoint.starts_with("https://");
        match (https, self) {
            (true, TransportSecurity::Plain) => Err(NodeClientError::config(
                "TLS_TRUST_REQUIRED: https 端点必须提供固定证书（不信任系统根）",
            )),
            (false, TransportSecurity::PinnedCertificate(_)) => Err(NodeClientError::config(
                "INSECURE_TRANSPORT: 固定证书只能配 https 端点，http 会被拒绝",
            )),
            _ => Ok(()),
        }
    }
}
