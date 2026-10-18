//! PROTOCOL-004/006：验签原文字节。
//!
//! 两个用途各有独立域分隔字符串，且原文内部再编码协议主/次版本、消息类别与用途：
//!
//! - 候选作者签名：`chat/p1/candidate-author-signature/v1`；
//! - 节点回执签名：`chat/p1/coordinator-receipt-signature/v1`。
//!
//! 签名原文是「域字符串 || 规范编码」，不含 `author_signature` 自身。
//! 任何被签字段（群、设备、kind、head/policy/epoch、密文摘要、控制元数据）变化都会
//! 改变原文，因此验签失败。

pub use ed25519_dalek::SigningKey;
use ed25519_dalek::{Signature, Signer, VerifyingKey};

use crate::canonical::{
    CanonicalBytes, ProtocolVersion, WireField, WireValue, encode_canonical, field,
};
use crate::digest::{ControlHead, Digest};
use crate::error::{ProtocolError, Result};
use crate::ids::{DeviceId, GroupId, SubmissionId};

/// 候选作者签名域。
pub const CANDIDATE_SIGNING_DOMAIN: &[u8] = b"chat/p1/candidate-author-signature/v1";
/// 节点回执签名域。
pub const RECEIPT_SIGNING_DOMAIN: &[u8] = b"chat/p1/coordinator-receipt-signature/v1";
/// 候选密文上限（与消息上限一致）。
pub const MAX_CIPHERTEXT_BYTES: usize = 64 * 1024;
/// 当前规范 schema 版本。
pub const SCHEMA_VERSION: u64 = 1;

/// 候选类别。外层签名必须绑定类别，避免应用事件与控制元数据互换。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateKind {
    /// 普通应用密文。
    Application = 1,
    /// 单一受授权群控制操作。
    Control = 2,
}

impl CandidateKind {
    /// 冻结数值。
    pub const fn as_u64(self) -> u64 {
        self as u64
    }

    /// 由冻结数值还原；未知值显式失败。
    pub const fn from_u64(value: u64) -> Option<Self> {
        match value {
            1 => Some(CandidateKind::Application),
            2 => Some(CandidateKind::Control),
            _ => None,
        }
    }
}

/// 公开控制操作种类。群名等正文不进公开层。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlOperation {
    /// 邀请成员。
    Invite = 1,
    /// 移除成员。
    Remove = 2,
    /// 变更角色。
    SetRole = 3,
    /// 设置禁言。
    SetMuted = 4,
    /// 改名（公开层只留摘要）。
    Rename = 5,
    /// 转让群主。
    TransferOwnership = 6,
    /// 退出。
    Leave = 7,
    /// 解散。
    Dissolve = 8,
}

impl ControlOperation {
    /// 冻结数值。
    pub const fn as_u64(self) -> u64 {
        self as u64
    }

    /// 由冻结数值还原；未知值显式失败。
    pub const fn from_u64(value: u64) -> Option<Self> {
        match value {
            1 => Some(ControlOperation::Invite),
            2 => Some(ControlOperation::Remove),
            3 => Some(ControlOperation::SetRole),
            4 => Some(ControlOperation::SetMuted),
            5 => Some(ControlOperation::Rename),
            6 => Some(ControlOperation::TransferOwnership),
            7 => Some(ControlOperation::Leave),
            8 => Some(ControlOperation::Dissolve),
            _ => None,
        }
    }
}

/// 领域操作声明（可选）：当 MLS 提交只能表达 `SelfUpdate`/自移除/无成员变更时，
/// 用它显式声明真正的领域语义（改角色 / 转让群主 / 本机退出 / 解散）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomainOperation {
    /// 常规改角色（MLS `SelfUpdate`）。
    SetRole = 1,
    /// 转让群主（MLS 侧通常也表达为 `SelfUpdate`，但领域语义是转让）。
    TransferOwnership = 2,
    /// 本机退出（`leave_self`）。
    LeaveSelf = 3,
    /// 解散群（无成员变更的领域操作）。
    Dissolve = 4,
}

