//! 一次性挑战与请求鉴权（API-002 / API-003）。
//!
//! 挑战由节点签名，绑定 node_id、用途和短期期限；服务端只持久化 nonce 的 SHA-256
//! （沿用 0001 的 auth_challenges.nonce_hash），客户端回送 nonce 参与请求签名。
//! 请求签名原文固定为：域分隔 + node_id + device_id + 用途 + 方法 + 规范路径 + 查询 +
//! 正文摘要 + nonce，因此换方法、换路径、换查询、换正文、换节点或换设备都会被拒绝。
use crate::canonical::{
    CHALLENGE_DOMAIN, REQUEST_DOMAIN, canonical_method, canonical_path, canonical_query,
    hex_decode, hex_encode, push_field, validate_bounded_id, validate_timestamp, verify_ed25519,
};
use crate::error::{NodeError, NodeResult};
use crate::limits::{MAX_CHALLENGE_ID_BYTES, MAX_DEVICE_ID_BYTES, MAX_NODE_ID_BYTES};
use serde::{Deserialize, Serialize};

/// 挑战用途：不同用途的挑战不能互换，避免用只读挑战去换注册请求。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChallengePurpose {
    /// 已登记设备的普通请求鉴权。
    RequestAuth,
    /// 设备注册（新设备证明持有私钥）。
    DeviceRegistration,
}

impl ChallengePurpose {
    /// 写入签名原文与数据库的稳定标签。
    pub const fn tag(self) -> u8 {
        match self {
            Self::RequestAuth => 1,
            Self::DeviceRegistration => 2,
        }
    }

    /// 数据库/传输使用的稳定名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RequestAuth => "request_auth",
            Self::DeviceRegistration => "device_registration",
        }
    }

    /// 解析稳定名称，未知即拒绝。
    pub fn parse(value: &str) -> NodeResult<Self> {
        match value {
            "request_auth" => Ok(Self::RequestAuth),
            "device_registration" => Ok(Self::DeviceRegistration),
            _ => Err(NodeError::InvalidRequest("challenge purpose")),
        }
    }
}

/// 请求元数据：只有规范化后的方法/路径/查询进入签名原文。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestMeta {
    pub method: String,
    pub path: String,
    pub query: String,
}

impl RequestMeta {
    /// 规范化并校验 method/path/query；任何越界或歧义写法都显式拒绝。
    pub fn canonical(method: &str, path: &str, query: &str) -> NodeResult<Self> {
        Ok(Self {
            method: canonical_method(method)?,
            path: canonical_path(path)?,
            query: canonical_query(query)?,
        })
    }
}

/// 节点签发的短期一次性挑战。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthChallenge {
    /// 挑战标识，服务端按它原子消费。
    pub challenge_id: String,
    /// 32 字节随机 nonce 的小写十六进制；服务端只存它的 SHA-256。
    pub nonce: String,
    /// 签发节点标识。
    pub node_id: String,
    /// 用途。
    pub purpose: ChallengePurpose,
    /// 签发时间（Unix 秒）。
    pub issued_at: u64,
    /// 过期时间（Unix 秒）；到期后不可再用。
    pub expires_at: u64,
    /// 节点签名公钥（小写十六进制），客户端用它固定节点身份。
    pub node_public_key: String,
    /// 节点对挑战原文的 Ed25519 签名（小写十六进制）。
    pub signature: String,
}

impl AuthChallenge {
    /// 挑战签名原文。
    pub fn signing_bytes(&self) -> NodeResult<Vec<u8>> {
        validate_bounded_id("node id", &self.node_id, MAX_NODE_ID_BYTES)?;
        validate_bounded_id("challenge id", &self.challenge_id, MAX_CHALLENGE_ID_BYTES)?;
        let node_public_key = hex_decode::<32>("node public key", &self.node_public_key)?;
        let nonce = hex_decode::<32>("nonce", &self.nonce)?;
        validate_timestamp("issued at", self.issued_at)?;
        validate_timestamp("expires at", self.expires_at)?;
        let mut bytes = Vec::with_capacity(128);
        bytes.extend_from_slice(CHALLENGE_DOMAIN);
        push_field(&mut bytes, self.node_id.as_bytes());
        bytes.extend_from_slice(&node_public_key);
        push_field(&mut bytes, self.challenge_id.as_bytes());
        bytes.extend_from_slice(&nonce);
        bytes.push(self.purpose.tag());
        bytes.extend_from_slice(&self.issued_at.to_be_bytes());
        bytes.extend_from_slice(&self.expires_at.to_be_bytes());
        Ok(bytes)
    }

    /// 挑战标识的持久化键。
    pub fn nonce_digest(&self) -> NodeResult<[u8; 32]> {
        let nonce = hex_decode::<32>("nonce", &self.nonce)?;
        Ok(crate::canonical::sha256(&nonce))
    }
}

