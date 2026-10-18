//! 设备注册的类型与签名原文（NODE-001）。
//!
//! 节点只收公开凭据与公开包：设备凭据（账户主管钥签发）、注册能力（账户主管钥签发、
//! 限定 account/device/用途/期限/撤销版本）以及节点自己的签名回执。任何用户私钥都不上传。
use crate::canonical::{
    CAPABILITY_DOMAIN, CREDENTIAL_DOMAIN, REGISTRATION_DOMAIN, hex_decode, push_field, sha256,
    validate_bounded_id, validate_timestamp, verify_ed25519,
};
use crate::error::{NodeError, NodeResult};
use crate::limits::{
    MAX_ACCOUNT_ID_BYTES, MAX_CAPABILITY_ID_BYTES, MAX_CAPABILITY_LIFETIME_SECONDS,
    MAX_CREDENTIAL_LIFETIME_SECONDS, MAX_DEVICE_ID_BYTES, MAX_ENDPOINT_ID_BYTES, MAX_NODE_ID_BYTES,
};
use serde::{Deserialize, Serialize};

/// 注册能力唯一允许的作用域字符串。
pub const REGISTRATION_CAPABILITY_PURPOSE: &str = "device_registration";

/// 账户主管钥签发的设备凭据（公开材料）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCredential {
    pub account_id: String,
    pub device_id: String,
    /// 设备签名公钥（小写十六进制，32 字节）。
    pub device_public_key: String,
    pub endpoint_id: String,
    pub credential_version: u64,
    pub not_before: u64,
    pub expires_at: u64,
    /// 账户主管公钥（小写十六进制，32 字节）；节点按账户固定它。
    pub supervisor_public_key: String,
    /// 主管私钥对凭据原文的签名（小写十六进制，64 字节）。
    pub signature: String,
}

impl DeviceCredential {
    /// 凭据签名原文。
    pub fn signing_bytes(&self) -> NodeResult<Vec<u8>> {
        validate_bounded_id("account id", &self.account_id, MAX_ACCOUNT_ID_BYTES)?;
        validate_bounded_id("device id", &self.device_id, MAX_DEVICE_ID_BYTES)?;
        validate_bounded_id("endpoint id", &self.endpoint_id, MAX_ENDPOINT_ID_BYTES)?;
        let device_public_key = hex_decode::<32>("device public key", &self.device_public_key)?;
        let supervisor_public_key =
            hex_decode::<32>("supervisor public key", &self.supervisor_public_key)?;
        validate_timestamp("not before", self.not_before)?;
        validate_timestamp("expires at", self.expires_at)?;
        let mut bytes = Vec::with_capacity(256);
        bytes.extend_from_slice(CREDENTIAL_DOMAIN);
        push_field(&mut bytes, self.account_id.as_bytes());
        push_field(&mut bytes, self.device_id.as_bytes());
        bytes.extend_from_slice(&device_public_key);
        push_field(&mut bytes, self.endpoint_id.as_bytes());
        bytes.extend_from_slice(&self.credential_version.to_be_bytes());
        bytes.extend_from_slice(&self.not_before.to_be_bytes());
        bytes.extend_from_slice(&self.expires_at.to_be_bytes());
        bytes.extend_from_slice(&supervisor_public_key);
        Ok(bytes)
    }

    /// 设备签名公钥。
    pub fn device_key(&self) -> NodeResult<[u8; 32]> {
        hex_decode::<32>("device public key", &self.device_public_key)
    }

    /// 账户主管公钥。
    pub fn supervisor_key(&self) -> NodeResult<[u8; 32]> {
        hex_decode::<32>("supervisor public key", &self.supervisor_public_key)
    }

    /// 校验主管签名。账户根信任由节点按账户固定（见 NodeService::register_device）。
    pub fn verify(&self) -> NodeResult<()> {
        let supervisor = self.supervisor_key()?;
        let signature = hex_decode::<64>("credential signature", &self.signature)?;
        verify_ed25519(&supervisor, &self.signing_bytes()?, &signature, || {
            NodeError::InvalidCredential
        })
    }

    /// 凭据内容摘要，用于幂等比对与存储。
    pub fn digest(&self) -> NodeResult<[u8; 32]> {
        let signature = hex_decode::<64>("credential signature", &self.signature)?;
        let mut bytes = self.signing_bytes()?;
        bytes.extend_from_slice(&signature);
        Ok(sha256(&bytes))
    }

    /// 期限必须是有上限的区间，且不晚于允许的最长有效期。
    pub fn validate_lifetime(&self) -> NodeResult<()> {
        if self.expires_at <= self.not_before {
            return Err(NodeError::InvalidCredential);
        }
        let lifetime = self.expires_at - self.not_before;
        if lifetime > MAX_CREDENTIAL_LIFETIME_SECONDS {
            return Err(NodeError::InvalidCredential);
        }
        Ok(())
    }
}