impl DomainOperation {
    /// 冻结数值。
    pub const fn as_u64(self) -> u64 {
        self as u64
    }

    /// 由冻结数值还原；未知值显式失败（白名单）。
    pub const fn from_u64(value: u64) -> Option<Self> {
        match value {
            1 => Some(DomainOperation::SetRole),
            2 => Some(DomainOperation::TransferOwnership),
            3 => Some(DomainOperation::LeaveSelf),
            4 => Some(DomainOperation::Dissolve),
            _ => None,
        }
    }
}

/// 领域操作声明：操作种类 + 目标摘要（目标账户/设备集合/群等）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DomainOperationDeclaration {
    /// 领域操作。
    pub operation: DomainOperation,
    /// 目标摘要；必须是非零 32 字节摘要。
    pub target_digest: Digest,
}

/// 单一控制操作的公开元数据。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlMetadata {
    /// 操作种类（公开层；转让在 MLS `SelfUpdate` 场景下通常是 `SetRole`）。
    pub operation: ControlOperation,
    /// 目标对象摘要（例如目标账户/设备集合的规范摘要）。
    pub target_digest: Digest,
    /// 可选的领域操作声明；`None` 表示旧字节/未声明。
    pub domain_operation: Option<DomainOperationDeclaration>,
}

impl ControlMetadata {
    /// 无领域声明的公开元数据（兼容旧调用点）。
    pub const fn new(operation: ControlOperation, target_digest: Digest) -> Self {
        Self {
            operation,
            target_digest,
            domain_operation: None,
        }
    }

    /// 附带领域操作声明。
    pub const fn with_domain_operation(mut self, declaration: DomainOperationDeclaration) -> Self {
        self.domain_operation = Some(declaration);
        self
    }
}

fn validate_domain_declaration(kind: CandidateKind, metadata: &ControlMetadata) -> Result<()> {
    let Some(declaration) = metadata.domain_operation else {
        return Ok(());
    };
    if kind != CandidateKind::Control {
        return Err(ProtocolError::InvalidCandidate);
    }
    let compatible = match declaration.operation {
        DomainOperation::SetRole => metadata.operation == ControlOperation::SetRole,
        // MLS 把转让表达为 SelfUpdate（公开层 set_role），直连中继/兼容路径也可能直接给 transfer。
        DomainOperation::TransferOwnership => matches!(
            metadata.operation,
            ControlOperation::SetRole | ControlOperation::TransferOwnership
        ),
        DomainOperation::LeaveSelf => metadata.operation == ControlOperation::Leave,
        DomainOperation::Dissolve => metadata.operation == ControlOperation::Dissolve,
    };
    if !compatible {
        return Err(ProtocolError::InvalidCandidate);
    }
    if declaration.target_digest == Digest::ZERO {
        return Err(ProtocolError::InvalidCandidate);
    }
    Ok(())
}

/// 未签名候选：签名原文覆盖的全部路由与控制字段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsignedCandidate {
    /// 协议版本。
    pub protocol_version: ProtocolVersion,
    /// 固定密文提交 ID。
    pub submission_id: SubmissionId,
    /// 目标群。
    pub group_id: GroupId,
    /// 作者设备。
    pub author_device_id: DeviceId,
    /// 候选类别。
    pub kind: CandidateKind,
    /// 期望的正式控制头。
    pub expected_control_head: ControlHead,
    /// 期望的策略版本。
    pub policy_version: u64,
    /// 期望的 MLS epoch。
    pub mls_epoch: u64,
    /// 加密负载。
    pub encrypted_payload: Vec<u8>,
    /// 控制操作元数据；仅 `Control` 候选必须有。
    pub control_metadata: Option<ControlMetadata>,
}

