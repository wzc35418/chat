//! 节点启动前配置校验（NODE-018）：**必须在创建/打开数据库之前**完成。
//!
//! 任何一条不满足都返回 `INVALID_CONFIG` 且不启动（也不建库）：
//!
//! 1. `node_id`：1..=64 字节、可打印 ASCII、无空白/控制字符；
//! 2. `signing_seed`：不能是全零（弱种子）；长度由类型保证为 32 字节；
//! 3. `policy`：逐项范围校验（复用 [`NodeAuthPolicy::validate`]）；
//! 4. TTL/配额上下限：能力令牌 TTL 1..=3600、信箱/群日志/附件上限必须为正且不超过硬上限；
//! 5. capability 策略一致性：强制模式（`required`）下 TTL 不得短于 60 秒（否则客户端刚拿到就过期），
//!    并且放宽撤销（`revoke.self_only = false`）必须允许审计（审计关闭时由服务层拒绝）。

use crate::error::{NodeError, NodeResult};
use crate::limits::{
    MAX_ACCOUNT_ID_BYTES, MAX_DEVICE_ID_BYTES, MAX_HTTP_BODY_BYTES, MAX_NODE_ID_BYTES,
};
use crate::services::NodeServiceConfig;

/// 强制模式下能力令牌的最小 TTL（秒）。
pub const MIN_ENFORCED_CAPABILITY_TTL_SECONDS: u64 = 60;

/// 启动前配置校验器（无状态；所有函数都是纯校验）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeConfigValidation;

impl NodeConfigValidation {
    /// 校验完整启动配置；失败返回 `INVALID_CONFIG`。
    pub fn validate(config: &NodeServiceConfig) -> NodeResult<()> {
        Self::validate_node_id(&config.node_id)?;
        Self::validate_signing_seed(&config.signing_seed)?;
        config
            .policy
            .validate()
            .map_err(|error| match error {
                NodeError::InvalidConfig(detail) => NodeError::InvalidConfig(detail),
                other => other,
            })?;
        Self::validate_capability_policy(&config)?;
        Ok(())
    }

    /// 节点标识：1..=64 字节、可打印 ASCII、无空白/控制字符。
    pub fn validate_node_id(node_id: &str) -> NodeResult<()> {
        if node_id.is_empty() || node_id.len() > MAX_NODE_ID_BYTES {
            return Err(NodeError::InvalidConfig("node id length"));
        }
        if !node_id.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
            return Err(NodeError::InvalidConfig(
                "node id must be printable ASCII without spaces",
            ));
        }
        Ok(())
    }

    /// 签名种子：全零被拒绝（弱种子等于没有身份）。
    pub fn validate_signing_seed(seed: &[u8; 32]) -> NodeResult<()> {
        if seed.iter().all(|byte| *byte == 0) {
            return Err(NodeError::InvalidConfig(
                "signing seed must not be all zeros",
            ));
        }
        Ok(())
    }

    /// 能力策略一致性与 TTL 上下限。
    fn validate_capability_policy(config: &NodeServiceConfig) -> NodeResult<()> {
        let capability = &config.policy.capability;
        if capability.max_ttl_seconds == 0
            || capability.max_ttl_seconds > crate::capability::MAX_CAPABILITY_TTL_SECONDS
        {
            return Err(NodeError::InvalidConfig("capability ttl bounds"));
        }
        if capability.required && capability.max_ttl_seconds < MIN_ENFORCED_CAPABILITY_TTL_SECONDS {
            return Err(NodeError::InvalidConfig(
                "enforced capability mode needs a ttl of at least 60 seconds",
            ));
        }
        // 其它配额上下限（信箱/群日志/附件）必须为正；越界由各策略的 validate 负责。
        if config.policy.mailbox.max_page_items == 0
            || config.policy.mailbox.max_item_bytes == 0
            || config.policy.mailbox.max_bytes_per_device == 0
        {
            return Err(NodeError::InvalidConfig("mailbox quota bounds"));
        }
        if config.policy.group_log.max_page_entries == 0
            || config.policy.group_log.max_members == 0
            || config.policy.group_log.max_candidate_ciphertext_bytes == 0
        {
            return Err(NodeError::InvalidConfig("group log quota bounds"));
        }
        if config.policy.blob.max_object_bytes == 0
            || config.policy.blob.max_chunk_bytes == 0
            || config.policy.blob.max_chunk_count == 0
        {
            return Err(NodeError::InvalidConfig("blob quota bounds"));
        }
        // 请求体/标识上限是常量，这里只做编译期可见性引用，避免未使用导入。
        let _ = (MAX_ACCOUNT_ID_BYTES, MAX_DEVICE_ID_BYTES, MAX_HTTP_BODY_BYTES);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{NodeAuthPolicy, NodeServiceConfig};

    fn config() -> NodeServiceConfig {
        NodeServiceConfig::new("node-audit-test", [7u8; 32])
    }

    #[test]
    fn a_default_config_passes_validation() {
        NodeConfigValidation::validate(&config()).expect("default config is valid");
    }

    #[test]
    fn malformed_node_ids_are_rejected() {
        let too_long = "x".repeat(MAX_NODE_ID_BYTES + 1);
        for bad in ["", "has space", "has\tTab", too_long.as_str()] {
            let mut broken = config();
            broken.node_id = bad.to_owned();
            assert_eq!(
                NodeConfigValidation::validate(&broken)
                    .expect_err("bad node id")
                    .code(),
                "INVALID_CONFIG"
            );
        }
    }

    #[test]
    fn zero_signing_seed_is_rejected() {
        let mut broken = config();
        broken.signing_seed = [0u8; 32];
        assert_eq!(
            NodeConfigValidation::validate(&broken)
                .expect_err("zero seed")
                .code(),
            "INVALID_CONFIG"
        );
    }

    #[test]
    fn capability_policy_inconsistencies_are_rejected() {
        let mut enforced = config();
        enforced.policy.capability.required = true;
        enforced.policy.capability.max_ttl_seconds = 10;
        assert_eq!(
            NodeConfigValidation::validate(&enforced)
                .expect_err("short enforced ttl")
                .code(),
            "INVALID_CONFIG"
        );
        let mut relaxed = config();
        relaxed.policy = NodeAuthPolicy::default();
        relaxed.policy.capability.max_ttl_seconds = 0;
        assert!(NodeConfigValidation::validate(&relaxed).is_err());
    }
}