/// 账户主管钥签发的一次性注册能力。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrationCapability {
    pub capability_id: String,
    pub account_id: String,
    /// 该能力只允许登记这一台设备。
    pub device_id: String,
    /// 作用域字符串，当前只能是 device_registration。
    pub purpose: String,
    pub issued_at: u64,
    pub expires_at: u64,
    /// 能力版本，用于与能力撤销版本比较。
    pub revocation_version: u64,
    pub supervisor_public_key: String,
    pub signature: String,
}

impl RegistrationCapability {
    /// 能力签名原文。
    pub fn signing_bytes(&self) -> NodeResult<Vec<u8>> {
        validate_bounded_id("capability id", &self.capability_id, MAX_CAPABILITY_ID_BYTES)?;
        validate_bounded_id("account id", &self.account_id, MAX_ACCOUNT_ID_BYTES)?;
        validate_bounded_id("device id", &self.device_id, MAX_DEVICE_ID_BYTES)?;
        if self.purpose.len() > 64 || !self.purpose.is_ascii() {
            return Err(NodeError::InvalidCapability);
        }
        let supervisor_public_key =
            hex_decode::<32>("supervisor public key", &self.supervisor_public_key)?;
        validate_timestamp("issued at", self.issued_at)?;
        validate_timestamp("expires at", self.expires_at)?;
        let mut bytes = Vec::with_capacity(224);
        bytes.extend_from_slice(CAPABILITY_DOMAIN);
        push_field(&mut bytes, self.capability_id.as_bytes());
        push_field(&mut bytes, self.account_id.as_bytes());
        push_field(&mut bytes, self.device_id.as_bytes());
        push_field(&mut bytes, self.purpose.as_bytes());
        bytes.extend_from_slice(&self.issued_at.to_be_bytes());
        bytes.extend_from_slice(&self.expires_at.to_be_bytes());
        bytes.extend_from_slice(&self.revocation_version.to_be_bytes());
        bytes.extend_from_slice(&supervisor_public_key);
        Ok(bytes)
    }

    /// 账户主管公钥。
    pub fn supervisor_key(&self) -> NodeResult<[u8; 32]> {
        hex_decode::<32>("supervisor public key", &self.supervisor_public_key)
    }

    /// 校验主管签名。
    pub fn verify(&self) -> NodeResult<()> {
        let supervisor = self.supervisor_key()?;
        let signature = hex_decode::<64>("capability signature", &self.signature)?;
        verify_ed25519(&supervisor, &self.signing_bytes()?, &signature, || {
            NodeError::InvalidCapability
        })
    }

    /// 期限必须是有上限的短期区间。
    pub fn validate_lifetime(&self) -> NodeResult<()> {
        if self.expires_at <= self.issued_at {
            return Err(NodeError::InvalidCapability);
        }
        if self.expires_at - self.issued_at > MAX_CAPABILITY_LIFETIME_SECONDS {
            return Err(NodeError::InvalidCapability);
        }
        Ok(())
    }
}

/// 设备注册请求（只用公开材料）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterDeviceRequest {
    pub account_id: String,
    pub credential: DeviceCredential,
    pub capability: RegistrationCapability,
}

impl RegisterDeviceRequest {
    /// 请求形状校验：账户、设备、作用域必须彼此一致，字段必须有界。
    pub fn validate_shape(&self) -> NodeResult<()> {
        validate_bounded_id("account id", &self.account_id, MAX_ACCOUNT_ID_BYTES)?;
        if self.credential.account_id != self.account_id
            || self.capability.account_id != self.account_id
        {
            return Err(NodeError::InvalidCapability);
        }
        if self.capability.device_id != self.credential.device_id {
            return Err(NodeError::InvalidCapability);
        }
        if self.capability.purpose != REGISTRATION_CAPABILITY_PURPOSE {
            return Err(NodeError::InvalidCapability);
        }
        if self.credential.supervisor_public_key != self.capability.supervisor_public_key {
            return Err(NodeError::InvalidCredential);
        }
        self.credential.signing_bytes()?;
        self.capability.signing_bytes()?;
        Ok(())
    }
}

/// 节点签发的设备注册回执；重复注册返回同一份持久回执。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRegistration {
    pub account_id: String,
    pub device_id: String,
    pub device_public_key: String,
    pub endpoint_id: String,
    pub credential_version: u64,
    pub registered_at: u64,
    pub node_id: String,
    pub node_public_key: String,
    pub signature: String,
}