impl UnsignedCandidate {
    fn validate(&self) -> Result<()> {
        if !self.protocol_version.is_supported() {
            return Err(ProtocolError::UnsupportedProtocolVersion);
        }
        if self.encrypted_payload.is_empty() || self.encrypted_payload.len() > MAX_CIPHERTEXT_BYTES
        {
            return Err(ProtocolError::ContentTooLarge {
                limit: MAX_CIPHERTEXT_BYTES,
            });
        }
        match (self.kind, &self.control_metadata) {
            (CandidateKind::Control, Some(metadata)) => {
                validate_domain_declaration(self.kind, metadata)
            }
            (CandidateKind::Application, None) => Ok(()),
            // 应用候选不得夹带控制元数据，控制候选不得缺少元数据。
            _ => Err(ProtocolError::InvalidCandidate),
        }
    }

    /// 密文摘要。
    pub fn payload_digest(&self) -> Digest {
        Digest::sha256(&self.encrypted_payload)
    }

    /// 候选全文摘要（用于节点回执与日志链）。
    pub fn candidate_digest(&self) -> Result<Digest> {
        candidate_signing_bytes(self).map(|bytes| Digest::sha256(bytes.as_bytes()))
    }
}

fn candidate_body(candidate: &UnsignedCandidate) -> WireValue {
    let control_operation = candidate
        .control_metadata
        .map(|metadata| WireValue::U64(metadata.operation.as_u64()));
    let control_target = candidate
        .control_metadata
        .map(|metadata| WireValue::digest(metadata.target_digest));
    let mut body_fields = vec![
        WireField::new(field::SCHEMA_VERSION, WireValue::U64(SCHEMA_VERSION)),
        WireField::new(
            field::PROTOCOL_MAJOR,
            WireValue::U64(u64::from(candidate.protocol_version.major)),
        ),
        WireField::new(
            field::PROTOCOL_MINOR,
            WireValue::U64(u64::from(candidate.protocol_version.minor)),
        ),
        WireField::new(
            field::SUBMISSION_ID,
            WireValue::id_bytes(candidate.submission_id.as_bytes()),
        ),
        WireField::new(
            field::GROUP_ID,
            WireValue::id_bytes(candidate.group_id.as_bytes()),
        ),
        WireField::new(
            field::AUTHOR_DEVICE_ID,
            WireValue::id_bytes(candidate.author_device_id.as_bytes()),
        ),
        WireField::new(field::KIND, WireValue::U64(candidate.kind.as_u64())),
        WireField::new(
            field::CONTROL_HEAD,
            WireValue::id_bytes(candidate.expected_control_head.as_bytes()),
        ),
        WireField::new(
            field::POLICY_VERSION,
            WireValue::U64(candidate.policy_version),
        ),
        WireField::new(field::MLS_EPOCH, WireValue::U64(candidate.mls_epoch)),
        WireField::new(
            field::PAYLOAD_DIGEST,
            WireValue::digest(candidate.payload_digest()),
        ),
        WireField::new(
            field::CONTROL_OPERATION,
            WireValue::optional(control_operation),
        ),
        WireField::new(
            field::CONTROL_TARGET_DIGEST,
            WireValue::optional(control_target),
        ),
    ];
    // 旧候选（无领域声明）不写新字段，保证既有固定向量字节不变。
    if let Some(declaration) = candidate
        .control_metadata
        .and_then(|metadata| metadata.domain_operation)
    {
        body_fields.push(WireField::new(
            field::CONTROL_DOMAIN_OPERATION,
            WireValue::U64(declaration.operation.as_u64()),
        ));
        body_fields.push(WireField::new(
            field::CONTROL_DOMAIN_TARGET_DIGEST,
            WireValue::digest(declaration.target_digest),
        ));
    }
    WireValue::Struct(body_fields)
}

