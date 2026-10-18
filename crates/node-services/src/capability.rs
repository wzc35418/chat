//! API-004：节点签发的**对象级能力令牌**（无状态签名令牌，不落库）。
//!
//! 形状：`cap1.<hex(payload)>.<hex(signature)>`。payload 是域分隔 + 长度前缀的规范编码：
//!
//! ```text
//! domain || object_id || device_id || scope || issued_at(be64) || expires_at(be64) || node_id
//! ```
//!
//! 语义：
//!
//! - **绑定**：令牌与 @(object_id, device_id, scope)@ 一一对应；换对象、换设备、换用途都必须失败，
//!   所以“上传令牌”不能用来下载，“A 对象的令牌”不能读 B 对象（不可跨对象重放）；
//! - **过期**：@issued_at <= now < expires_at@，TTL 有硬上限（[MAX_CAPABILITY_TTL_SECONDS]）；
//! - **签名**：由节点签名钥签发，验证时用节点公钥；payload 会被重新编码并要求与原文逐字节相同，
//!   因此不存在“同一语义多种编码”的可塑性；
//! - **无状态**：不落库、不需要清理（过期即失效）。撤销要么换对象，要么撤销设备（设备撤销会让设备
//!   鉴权先行失败）。
//!
//! 授权分工：本模块只负责“令牌签得对、验得对”。**对象级授权**（这台设备能不能对这个对象拿令牌）
//! 由调用方先判定：node-api 在签发前会检查上传会话归属或附件可读性。
use ed25519_dalek::{Signature, Signer};

use chat_protocol::ObjectId;

use crate::canonical::{
    decode_hex_bytes, hex_decode, hex_encode, push_field, validate_bounded_id, validate_timestamp,
    verify_ed25519,
};
use crate::error::{NodeError, NodeResult};
use crate::limits::{MAX_BLOB_ID_BYTES, MAX_BLOB_UPLOAD_ID_BYTES, MAX_DEVICE_ID_BYTES};
use crate::services::NodeService;

/// 能力令牌签名域（与设备注册能力、回执等域分离）。
pub const AUTH_CAPABILITY_DOMAIN: &[u8] = b"chat/p1/node-object-capability/v1";
/// 令牌文本前缀（版本化，便于将来换格式）。
pub const CAPABILITY_TOKEN_PREFIX: &str = "cap1.";
/// 令牌 TTL 默认值（10 分钟）。
pub const DEFAULT_CAPABILITY_TTL_SECONDS: u64 = 600;
/// 令牌 TTL 硬上限（1 小时）：能力令牌是短期凭据，不是长期授权。
pub const MAX_CAPABILITY_TTL_SECONDS: u64 = 3_600;
/// 令牌文本长度上限（含前缀与签名）。
pub const MAX_CAPABILITY_TOKEN_BYTES: usize = 2_048;

/// 能力用途。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CapabilityScope {
    /// 向一个上传会话写密文块。
    BlobUpload,
    /// 读取一个已发布对象的密文块。
    BlobDownload,
}