impl DeviceRegistration {
    /// 回执签名原文。
    pub fn signing_bytes(&self) -> NodeResult<Vec<u8>> {
        validate_bounded_id("node id", &self.node_id, MAX_NODE_ID_BYTES)?;
        validate_bounded_id("account id", &self.account_id, MAX_ACCOUNT_ID_BYTES)?;
        validate_bounded_id("device id", &self.device_id, MAX_DEVICE_ID_BYTES)?;
        validate_bounded_id("endpoint id", &self.endpoint_id, MAX_ENDPOINT_ID_BYTES)?;
        let device_public_key = hex_decode::<32>("device public key", &self.device_public_key)?;
        let node_public_key = hex_decode::<32>("node public key", &self.node_public_key)?;
        validate_timestamp("registered at", self.registered_at)?;
        let mut bytes = Vec::with_capacity(224);
        bytes.extend_from_slice(REGISTRATION_DOMAIN);
        push_field(&mut bytes, self.node_id.as_bytes());
        bytes.extend_from_slice(&node_public_key);
        push_field(&mut bytes, self.account_id.as_bytes());
        push_field(&mut bytes, self.device_id.as_bytes());
        bytes.extend_from_slice(&device_public_key);
        push_field(&mut bytes, self.endpoint_id.as_bytes());
        bytes.extend_from_slice(&self.credential_version.to_be_bytes());
        bytes.extend_from_slice(&self.registered_at.to_be_bytes());
        Ok(bytes)
    }

    /// 用固定节点公钥验证回执。
    pub fn verify(&self, expected_node_public_key: &[u8; 32]) -> NodeResult<()> {
        let node_public_key = hex_decode::<32>("node public key", &self.node_public_key)?;
        if &node_public_key != expected_node_public_key {
            return Err(NodeError::NodeIdentityMismatch);
        }
        let signature = hex_decode::<64>("registration signature", &self.signature)?;
        verify_ed25519(
            expected_node_public_key,
            &self.signing_bytes()?,
            &signature,
            || NodeError::InvalidSignature,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::hex_encode;

    fn credential() -> DeviceCredential {
        DeviceCredential {
            account_id: "acct-1".into(),
            device_id: "dev-1".into(),
            device_public_key: hex_encode(&[1_u8; 32]),
            endpoint_id: "endpoint-1".into(),
            credential_version: 1,
            not_before: 100,
            expires_at: 1_000,
            supervisor_public_key: hex_encode(&[2_u8; 32]),
            signature: hex_encode(&[3_u8; 64]),
        }
    }

    #[test]
    fn credential_bytes_bind_every_bound_field() {
        let base = credential();
        let mut changed = base.clone();
        changed.endpoint_id = "endpoint-2".into();
        assert_ne!(base.signing_bytes().unwrap(), changed.signing_bytes().unwrap());
        let mut changed = base.clone();
        changed.device_public_key = hex_encode(&[9_u8; 32]);
        assert_ne!(base.signing_bytes().unwrap(), changed.signing_bytes().unwrap());
        let mut changed = base.clone();
        changed.credential_version = 2;
        assert_ne!(base.signing_bytes().unwrap(), changed.signing_bytes().unwrap());
        assert_ne!(base.digest().unwrap(), changed.digest().unwrap());
        assert!(base.validate_lifetime().is_ok());
        let mut expired = base.clone();
        expired.not_before = expired.expires_at;
        assert!(expired.validate_lifetime().is_err());
        let mut too_long = base;
        too_long.not_before = 0;
        too_long.expires_at = MAX_CREDENTIAL_LIFETIME_SECONDS + 1;
        assert!(too_long.validate_lifetime().is_err());
    }

    #[test]
    fn capability_rejects_other_scope_and_bad_hex() {
        let mut capability = RegistrationCapability {
            capability_id: "cap-1".into(),
            account_id: "acct-1".into(),
            device_id: "dev-1".into(),
            purpose: REGISTRATION_CAPABILITY_PURPOSE.into(),
            issued_at: 100,
            expires_at: 200,
            revocation_version: 1,
            supervisor_public_key: hex_encode(&[2_u8; 32]),
            signature: hex_encode(&[3_u8; 64]),
        };
        assert!(capability.signing_bytes().is_ok());
        capability.supervisor_public_key = "zz".into();
        assert!(capability.signing_bytes().is_err());
        let mut request = RegisterDeviceRequest {
            account_id: "acct-1".into(),
            credential: credential(),
            capability: RegistrationCapability {
                capability_id: "cap-1".into(),
                account_id: "acct-1".into(),
                device_id: "dev-1".into(),
                purpose: REGISTRATION_CAPABILITY_PURPOSE.into(),
                issued_at: 100,
                expires_at: 200,
                revocation_version: 1,
                supervisor_public_key: hex_encode(&[2_u8; 32]),
                signature: hex_encode(&[3_u8; 64]),
            },
        };
        assert!(request.validate_shape().is_ok());
        request.capability.purpose = "group_admin".into();
        assert_eq!(request.validate_shape().unwrap_err().code(), "INVALID_CAPABILITY");
    }
}