/// PROTOCOL-004：候选作者签名原文。
///
/// 计算 payload 摘要 → 加入用途域 → 规范编码全部路由/控制字段（排除
/// `author_signature` 自身）。字段任一变化都会改变输出。
pub fn candidate_signing_bytes(candidate: &UnsignedCandidate) -> Result<CanonicalBytes> {
    candidate.validate()?;
    let body = encode_canonical(&candidate_body(candidate), candidate.protocol_version)?;
    let mut bytes = Vec::with_capacity(CANDIDATE_SIGNING_DOMAIN.len() + body.len());
    bytes.extend_from_slice(CANDIDATE_SIGNING_DOMAIN);
    bytes.extend_from_slice(body.as_bytes());
    Ok(CanonicalBytes::from_raw(bytes))
}

/// 从候选规范结构解码可选领域操作声明。
///
/// 旧字节（没有这两个字段）返回 `Ok(None)`；只出现一个字段、未知枚举值、
/// 目标摘要长度不是 32 字节或全零都显式失败（`INVALID_CANDIDATE`）。
pub fn decode_domain_operation(body: &WireValue) -> Result<Option<DomainOperationDeclaration>> {
    let WireValue::Struct(fields) = body else {
        return Err(ProtocolError::InvalidCandidate);
    };
    let operation = fields
        .iter()
        .find(|candidate| candidate.id == field::CONTROL_DOMAIN_OPERATION);
    let target = fields
        .iter()
        .find(|candidate| candidate.id == field::CONTROL_DOMAIN_TARGET_DIGEST);
    match (operation, target) {
        (None, None) => Ok(None),
        (Some(operation), Some(target)) => {
            let WireValue::U64(raw) = operation.value else {
                return Err(ProtocolError::InvalidCandidate);
            };
            let operation =
                DomainOperation::from_u64(raw).ok_or(ProtocolError::InvalidCandidate)?;
            let WireValue::Bytes(bytes) = &target.value else {
                return Err(ProtocolError::InvalidCandidate);
            };
            let digest: [u8; 32] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| ProtocolError::InvalidCandidate)?;
            let target_digest = Digest::from_bytes(digest);
            if target_digest == Digest::ZERO {
                return Err(ProtocolError::InvalidCandidate);
            }
            Ok(Some(DomainOperationDeclaration {
                operation,
                target_digest,
            }))
        }
        // 两个字段必须成对出现。
        _ => Err(ProtocolError::InvalidCandidate),
    }
}

/// PROTOCOL-006：节点回执签名原文。
///
/// seq 从 1 开始；节点签名覆盖候选全文摘要、群、序号与前序日志摘要。
pub fn receipt_signing_bytes(
    candidate_hash: Digest,
    seq: u64,
    previous: Digest,
    group: GroupId,
) -> Result<CanonicalBytes> {
    if seq == 0 {
        return Err(ProtocolError::InvalidSequence);
    }
    let body = WireValue::structure([
        WireField::new(field::SCHEMA_VERSION, WireValue::U64(SCHEMA_VERSION)),
        WireField::new(field::RECEIPT_SEQ, WireValue::U64(seq)),
        WireField::new(field::GROUP_ID, WireValue::id_bytes(group.as_bytes())),
        WireField::new(field::PREVIOUS_LOG_HASH, WireValue::digest(previous)),
        WireField::new(field::CANDIDATE_DIGEST, WireValue::digest(candidate_hash)),
    ]);
    let body = encode_canonical(&body, ProtocolVersion::V1)?;
    let mut bytes = Vec::with_capacity(RECEIPT_SIGNING_DOMAIN.len() + body.len());
    bytes.extend_from_slice(RECEIPT_SIGNING_DOMAIN);
    bytes.extend_from_slice(body.as_bytes());
    Ok(CanonicalBytes::from_raw(bytes))
}

/// 用固定种子构造测试/调用方签名密钥。生产路径由身份层托管密钥。
pub fn signing_key_from_seed(seed: &[u8; 32]) -> SigningKey {
    SigningKey::from_bytes(seed)
}