/// 客户端回送的签名鉴权材料。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedAuth {
    /// 使用的挑战标识。
    pub challenge_id: String,
    /// 签名设备标识；必须与凭据中的设备一致。
    pub device_id: String,
    /// 挑战 nonce（小写十六进制），服务端据此校验 nonce_hash。
    pub nonce: String,
    /// 设备对请求签名原文的 Ed25519 签名（小写十六进制）。
    pub signature: String,
}

/// 请求签名原文（设备私钥签名，节点用凭据公钥验证）。
pub fn request_signing_bytes(
    device_id: &str,
    meta: &RequestMeta,
    body_hash: &[u8; 32],
    nonce: &[u8; 32],
    node_id: &str,
    purpose: ChallengePurpose,
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(256);
    bytes.extend_from_slice(REQUEST_DOMAIN);
    push_field(&mut bytes, node_id.as_bytes());
    push_field(&mut bytes, device_id.as_bytes());
    bytes.push(purpose.tag());
    push_field(&mut bytes, meta.method.as_bytes());
    push_field(&mut bytes, meta.path.as_bytes());
    push_field(&mut bytes, meta.query.as_bytes());
    bytes.extend_from_slice(body_hash);
    bytes.extend_from_slice(nonce);
    bytes
}

/// 校验挑战签名与节点绑定；供客户端固定节点身份、供测试证明节点签发的是同一原文。
pub fn verify_auth_challenge(
    challenge: &AuthChallenge,
    expected_node_public_key: &[u8; 32],
) -> NodeResult<()> {
    let node_public_key = hex_decode::<32>("node public key", &challenge.node_public_key)?;
    if &node_public_key != expected_node_public_key {
        return Err(NodeError::NodeIdMismatch);
    }
    let signature = hex_decode::<64>("challenge signature", &challenge.signature)?;
    verify_ed25519(
        expected_node_public_key,
        &challenge.signing_bytes()?,
        &signature,
        || NodeError::InvalidSignature,
    )
}

/// 解码并校验 SignedAuth 的固定长度字段（签名与 nonce）。
pub fn decode_signed_auth(auth: &SignedAuth) -> NodeResult<([u8; 32], [u8; 64])> {
    validate_bounded_id("challenge id", &auth.challenge_id, MAX_CHALLENGE_ID_BYTES)?;
    validate_bounded_id("device id", &auth.device_id, MAX_DEVICE_ID_BYTES)?;
    let nonce = hex_decode::<32>("nonce", &auth.nonce)?;
    let signature = hex_decode::<64>("device signature", &auth.signature)?;
    Ok((nonce, signature))
}

/// 便于日志/测试的 nonce 编码。
pub fn encode_nonce(nonce: &[u8; 32]) -> String {
    hex_encode(nonce)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purpose_labels_round_trip_and_reject_unknown() {
        assert_eq!(ChallengePurpose::RequestAuth.as_str(), "request_auth");
        assert_eq!(ChallengePurpose::DeviceRegistration.tag(), 2);
        assert_eq!(
            ChallengePurpose::parse("device_registration").unwrap(),
            ChallengePurpose::DeviceRegistration
        );
        assert!(ChallengePurpose::parse("admin").is_err());
    }

    #[test]
    fn request_signing_bytes_bind_every_field() {
        let meta = RequestMeta::canonical("post", "/v1/devices", "").unwrap();
        let body_hash = [7_u8; 32];
        let nonce = [9_u8; 32];
        let base =
            request_signing_bytes("dev-1", &meta, &body_hash, &nonce, "node-1", ChallengePurpose::DeviceRegistration);
        let other_method = RequestMeta::canonical("get", "/v1/devices", "").unwrap();
        let other_path = RequestMeta::canonical("post", "/v1/auth/challenge", "").unwrap();
        let other_query = RequestMeta::canonical("post", "/v1/devices", "x=1").unwrap();
        assert_ne!(base, request_signing_bytes("dev-1", &other_method, &body_hash, &nonce, "node-1", ChallengePurpose::DeviceRegistration));
        assert_ne!(base, request_signing_bytes("dev-1", &other_path, &body_hash, &nonce, "node-1", ChallengePurpose::DeviceRegistration));
        assert_ne!(base, request_signing_bytes("dev-1", &other_query, &body_hash, &nonce, "node-1", ChallengePurpose::DeviceRegistration));
        assert_ne!(base, request_signing_bytes("dev-1", &meta, &[8_u8; 32], &nonce, "node-1", ChallengePurpose::DeviceRegistration));
        assert_ne!(base, request_signing_bytes("dev-1", &meta, &body_hash, &[8_u8; 32], "node-1", ChallengePurpose::DeviceRegistration));
        assert_ne!(base, request_signing_bytes("dev-1", &meta, &body_hash, &nonce, "node-2", ChallengePurpose::DeviceRegistration));
        assert_ne!(base, request_signing_bytes("dev-2", &meta, &body_hash, &nonce, "node-1", ChallengePurpose::DeviceRegistration));
        assert_ne!(base, request_signing_bytes("dev-1", &meta, &body_hash, &nonce, "node-1", ChallengePurpose::RequestAuth));
    }
}