impl CapabilityScope {
    /// 稳定文本（进入签名原文，不可改）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BlobUpload => "blob:upload",
            Self::BlobDownload => "blob:download",
        }
    }

    /// 由稳定文本还原；未知文本显式失败。
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "blob:upload" => Some(Self::BlobUpload),
            "blob:download" => Some(Self::BlobDownload),
            _ => None,
        }
    }

    /// 该用途的对象标识该按哪种 PROTOCOL-001 文本校验。
    fn object_kind(self) -> ObjectKind {
        match self {
            Self::BlobUpload => ObjectKind::UploadId,
            Self::BlobDownload => ObjectKind::BlobId,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObjectKind {
    UploadId,
    BlobId,
}

/// 已验证的令牌内容（只含公开绑定字段）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityGrant {
    /// 被授权的对象（上传会话 id 或已发布 blob id）。
    pub object_id: String,
    /// 被授权的设备。
    pub device_id: String,
    /// 用途。
    pub scope: CapabilityScope,
    /// 签发时间（Unix 秒）。
    pub issued_at: u64,
    /// 过期时间（Unix 秒）。
    pub expires_at: u64,
    /// 签发节点。
    pub node_id: String,
}

impl CapabilityGrant {
    /// 规范编码的 payload 字节（签名覆盖它）。
    fn encode_payload(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(256);
        bytes.extend_from_slice(AUTH_CAPABILITY_DOMAIN);
        push_field(&mut bytes, self.object_id.as_bytes());
        push_field(&mut bytes, self.device_id.as_bytes());
        push_field(&mut bytes, self.scope.as_str().as_bytes());
        bytes.extend_from_slice(&self.issued_at.to_be_bytes());
        bytes.extend_from_slice(&self.expires_at.to_be_bytes());
        push_field(&mut bytes, self.node_id.as_bytes());
        bytes
    }
}

/// 校验对象标识形状（上传会话 id 是节点随机文本，blob id 是 PROTOCOL-001 object 文本）。
fn validate_object_id(scope: CapabilityScope, object_id: &str) -> NodeResult<()> {
    match scope.object_kind() {
        ObjectKind::UploadId => validate_bounded_id("upload id", object_id, MAX_BLOB_UPLOAD_ID_BYTES),
        ObjectKind::BlobId => {
            validate_bounded_id("blob id", object_id, MAX_BLOB_ID_BYTES)?;
            ObjectId::parse(object_id).map_err(|_| NodeError::InvalidRequest("blob id"))?;
            Ok(())
        }
    }
}

/// 设备必须已登记且仍有效（撤销的设备拿不到新令牌）。
fn require_active_registered_device(service: &NodeService, device_id: &str) -> NodeResult<()> {
    let (_, state, _) = service
        .device_account_state(device_id)?
        .ok_or(NodeError::InvalidCredential)?;
    if state != "active" {
        return Err(NodeError::DeviceRevoked);
    }
    Ok(())
}

impl NodeService {
    /// 签发一个对象级能力令牌。
    ///
    /// 前置：设备已登记且未撤销、对象标识形状合法、TTL 在 @1..=MAX_CAPABILITY_TTL_SECONDS@。
    /// **对象级授权由调用方先判定**（本函数不判断“这台设备该不该拿到这个对象”）。
    pub fn issue_capability(
        &mut self,
        object_id: &str,
        device_id: &str,
        scope: CapabilityScope,
        ttl_seconds: u64,
        now: u64,
    ) -> NodeResult<String> {
        validate_timestamp("now", now)?;
        validate_bounded_id("device id", device_id, MAX_DEVICE_ID_BYTES)?;
        validate_object_id(scope, object_id)?;
        // 生效上限 = 策略上限（CAP 包可调）与协议的硬上限取小。
        let policy_limit = self
            .policy()
            .capability
            .max_ttl_seconds
            .min(MAX_CAPABILITY_TTL_SECONDS);
        if ttl_seconds == 0 || ttl_seconds > policy_limit {
            return Err(NodeError::InvalidRequest("capability ttl"));
        }
        let expires_at = now
            .checked_add(ttl_seconds)
            .ok_or(NodeError::InvalidRequest("capability ttl"))?;
        validate_timestamp("capability expiry", expires_at)?;
        require_active_registered_device(self, device_id)?;
        let grant = CapabilityGrant {
            object_id: object_id.to_owned(),
            device_id: device_id.to_owned(),
            scope,
            issued_at: now,
            expires_at,
            node_id: self.node_id().to_owned(),
        };
        let payload = grant.encode_payload();
        let signature: Signature = self.signing_key().sign(&payload);
        Ok(format!(
            "{CAPABILITY_TOKEN_PREFIX}{}.{}",
            hex_encode(&payload),
            hex_encode(&signature.to_bytes())
        ))
    }

    /// 校验能力令牌：绑定（对象/设备/用途）+ 过期 + 节点身份 + 签名。
    ///
    /// 任何一项不满足都显式失败：过期 → @AUTH_EXPIRED@；绑定不符 → @INVALID_CAPABILITY@；
    /// 节点不符 → @NODE_ID_MISMATCH@；签名/编码不符 → @INVALID_SIGNATURE@ / @INVALID_REQUEST@。
    pub fn verify_capability(
        &self,
        token: &str,
        object_id: &str,
        device_id: &str,
        scope: CapabilityScope,
        now: u64,
    ) -> NodeResult<CapabilityGrant> {
        validate_timestamp("now", now)?;
        let grant = self.parse_capability(token)?;
        if grant.scope != scope
            || grant.object_id != object_id
            || grant.device_id != device_id
        {
            return Err(NodeError::InvalidCapability);
        }
        if grant.node_id != self.node_id() {
            return Err(NodeError::NodeIdMismatch);
        }
        if now < grant.issued_at {
            // 本机时钟早于签发时间（时钟回拨或令牌来自未来）：不接受。
            return Err(NodeError::InvalidCapability);
        }
        if now >= grant.expires_at {
            return Err(NodeError::AuthExpired);
        }
        Ok(grant)
    }

    /// 解析并验签令牌（不校验绑定，供上层自行组合校验）。
    pub fn parse_capability(&self, token: &str) -> NodeResult<CapabilityGrant> {
        if token.is_empty() || token.len() > MAX_CAPABILITY_TOKEN_BYTES {
            return Err(NodeError::InvalidRequest("capability token"));
        }
        let rest = token
            .strip_prefix(CAPABILITY_TOKEN_PREFIX)
            .ok_or(NodeError::InvalidRequest("capability token"))?;
        let (payload_hex, signature_hex) = rest
            .split_once('.')
            .ok_or(NodeError::InvalidRequest("capability token"))?;
        let payload = decode_hex_bytes("capability payload", payload_hex, 1_024)?;
        let signature_bytes = hex_decode::<64>("capability signature", signature_hex)?;
        if payload.len() < AUTH_CAPABILITY_DOMAIN.len()
            || &payload[..AUTH_CAPABILITY_DOMAIN.len()] != AUTH_CAPABILITY_DOMAIN
        {
            return Err(NodeError::InvalidSignature);
        }
        let grant = decode_grant(&payload)?;
        // 规范形式复核：重编码必须与原文逐字节相同（杜绝可塑编码）。
        if grant.encode_payload() != payload {
            return Err(NodeError::InvalidSignature);
        }
        verify_ed25519(&self.node_public_key(), &payload, &signature_bytes, || {
            NodeError::InvalidSignature
        })?;
        Ok(grant)
    }
}

/// 从 payload 解出各字段（严格长度前缀，多余/缺失字节都失败）。
fn decode_grant(payload: &[u8]) -> NodeResult<CapabilityGrant> {
    let mut cursor = AUTH_CAPABILITY_DOMAIN.len();
    let object_id = read_field(payload, &mut cursor)?;
    let device_id = read_field(payload, &mut cursor)?;
    let scope_text = read_field(payload, &mut cursor)?;
    let scope = CapabilityScope::parse(&scope_text)
        .ok_or(NodeError::InvalidRequest("capability scope"))?;
    let issued_at = read_u64(payload, &mut cursor)?;
    let expires_at = read_u64(payload, &mut cursor)?;
    let node_id = read_field(payload, &mut cursor)?;
    if cursor != payload.len() {
        return Err(NodeError::InvalidSignature);
    }
    validate_timestamp("capability issued at", issued_at)?;
    validate_timestamp("capability expiry", expires_at)?;
    if expires_at <= issued_at {
        return Err(NodeError::InvalidRequest("capability window"));
    }
    validate_object_id(scope, &object_id)?;
    validate_bounded_id("device id", &device_id, MAX_DEVICE_ID_BYTES)?;
    Ok(CapabilityGrant {
        object_id,
        device_id,
        scope,
        issued_at,
        expires_at,
        node_id,
    })
}

fn read_field(payload: &[u8], cursor: &mut usize) -> NodeResult<String> {
    if payload.len() < *cursor + 2 {
        return Err(NodeError::InvalidSignature);
    }
    let length = usize::from(u16::from_be_bytes([
        payload[*cursor],
        payload[*cursor + 1],
    ]));
    *cursor += 2;
    let end = cursor
        .checked_add(length)
        .ok_or(NodeError::InvalidSignature)?;
    if payload.len() < end {
        return Err(NodeError::InvalidSignature);
    }
    let bytes = payload[*cursor..end].to_vec();
    *cursor = end;
    String::from_utf8(bytes).map_err(|_| NodeError::InvalidSignature)
}

fn read_u64(payload: &[u8], cursor: &mut usize) -> NodeResult<u64> {
    let end = cursor.checked_add(8).ok_or(NodeError::InvalidSignature)?;
    if payload.len() < end {
        return Err(NodeError::InvalidSignature);
    }
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&payload[*cursor..end]);
    *cursor = end;
    Ok(u64::from_be_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::NodeServiceConfig;

    /// 一个可签发/校验令牌的最小节点（设备登记在集成测试里覆盖）。
    fn service() -> NodeService {
        NodeService::open_in_memory(NodeServiceConfig {
            node_id: "node-capability".into(),
            signing_seed: [7_u8; 32],
            policy: Default::default(),
        })
        .expect("service")
    }

    fn object_text(seed: u8) -> String {
        ObjectId::from_bytes([seed; 32]).encode()
    }

    #[test]
    fn scopes_have_stable_text() {
        assert_eq!(CapabilityScope::BlobUpload.as_str(), "blob:upload");
        assert_eq!(CapabilityScope::BlobDownload.as_str(), "blob:download");
        assert_eq!(
            CapabilityScope::parse("blob:upload"),
            Some(CapabilityScope::BlobUpload)
        );
        assert_eq!(CapabilityScope::parse("blob:delete"), None);
    }

    /// payload 规范编码往返、绑定、过期与篡改（不需要注册设备，直接手工签一个令牌）。
    #[test]
    fn payload_round_trip_binding_and_tamper_are_enforced() {
        let service = service();
        let grant = CapabilityGrant {
            object_id: object_text(41),
            device_id: "dev:1:".to_owned() + &"33".repeat(32),
            scope: CapabilityScope::BlobDownload,
            issued_at: 1_000,
            expires_at: 1_060,
            node_id: service.node_id().to_owned(),
        };
        let payload = grant.encode_payload();
        let signature: Signature = service.signing_key().sign(&payload);
        let token = format!(
            "{CAPABILITY_TOKEN_PREFIX}{}.{}",
            hex_encode(&payload),
            hex_encode(&signature.to_bytes())
        );

        assert_eq!(service.parse_capability(&token).expect("parses"), grant);
        assert_eq!(
            service
                .verify_capability(
                    &token,
                    &grant.object_id,
                    &grant.device_id,
                    CapabilityScope::BlobDownload,
                    1_030
                )
                .expect("verifies"),
            grant
        );
        // 到期即拒绝。
        assert_eq!(
            service
                .verify_capability(
                    &token,
                    &grant.object_id,
                    &grant.device_id,
                    CapabilityScope::BlobDownload,
                    1_060
                )
                .expect_err("expired")
                .code(),
            "AUTH_EXPIRED"
        );
        // 绑定不符。
        assert_eq!(
            service
                .verify_capability(
                    &token,
                    &object_text(42),
                    &grant.device_id,
                    CapabilityScope::BlobDownload,
                    1_030
                )
                .expect_err("other object")
                .code(),
            "INVALID_CAPABILITY"
        );
        assert_eq!(
            service
                .verify_capability(
                    &token,
                    &grant.object_id,
                    &("dev:1:".to_owned() + &"44".repeat(32)),
                    CapabilityScope::BlobDownload,
                    1_030
                )
                .expect_err("other device")
                .code(),
            "INVALID_CAPABILITY"
        );
        assert_eq!(
            service
                .verify_capability(
                    &token,
                    &grant.object_id,
                    &grant.device_id,
                    CapabilityScope::BlobUpload,
                    1_030
                )
                .expect_err("other scope")
                .code(),
            "INVALID_CAPABILITY"
        );
        // 篡改 payload 的一个字节：规范形式复核/验签必失败。
        let mut tampered = payload.clone();
        let index = AUTH_CAPABILITY_DOMAIN.len() + 2;
        tampered[index] ^= 0x01;
        let tampered_token = format!(
            "{CAPABILITY_TOKEN_PREFIX}{}.{}",
            hex_encode(&tampered),
            hex_encode(&signature.to_bytes())
        );
        assert!(
            service.parse_capability(&tampered_token).is_err(),
            "tampered payload must not verify"
        );
    }
}