/// 对签名原文签名，返回 64 字节签名。
pub fn sign_bytes(signing_key: &SigningKey, message: &[u8]) -> [u8; 64] {
    signing_key.sign(message).to_bytes()
}

/// PROTOCOL-004 的验签原语：严格验证 Ed25519 签名。
///
/// 这不是完整的 `verify_candidate`：凭据/设备绑定、群上下文与 MLS 内层验证
/// 由上层函数负责。
pub fn verify_ed25519(public_key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> Result<()> {
    let key = VerifyingKey::from_bytes(public_key).map_err(|_| ProtocolError::InvalidSignature)?;
    let signature = Signature::from_bytes(signature);
    key.verify_strict(message, &signature)
        .map_err(|_| ProtocolError::InvalidSignature)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate() -> UnsignedCandidate {
        UnsignedCandidate {
            protocol_version: ProtocolVersion::V1,
            submission_id: SubmissionId::from_bytes([0x11; 32]),
            group_id: GroupId::from_bytes([0x22; 32]),
            author_device_id: DeviceId::from_bytes([0x33; 32]),
            kind: CandidateKind::Control,
            expected_control_head: ControlHead::from_bytes([0x44; 32]),
            policy_version: 7,
            mls_epoch: 9,
            encrypted_payload: b"ciphertext".to_vec(),
            control_metadata: Some(ControlMetadata {
                operation: ControlOperation::Invite,
                target_digest: Digest::from_bytes([0x55; 32]),
                domain_operation: None,
            }),
        }
    }

    fn base_signature() -> ([u8; 32], [u8; 64]) {
        let signing_key = signing_key_from_seed(&[0x42; 32]);
        let public_key = signing_key.verifying_key().to_bytes();
        let bytes = candidate_signing_bytes(&candidate()).expect("base candidate must encode");
        let signature = sign_bytes(&signing_key, bytes.as_bytes());
        (public_key, signature)
    }

    /// 用「原候选」的签名去验证改过的候选：任一被签字段变化都必须失败。
    fn still_verifies(
        candidate: &UnsignedCandidate,
        public_key: &[u8; 32],
        signature: &[u8; 64],
    ) -> bool {
        match candidate_signing_bytes(candidate) {
            Ok(bytes) => verify_ed25519(public_key, bytes.as_bytes(), signature).is_ok(),
            // 候选自身已不合法（例如应用候选夹带控制元数据），无法重放原签名。
            Err(_) => false,
        }
    }

    #[test]
    fn every_signed_field_change_breaks_verification() {
        let (public_key, signature) = base_signature();
        assert!(still_verifies(&candidate(), &public_key, &signature));
        type Mutation = Box<dyn Fn(&mut UnsignedCandidate)>;
        let mut mutations: Vec<Mutation> = vec![
            Box::new(|c| c.group_id = GroupId::from_bytes([0x99; 32])),
            Box::new(|c| c.author_device_id = DeviceId::from_bytes([0x99; 32])),
            Box::new(|c| c.submission_id = SubmissionId::from_bytes([0x99; 32])),
            Box::new(|c| c.expected_control_head = ControlHead::from_bytes([0x99; 32])),
            Box::new(|c| c.policy_version += 1),
            Box::new(|c| c.mls_epoch += 1),
            Box::new(|c| c.encrypted_payload.push(0)),
            Box::new(|c| c.kind = CandidateKind::Application),
            Box::new(|c| {
                c.control_metadata = Some(ControlMetadata {
                    operation: ControlOperation::Remove,
                    target_digest: Digest::from_bytes([0x55; 32]),
                    domain_operation: None,
                })
            }),
            Box::new(|c| {
                c.control_metadata = Some(ControlMetadata {
                    operation: ControlOperation::Invite,
                    target_digest: Digest::from_bytes([0x99; 32]),
                    domain_operation: None,
                })
            }),
        ];
        for mutate in mutations.iter_mut() {
            let mut changed = candidate();
            mutate(&mut changed);
            assert!(
                !still_verifies(&changed, &public_key, &signature),
                "mutation must invalidate the author signature"
            );
        }
    }

    #[test]
    fn application_candidate_cannot_carry_control_metadata() {
        let mut candidate = candidate();
        candidate.kind = CandidateKind::Application;
        assert_eq!(
            candidate_signing_bytes(&candidate).unwrap_err(),
            ProtocolError::InvalidCandidate
        );
        let mut candidate = self::candidate();
        candidate.control_metadata = None;
        assert_eq!(
            candidate_signing_bytes(&candidate).unwrap_err(),
            ProtocolError::InvalidCandidate
        );
    }

    #[test]
    fn empty_and_oversized_ciphertext_are_rejected() {
        let mut candidate = candidate();
        candidate.encrypted_payload.clear();
        assert_eq!(
            candidate_signing_bytes(&candidate).unwrap_err().code(),
            crate::error::ErrorCode::ContentTooLarge
        );
        let mut candidate = self::candidate();
        candidate.encrypted_payload = vec![0; MAX_CIPHERTEXT_BYTES + 1];
        assert_eq!(
            candidate_signing_bytes(&candidate).unwrap_err().code(),
            crate::error::ErrorCode::ContentTooLarge
        );
    }

    #[test]
    fn receipt_changes_each_binding() {
        let candidate_hash = Digest::from_bytes([1; 32]);
        let previous = Digest::from_bytes([2; 32]);
        let group = GroupId::from_bytes([3; 32]);
        let base = receipt_signing_bytes(candidate_hash, 1, previous, group).unwrap();
        assert_ne!(
            base.as_bytes(),
            receipt_signing_bytes(candidate_hash, 2, previous, group)
                .unwrap()
                .as_bytes()
        );
        assert_ne!(
            base.as_bytes(),
            receipt_signing_bytes(Digest::from_bytes([9; 32]), 1, previous, group)
                .unwrap()
                .as_bytes()
        );
        assert_ne!(
            base.as_bytes(),
            receipt_signing_bytes(candidate_hash, 1, Digest::from_bytes([9; 32]), group)
                .unwrap()
                .as_bytes()
        );
        assert_ne!(
            base.as_bytes(),
            receipt_signing_bytes(candidate_hash, 1, previous, GroupId::from_bytes([9; 32]))
                .unwrap()
                .as_bytes()
        );
        // 域分隔：候选原文与回执原文绝不相同。
        assert_ne!(
            candidate_signing_bytes(&candidate()).unwrap().as_bytes(),
            base.as_bytes()
        );
        assert_eq!(
            receipt_signing_bytes(candidate_hash, 0, previous, group).unwrap_err(),
            ProtocolError::InvalidSequence
        );
    }

    #[test]
    fn tampered_signature_is_rejected() {
        let seed = [7; 32];
        let signing_key = signing_key_from_seed(&seed);
        let public_key = signing_key.verifying_key().to_bytes();
        let bytes = candidate_signing_bytes(&candidate()).unwrap();
        let signature = sign_bytes(&signing_key, bytes.as_bytes());
        assert!(verify_ed25519(&public_key, bytes.as_bytes(), &signature).is_ok());
        let mut tampered = signature;
        tampered[0] ^= 0xff;
        assert_eq!(
            verify_ed25519(&public_key, bytes.as_bytes(), &tampered).unwrap_err(),
            ProtocolError::InvalidSignature
        );
    }

    fn declared_candidate() -> UnsignedCandidate {
        let mut candidate = self::candidate();
        candidate.control_metadata = Some(
            ControlMetadata::new(ControlOperation::SetRole, Digest::from_bytes([0x55; 32]))
                .with_domain_operation(DomainOperationDeclaration {
                    operation: DomainOperation::TransferOwnership,
                    target_digest: Digest::from_bytes([0x66; 32]),
                }),
        );
        candidate
    }

    fn candidate_body_fields(candidate: &UnsignedCandidate) -> WireValue {
        let bytes = candidate_signing_bytes(candidate).expect("candidate must encode");
        let body = &bytes.as_bytes()[CANDIDATE_SIGNING_DOMAIN.len()..];
        crate::decode::decode_bounded(body, crate::decode::DecodeLimits::DEFAULT)
            .expect("body must decode")
            .value()
            .clone()
    }

    #[test]
    fn domain_declaration_is_signed_and_any_change_breaks_verification() {
        let key = signing_key_from_seed(&[0x42; 32]);
        let public_key = key.verifying_key().to_bytes();
        let bytes = candidate_signing_bytes(&declared_candidate()).expect("declared candidate");
        let signature = sign_bytes(&key, bytes.as_bytes());
        assert!(verify_ed25519(&public_key, bytes.as_bytes(), &signature).is_ok());
        // 声明字段任一变化都必须让原签名失效。
        let mut changed = declared_candidate();
        if let Some(metadata) = changed.control_metadata.as_mut() {
            metadata.domain_operation = Some(DomainOperationDeclaration {
                operation: DomainOperation::SetRole,
                target_digest: Digest::from_bytes([0x66; 32]),
            });
        }
        assert!(!still_verifies(&changed, &public_key, &signature));
        let mut changed = declared_candidate();
        if let Some(metadata) = changed.control_metadata.as_mut() {
            metadata.domain_operation = Some(DomainOperationDeclaration {
                operation: DomainOperation::TransferOwnership,
                target_digest: Digest::from_bytes([0x67; 32]),
            });
        }
        assert!(!still_verifies(&changed, &public_key, &signature));
        let mut changed = declared_candidate();
        if let Some(metadata) = changed.control_metadata.as_mut() {
            metadata.domain_operation = None;
        }
        assert!(!still_verifies(&changed, &public_key, &signature));
    }

    #[test]
    fn domain_declaration_requires_control_kind_and_compatible_operation() {
        // 应用候选携带声明 → 拒绝。
        let mut application = declared_candidate();
        application.kind = CandidateKind::Application;
        assert_eq!(
            candidate_signing_bytes(&application).unwrap_err(),
            ProtocolError::InvalidCandidate
        );
        // 公开操作与领域声明不匹配 → 拒绝。
        for (public, domain) in [
            (ControlOperation::Rename, DomainOperation::TransferOwnership),
            (ControlOperation::Remove, DomainOperation::LeaveSelf),
            (ControlOperation::SetRole, DomainOperation::Dissolve),
            (ControlOperation::Invite, DomainOperation::SetRole),
        ] {
            let mut candidate = self::candidate();
            candidate.control_metadata = Some(
                ControlMetadata::new(public, Digest::from_bytes([0x55; 32])).with_domain_operation(
                    DomainOperationDeclaration {
                        operation: domain,
                        target_digest: Digest::from_bytes([0x66; 32]),
                    },
                ),
            );
            assert_eq!(
                candidate_signing_bytes(&candidate).unwrap_err(),
                ProtocolError::InvalidCandidate,
                "public {public:?} + domain {domain:?} must be rejected"
            );
        }
        // 合法组合。
        for (public, domain) in [
            (ControlOperation::SetRole, DomainOperation::SetRole),
            (
                ControlOperation::SetRole,
                DomainOperation::TransferOwnership,
            ),
            (
                ControlOperation::TransferOwnership,
                DomainOperation::TransferOwnership,
            ),
            (ControlOperation::Leave, DomainOperation::LeaveSelf),
            (ControlOperation::Dissolve, DomainOperation::Dissolve),
        ] {
            let mut candidate = self::candidate();
            candidate.control_metadata = Some(
                ControlMetadata::new(public, Digest::from_bytes([0x55; 32])).with_domain_operation(
                    DomainOperationDeclaration {
                        operation: domain,
                        target_digest: Digest::from_bytes([0x66; 32]),
                    },
                ),
            );
            assert!(
                candidate_signing_bytes(&candidate).is_ok(),
                "public {public:?} + domain {domain:?} must be accepted"
            );
        }
        // 全零目标摘要视为缺目标摘要。
        let mut candidate = self::candidate();
        candidate.control_metadata = Some(
            ControlMetadata::new(ControlOperation::Leave, Digest::from_bytes([0x55; 32]))
                .with_domain_operation(DomainOperationDeclaration {
                    operation: DomainOperation::LeaveSelf,
                    target_digest: Digest::ZERO,
                }),
        );
        assert_eq!(
            candidate_signing_bytes(&candidate).unwrap_err(),
            ProtocolError::InvalidCandidate
        );
    }

    #[test]
    fn domain_declaration_is_omitted_for_old_bytes_and_decoded_from_new_bytes() {
        // 旧候选：body 不含新字段号 → 解码为 None，且字节与既有向量一致。
        let old_body = candidate_body_fields(&candidate());
        let WireValue::Struct(fields) = &old_body else {
            panic!("candidate body must be a struct");
        };
        assert!(
            fields
                .iter()
                .all(|f| f.id != field::CONTROL_DOMAIN_OPERATION
                    && f.id != field::CONTROL_DOMAIN_TARGET_DIGEST)
        );
        assert_eq!(decode_domain_operation(&old_body).unwrap(), None);
        // 新候选：字段成对出现并解回同一声明。
        let declared = declared_candidate();
        let new_body = candidate_body_fields(&declared);
        assert_eq!(
            decode_domain_operation(&new_body).unwrap(),
            declared
                .control_metadata
                .and_then(|metadata| metadata.domain_operation)
        );
        // 只出现一个字段 → 拒绝。
        for partial in [
            vec![WireField::new(
                field::CONTROL_DOMAIN_OPERATION,
                WireValue::U64(2),
            )],
            vec![WireField::new(
                field::CONTROL_DOMAIN_TARGET_DIGEST,
                WireValue::digest(Digest::from_bytes([7; 32])),
            )],
        ] {
            assert_eq!(
                decode_domain_operation(&WireValue::Struct(partial)).unwrap_err(),
                ProtocolError::InvalidCandidate
            );
        }
        // 未知枚举值、错误摘要长度、全零摘要、非结构体 → 全部拒绝。
        for invalid in [
            WireValue::Struct(vec![
                WireField::new(field::CONTROL_DOMAIN_OPERATION, WireValue::U64(9)),
                WireField::new(
                    field::CONTROL_DOMAIN_TARGET_DIGEST,
                    WireValue::digest(Digest::from_bytes([7; 32])),
                ),
            ]),
            WireValue::Struct(vec![
                WireField::new(field::CONTROL_DOMAIN_OPERATION, WireValue::U64(2)),
                WireField::new(
                    field::CONTROL_DOMAIN_TARGET_DIGEST,
                    WireValue::Bytes(vec![7; 31]),
                ),
            ]),
            WireValue::Struct(vec![
                WireField::new(field::CONTROL_DOMAIN_OPERATION, WireValue::U64(2)),
                WireField::new(
                    field::CONTROL_DOMAIN_TARGET_DIGEST,
                    WireValue::Bytes(vec![0; 32]),
                ),
            ]),
            WireValue::U64(1),
        ] {
            assert_eq!(
                decode_domain_operation(&invalid).unwrap_err(),
                ProtocolError::InvalidCandidate
            );
        }
        // 新字段号已登记为合法字段。
        assert!(crate::canonical::is_known_field(
            field::CONTROL_DOMAIN_OPERATION
        ));
        assert!(crate::canonical::is_known_field(
            field::CONTROL_DOMAIN_TARGET_DIGEST
        ));
        assert_eq!(DomainOperation::from_u64(5), None);
        assert_eq!(
            DomainOperation::from_u64(DomainOperation::LeaveSelf.as_u64()),
            Some(DomainOperation::LeaveSelf)
        );
    }
}
