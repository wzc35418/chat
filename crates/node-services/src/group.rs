//! NODE-005~008：群登记、排序日志追加、日志分页与节点签名 head。
//!
//! 事务边界（TRANSACTIONS.md T4）：群登记（groups/group_members/group_control_heads/
//! group_registrations）与日志追加（group_log/控制头/groups.last_seq+last_hash）
//! 各自是单个 IMMEDIATE 事务；崩溃只会回滚整个事务，不会留下半条日志。
//! 节点不读 MLS 明文，只做公开元数据校验；安全校验失败一律显式返回错误。
use chat_protocol::{
    CandidateKind, ControlHead, ControlMetadata, ControlOperation, Digest, DomainOperation,
    DomainOperationDeclaration, GroupId, DeviceId, ProtocolVersion, SubmissionId, UnsignedCandidate,
    candidate_signing_bytes, receipt_signing_bytes,
};
use ed25519_dalek::{Signature, Signer, SigningKey};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::canonical::{
    GROUP_CREATION_DOMAIN, GROUP_REGISTRATION_DOMAIN, LOG_CHAIN_DOMAIN, LOG_HEAD_DOMAIN, bytes32,
    decode_hex_bytes, hex_decode, hex_encode, push_field, sha256, sha256_concat,
    validate_bounded_id, validate_timestamp, verify_ed25519,
};
use crate::error::{NodeError, NodeResult};
use crate::limits::{
    MAX_ACCOUNT_ID_BYTES, MAX_GROUP_ID_BYTES, MAX_GROUP_ROLE_BYTES,
};
use crate::services::{AuthenticatedDevice, NodeService, to_i64};

/// 空日志 head 的初始摘要（与任何真实条目摘要都不相同）。
pub const ZERO_HASH: [u8; 32] = [0_u8; 32];
/// 候选类别标签。
pub const KIND_APPLICATION: &str = "application";
pub const KIND_CONTROL: &str = "control";
/// 成员角色标签。
pub const ROLE_OWNER: &str = "owner";
pub const ROLE_ADMIN: &str = "admin";
pub const ROLE_MEMBER: &str = "member";

/// 崩溃/审计检查点；仅供崩溃实验与测试注入，不改变生产语义。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppendCheckpoint {
    /// 日志行已写入事务但尚未提交。
    AfterLogInsert,
    /// 日志、控制头与 head 都已写入，提交之前。
    BeforeCommit,
    /// 已提交（用于“提交后响应丢失”）。
    AfterCommit,
}

/// 群成员声明（创建时固定）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupMemberSpec {
    pub device_id: String,
    pub role: String,
}

impl GroupMemberSpec {
    fn validate(&self) -> NodeResult<()> {
        validate_bounded_id("member device id", &self.device_id, MAX_GROUP_ID_BYTES)?;
        DeviceId::parse(&self.device_id).map_err(|_| NodeError::InvalidRequest("member device id"))?;
        if self.role.len() > MAX_GROUP_ROLE_BYTES {
            return Err(NodeError::InvalidRequest("member role"));
        }
        match self.role.as_str() {
            ROLE_OWNER | ROLE_ADMIN | ROLE_MEMBER => Ok(()),
            _ => Err(NodeError::InvalidRequest("member role")),
        }
    }
}

/// 群创建请求（创建设备用私钥签名）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupCreation {
    pub group_id: String,
    pub owner_account_id: String,
    /// 初始正式控制头摘要（小写十六进制 32 字节）。
    pub initial_control_head: String,
    pub policy_version: u64,
    pub mls_epoch: u64,
    pub members: Vec<GroupMemberSpec>,
}

impl GroupCreation {
    /// 形状与容量校验；不检查签名（那是 verify 的职责）。
    pub fn validate_shape(&self, max_members: usize) -> NodeResult<()> {
        validate_bounded_id("group id", &self.group_id, MAX_GROUP_ID_BYTES)?;
        GroupId::parse(&self.group_id).map_err(|_| NodeError::InvalidRequest("group id"))?;
        validate_bounded_id("owner account id", &self.owner_account_id, MAX_ACCOUNT_ID_BYTES)?;
        hex_decode::<32>("initial control head", &self.initial_control_head)?;
        validate_timestamp("policy version", self.policy_version)?;
        validate_timestamp("mls epoch", self.mls_epoch)?;
        if self.members.is_empty() {
            return Err(NodeError::InvalidRequest("group members"));
        }
        if self.members.len() > max_members {
            return Err(NodeError::GroupCapacityExceeded);
        }
        for member in &self.members {
            member.validate()?;
        }
        // 成员设备必须唯一，且恰好一个群主。
        let mut seen: Vec<&str> = Vec::with_capacity(self.members.len());
        let mut owners = 0_usize;
        for member in &self.members {
            if seen.contains(&member.device_id.as_str()) {
                return Err(NodeError::InvalidRequest("duplicate group member"));
            }
            seen.push(&member.device_id);
            if member.role == ROLE_OWNER {
                owners += 1;
            }
        }
        if owners != 1 {
            return Err(NodeError::InvalidRequest("group must have exactly one owner"));
        }
        Ok(())
    }

    /// 创建请求签名原文（域分隔 + 长度前缀；成员顺序参与签名）。
    pub fn signing_bytes(&self) -> NodeResult<Vec<u8>> {
        let control_head = hex_decode::<32>("initial control head", &self.initial_control_head)?;
        let mut bytes = Vec::with_capacity(256);
        bytes.extend_from_slice(GROUP_CREATION_DOMAIN);
        push_field(&mut bytes, self.group_id.as_bytes());
        push_field(&mut bytes, self.owner_account_id.as_bytes());
        bytes.extend_from_slice(&control_head);
        bytes.extend_from_slice(&self.policy_version.to_be_bytes());
        bytes.extend_from_slice(&self.mls_epoch.to_be_bytes());
        bytes.extend_from_slice(&(self.members.len() as u32).to_be_bytes());
        for member in &self.members {
            push_field(&mut bytes, member.device_id.as_bytes());
            push_field(&mut bytes, member.role.as_bytes());
        }
        Ok(bytes)
    }
}

/// 设备签名的群创建请求。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedGroupCreation {
    pub creation: GroupCreation,
    /// 创建设备对创建原文的 Ed25519 签名（小写十六进制 64 字节）。
    pub device_signature: String,
}

impl SignedGroupCreation {
    /// 用创建设备公钥严格验签。
    pub fn verify(&self, device_public_key: &[u8; 32]) -> NodeResult<()> {
        let signature = hex_decode::<64>("group creation signature", &self.device_signature)?;
        verify_ed25519(device_public_key, &self.creation.signing_bytes()?, &signature, || {
            NodeError::InvalidSignature
        })
    }

    /// 创建内容摘要（含签名）：用于同 GroupId 的幂等/冲突判定。
    pub fn digest(&self) -> NodeResult<[u8; 32]> {
        let signature = hex_decode::<64>("group creation signature", &self.device_signature)?;
        let bytes = self.creation.signing_bytes()?;
        Ok(sha256_concat(&[&bytes, &signature]))
    }
}

/// 节点签发的群登记回执；同 ID 同摘要重试返回同一份持久回执。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupRegistration {
    pub group_id: String,
    pub owner_account_id: String,
    pub coordinator_id: String,
    pub initial_control_head: String,
    pub policy_version: u64,
    pub mls_epoch: u64,
    pub creation_hash: String,
    pub members: Vec<GroupMemberSpec>,
    pub registered_at: u64,
    pub node_id: String,
    pub node_public_key: String,
    pub signature: String,
}

impl GroupRegistration {
    /// 回执签名原文。
    pub fn signing_bytes(&self) -> NodeResult<Vec<u8>> {
        let node_public_key = hex_decode::<32>("node public key", &self.node_public_key)?;
        let control_head = hex_decode::<32>("initial control head", &self.initial_control_head)?;
        let creation_hash = hex_decode::<32>("creation hash", &self.creation_hash)?;
        validate_timestamp("registered at", self.registered_at)?;
        let mut bytes = Vec::with_capacity(256);
        bytes.extend_from_slice(GROUP_REGISTRATION_DOMAIN);
        push_field(&mut bytes, self.node_id.as_bytes());
        bytes.extend_from_slice(&node_public_key);
        push_field(&mut bytes, self.group_id.as_bytes());
        push_field(&mut bytes, self.coordinator_id.as_bytes());
        push_field(&mut bytes, self.owner_account_id.as_bytes());
        bytes.extend_from_slice(&creation_hash);
        bytes.extend_from_slice(&control_head);
        bytes.extend_from_slice(&self.policy_version.to_be_bytes());
        bytes.extend_from_slice(&self.mls_epoch.to_be_bytes());
        bytes.extend_from_slice(&self.registered_at.to_be_bytes());
        bytes.extend_from_slice(&(self.members.len() as u32).to_be_bytes());
        for member in &self.members {
            push_field(&mut bytes, member.device_id.as_bytes());
            push_field(&mut bytes, member.role.as_bytes());
        }
        Ok(bytes)
    }

    /// 用固定节点公钥验证回执。
    pub fn verify(&self, expected_node_public_key: &[u8; 32]) -> NodeResult<()> {
        let node_public_key = hex_decode::<32>("node public key", &self.node_public_key)?;
        if &node_public_key != expected_node_public_key {
            return Err(NodeError::NodeIdentityMismatch);
        }
        let signature = hex_decode::<64>("group registration signature", &self.signature)?;
        verify_ed25519(
            expected_node_public_key,
            &self.signing_bytes()?,
            &signature,
            || NodeError::InvalidSignature,
        )
    }
}

/// 候选附带的公开控制元数据（不含群名等正文）。
///
/// 刻意保持**两个字段**：扩散的传输结构（node-api / core / 测试）都按字面量构造它，
/// 加字段会撕裂不在本包写作用域的调用点。领域操作声明因此走**传输层的可选字段**
/// `CandidateDomainOperation`（见下），最终仍被折叠进 PROTOCOL-004 的字段 55/56 参与签名。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateControlMetadata {
    /// invite/remove/set_role/set_muted/rename/transfer_ownership/leave/dissolve。
    pub operation: String,
    pub target_digest: String,
    /// PROTOCOL-004 字段 59：邀请新增的成员凭据（UTF-8，≤128 字节/条，≤64 条）。
    #[serde(default, alias = "addedCredentials")]
    pub added_credentials: Vec<String>,
    /// PROTOCOL-004 字段 60：被移除的成员凭据。
    #[serde(default, alias = "removedCredentials")]
    pub removed_credentials: Vec<String>,
}

/// 领域操作声明（PROTOCOL-004 字段 55/56 的**传输层**形态）。
///
/// 它不在 `control_metadata` 里，而是作为候选请求的可选字段传入；节点会把它折叠进
/// `ControlMetadata.domain_operation` 再算签名原文与候选摘要——因此“只改声明不改签名”会验签失败。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateDomainOperation {
    /// 领域操作文本：set_role / transfer_ownership / leave_self / dissolve（白名单）。
    #[serde(alias = "domainOperation")]
    pub operation: String,
    /// 目标摘要（非零 32 字节 hex）。
    #[serde(alias = "domainTargetDigest")]
    pub target_digest: String,
}

impl CandidateDomainOperation {
    /// 解析成协议枚举 + 目标摘要；未知文本/非法 hex/全零都显式 `INVALID_CANDIDATE`。
    pub fn to_declaration(&self) -> NodeResult<DomainOperationDeclaration> {
        let operation = domain_operation(&self.operation)?;
        // 长度/非法 hex 与“未知枚举”同属声明不合法 → 统一 INVALID_CANDIDATE。
        let target_digest = Digest::from_bytes(
            hex_decode::<32>("domain target digest", &self.target_digest)
                .map_err(|_| NodeError::InvalidCandidate)?,
        );
        if target_digest == Digest::ZERO {
            return Err(NodeError::InvalidCandidate);
        }
        Ok(DomainOperationDeclaration {
            operation,
            target_digest,
        })
    }
}

/// 领域操作文本 → 协议枚举（白名单；未知文本显式 INVALID_CANDIDATE）。
fn domain_operation(value: &str) -> NodeResult<DomainOperation> {
    match value {
        "set_role" => Ok(DomainOperation::SetRole),
        "transfer_ownership" => Ok(DomainOperation::TransferOwnership),
        "leave_self" => Ok(DomainOperation::LeaveSelf),
        "dissolve" => Ok(DomainOperation::Dissolve),
        _ => Err(NodeError::InvalidCandidate),
    }
}

/// 协议枚举 → 节点侧稳定文本（写入日志元数据投影）。
pub fn domain_operation_text(operation: DomainOperation) -> &'static str {
    match operation {
        DomainOperation::SetRole => "set_role",
        DomainOperation::TransferOwnership => "transfer_ownership",
        DomainOperation::LeaveSelf => "leave_self",
        DomainOperation::Dissolve => "dissolve",
    }
}

/// 领域声明与公开控制操作的合法组合（与 PROTOCOL-004 `validate_domain_declaration` 同规则）。
fn domain_pairing_is_valid(operation: ControlOperation, domain: DomainOperation) -> bool {
    match domain {
        DomainOperation::SetRole => operation == ControlOperation::SetRole,
        // MLS 把转让表达为 SelfUpdate（公开层 set_role）；直连路径也可能直接给 transfer。
        DomainOperation::TransferOwnership => {
            matches!(
                operation,
                ControlOperation::SetRole | ControlOperation::TransferOwnership
            )
        }
        DomainOperation::LeaveSelf => operation == ControlOperation::Leave,
        DomainOperation::Dissolve => operation == ControlOperation::Dissolve,
    }
}

/// 传输层候选字段（文本形式，具体规范编码由 PROTOCOL-004 冻结）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupCandidate {
    pub protocol_major: u8,
    pub protocol_minor: u8,
    pub submission_id: String,
    pub group_id: String,
    pub author_device_id: String,
    pub kind: String,
    pub expected_control_head: String,
    pub policy_version: u64,
    pub mls_epoch: u64,
    /// 客户端加密后的密文（小写十六进制），节点不解析明文。
    pub encrypted_payload: String,
    pub control_metadata: Option<CandidateControlMetadata>,
}

fn control_operation(value: &str) -> NodeResult<ControlOperation> {
    match value {
        "invite" => Ok(ControlOperation::Invite),
        "remove" => Ok(ControlOperation::Remove),
        "set_role" => Ok(ControlOperation::SetRole),
        "set_muted" => Ok(ControlOperation::SetMuted),
        "rename" => Ok(ControlOperation::Rename),
        "transfer_ownership" => Ok(ControlOperation::TransferOwnership),
        "leave" => Ok(ControlOperation::Leave),
        "dissolve" => Ok(ControlOperation::Dissolve),
        _ => Err(NodeError::InvalidRequest("control operation")),
    }
}

impl GroupCandidate {
    /// 转换到协议层候选（不含领域操作声明；旧客户端字节完全不变）。
    pub fn to_unsigned(&self, max_ciphertext: usize) -> NodeResult<UnsignedCandidate> {
        self.to_unsigned_with_domain(max_ciphertext, None)
    }

    /// 转换到协议层候选，并把可选的领域操作声明折叠进 PROTOCOL-004 的字段 55/56。
    ///
    /// 声明与公开 `control_metadata` 的组合必须合法（非法组合 → `INVALID_CANDIDATE`）；
    /// 声明只能出现在 `kind = control` 的候选上。旧调用方传 `None` 时与旧行为逐字节一致。
    pub fn to_unsigned_with_domain(
        &self,
        max_ciphertext: usize,
        domain: Option<&CandidateDomainOperation>,
    ) -> NodeResult<UnsignedCandidate> {
        let kind = match self.kind.as_str() {
            KIND_APPLICATION => CandidateKind::Application,
            KIND_CONTROL => CandidateKind::Control,
            _ => return Err(NodeError::InvalidRequest("candidate kind")),
        };
        validate_timestamp("policy version", self.policy_version)?;
        validate_timestamp("mls epoch", self.mls_epoch)?;
        let control_metadata = match &self.control_metadata {
            Some(metadata) => {
                let operation = control_operation(&metadata.operation)?;
                let mut resolved = ControlMetadata::new(
                    operation,
                    Digest::from_bytes(hex_decode::<32>(
                        "control target digest",
                        &metadata.target_digest,
                    )?),
                );
                if let Some(declaration) = domain {
                    let declaration = declaration.to_declaration()?;
                    if !domain_pairing_is_valid(operation, declaration.operation) {
                        return Err(NodeError::InvalidCandidate);
                    }
                    resolved = resolved.with_domain_operation(declaration);
                }
                if !metadata.added_credentials.is_empty() || !metadata.removed_credentials.is_empty() {
                    resolved = resolved.with_credentials(
                        metadata.added_credentials.clone(),
                        metadata.removed_credentials.clone(),
                    );
                }
                Some(resolved)
            }
            None => {
                // 应用候选不携带控制元数据，也就不可能声明领域操作。
                if domain.is_some() {
                    return Err(NodeError::InvalidCandidate);
                }
                None
            }
        };
        Ok(UnsignedCandidate {
            protocol_version: ProtocolVersion {
                major: self.protocol_major,
                minor: self.protocol_minor,
            },
            submission_id: SubmissionId::parse(&self.submission_id)
                .map_err(|_| NodeError::InvalidRequest("submission id"))?,
            group_id: GroupId::parse(&self.group_id)
                .map_err(|_| NodeError::InvalidRequest("candidate group id"))?,
            author_device_id: DeviceId::parse(&self.author_device_id)
                .map_err(|_| NodeError::InvalidRequest("author device id"))?,
            kind,
            expected_control_head: ControlHead::from_bytes(hex_decode::<32>(
                "expected control head",
                &self.expected_control_head,
            )?),
            policy_version: self.policy_version,
            mls_epoch: self.mls_epoch,
            encrypted_payload: decode_hex_bytes(
                "candidate payload",
                &self.encrypted_payload,
                max_ciphertext,
            )?,
            control_metadata,
        })
    }
}

impl SignedCandidate {
    /// 节点日志里的领域操作投影（标准化文本 + 规范化目标摘要 hex）；未声明返回 `None`。
    ///
    /// 组合合法性已在 `GroupCandidate::to_unsigned_with_domain` 校验；这里再核一次
    /// （投影写进日志列，宁可重复校验也不写一条与公开操作不符的语义）。
    pub fn domain_projection(
        &self,
        domain: Option<&CandidateDomainOperation>,
    ) -> NodeResult<Option<(String, String)>> {
        let Some(domain) = domain else {
            return Ok(None);
        };
        let declaration = domain.to_declaration()?;
        let metadata = self
            .candidate
            .control_metadata
            .as_ref()
            .ok_or(NodeError::InvalidCandidate)?;
        let operation = control_operation(&metadata.operation)?;
        if !domain_pairing_is_valid(operation, declaration.operation) {
            return Err(NodeError::InvalidCandidate);
        }
        Ok(Some((
            domain_operation_text(declaration.operation).to_owned(),
            hex_encode(declaration.target_digest.as_bytes()),
        )))
    }
}

/// 作者签名的候选（NODE-006 的输入）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedCandidate {
    pub candidate: GroupCandidate,
    /// 作者设备对 PROTOCOL-004 签名原文的 Ed25519 签名（小写十六进制 64 字节）。
    pub author_signature: String,
}

impl SignedCandidate {
    /// 候选全文摘要（= 节点回执与日志链的 candidate_hash）。
    pub fn candidate_hash(&self, max_ciphertext: usize) -> NodeResult<[u8; 32]> {
        let unsigned = self.candidate.to_unsigned(max_ciphertext)?;
        let bytes = candidate_signing_bytes(&unsigned).map_err(|_| NodeError::InvalidRequest("candidate"))?;
        Ok(sha256(bytes.as_bytes()))
    }

    /// 用作者设备公钥验外层签名。
    pub fn verify_author(&self, author_public_key: &[u8; 32], max_ciphertext: usize) -> NodeResult<()> {
        let unsigned = self.candidate.to_unsigned(max_ciphertext)?;
        let bytes = candidate_signing_bytes(&unsigned).map_err(|_| NodeError::InvalidRequest("candidate"))?;
        let signature = hex_decode::<64>("author signature", &self.author_signature)?;
        verify_ed25519(author_public_key, bytes.as_bytes(), &signature, || {
            NodeError::InvalidSignature
        })
    }
}

/// 节点给作者的唯一序号与签名回执。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppendResult {
    pub group_id: String,
    pub submission_id: String,
    pub author_device_id: String,
    pub seq: u64,
    pub candidate_hash: String,
    pub previous_hash: String,
    pub log_hash: String,
    pub accepted_at: u64,
    pub node_id: String,
    pub node_public_key: String,
    pub signature: String,
}

/// 日志链摘要：H(域 || group || seq || previous_hash || candidate_hash)。
pub fn log_chain_hash(
    group_id: &str,
    seq: u64,
    previous_hash: &[u8; 32],
    candidate_hash: &[u8; 32],
) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(LOG_CHAIN_DOMAIN.len() + group_id.len() + 80);
    bytes.extend_from_slice(LOG_CHAIN_DOMAIN);
    push_field(&mut bytes, group_id.as_bytes());
    bytes.extend_from_slice(&seq.to_be_bytes());
    bytes.extend_from_slice(previous_hash);
    bytes.extend_from_slice(candidate_hash);
    sha256(&bytes)
}

impl AppendResult {
    /// 用固定节点公钥验证回执，并复核日志链摘要与前序摘要一致。
    pub fn verify(&self, expected_node_public_key: &[u8; 32]) -> NodeResult<()> {
        let node_public_key = hex_decode::<32>("node public key", &self.node_public_key)?;
        if &node_public_key != expected_node_public_key {
            return Err(NodeError::NodeIdentityMismatch);
        }
        if self.seq == 0 {
            return Err(NodeError::InvalidSignature);
        }
        let candidate_hash = hex_decode::<32>("candidate hash", &self.candidate_hash)?;
        let previous_hash = hex_decode::<32>("previous hash", &self.previous_hash)?;
        let log_hash = hex_decode::<32>("log hash", &self.log_hash)?;
        if log_chain_hash(&self.group_id, self.seq, &previous_hash, &candidate_hash) != log_hash {
            return Err(NodeError::InvalidSignature);
        }
        let group = GroupId::parse(&self.group_id).map_err(|_| NodeError::InvalidSignature)?;
        let bytes = receipt_signing_bytes(
            Digest::from_bytes(candidate_hash),
            self.seq,
            Digest::from_bytes(previous_hash),
            group,
        )
        .map_err(|_| NodeError::InvalidSignature)?;
        let signature = hex_decode::<64>("receipt signature", &self.signature)?;
        verify_ed25519(expected_node_public_key, bytes.as_bytes(), &signature, || {
            NodeError::InvalidSignature
        })
    }

    /// 复核回执确实绑定到这份候选（客户端用它确认“同密文得到同结果”）。
    pub fn verify_candidate(&self, candidate: &SignedCandidate, max_ciphertext: usize) -> NodeResult<()> {
        if self.submission_id != candidate.candidate.submission_id
            || self.group_id != candidate.candidate.group_id
            || self.author_device_id != candidate.candidate.author_device_id
        {
            return Err(NodeError::InvalidSignature);
        }
        if self.candidate_hash != hex_encode(&candidate.candidate_hash(max_ciphertext)?) {
            return Err(NodeError::InvalidSignature);
        }
        Ok(())
    }
}

/// 节点签名的日志 head；与条目回执使用不同签名域。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedLogHead {
    pub group_id: String,
    pub coordinator_id: String,
    pub seq: u64,
    pub log_hash: String,
    pub control_head: String,
    pub policy_version: u64,
    pub mls_epoch: u64,
    pub node_id: String,
    pub node_public_key: String,
    pub signature: String,
}

impl SignedLogHead {
    /// head 签名原文。
    pub fn signing_bytes(&self) -> NodeResult<Vec<u8>> {
        let node_public_key = hex_decode::<32>("node public key", &self.node_public_key)?;
        let log_hash = hex_decode::<32>("log hash", &self.log_hash)?;
        let control_head = hex_decode::<32>("control head", &self.control_head)?;
        let mut bytes = Vec::with_capacity(256);
        bytes.extend_from_slice(LOG_HEAD_DOMAIN);
        push_field(&mut bytes, self.node_id.as_bytes());
        bytes.extend_from_slice(&node_public_key);
        push_field(&mut bytes, self.group_id.as_bytes());
        push_field(&mut bytes, self.coordinator_id.as_bytes());
        bytes.extend_from_slice(&self.seq.to_be_bytes());
        bytes.extend_from_slice(&log_hash);
        bytes.extend_from_slice(&control_head);
        bytes.extend_from_slice(&self.policy_version.to_be_bytes());
        bytes.extend_from_slice(&self.mls_epoch.to_be_bytes());
        Ok(bytes)
    }

    /// 用固定节点公钥验签。
    pub fn verify(&self, expected_node_public_key: &[u8; 32]) -> NodeResult<()> {
        let node_public_key = hex_decode::<32>("node public key", &self.node_public_key)?;
        if &node_public_key != expected_node_public_key {
            return Err(NodeError::NodeIdentityMismatch);
        }
        let signature = hex_decode::<64>("head signature", &self.signature)?;
        verify_ed25519(
            expected_node_public_key,
            &self.signing_bytes()?,
            &signature,
            || NodeError::InvalidSignature,
        )
    }
}

/// 日志条目：被接受的候选 + 节点回执（客户端据此重建并验链）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub candidate: SignedCandidate,
    pub receipt: AppendResult,
    /// 节点侧的领域操作投影（PROTOCOL-004 字段 55 的文本形式）；未声明为 `None`。
    #[serde(default)]
    pub domain_operation: Option<String>,
    /// 领域操作目标摘要（小写 hex）；未声明为 `None`。
    #[serde(default)]
    pub domain_target_digest: Option<String>,
}

/// 一页日志 + 签名 head + 位置。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogPage {
    pub group_id: String,
    pub head: SignedLogHead,
    pub entries: Vec<LogEntry>,
    /// 下一页的 after 参数（本页最后一条的 seq；空页时等于请求的 after）。
    pub next_after: u64,
    /// 是否还有更多已授权范围内的条目。
    pub has_more: bool,
}

pub(crate) struct GroupRow {
    pub(crate) coordinator_id: String,
    pub(crate) last_seq: u64,
    pub(crate) last_hash: [u8; 32],
    pub(crate) state: String,
}

struct ControlRow {
    control_head: [u8; 32],
    policy_version: u64,
    mls_epoch: u64,
}

/// 节点快照：事务外先取出的签名材料，避免与事务借用冲突。
struct NodeSnapshot {
    node_id: String,
    node_public_key: [u8; 32],
    signing_key: SigningKey,
}

impl NodeSnapshot {
    fn take(service: &NodeService) -> Self {
        Self {
            node_id: service.node_id().to_owned(),
            node_public_key: service.node_public_key(),
            signing_key: service.signing_key().clone(),
        }
    }
}

pub(crate) fn load_group(
    tx: &rusqlite::Transaction<'_>,
    group_id: &str,
) -> NodeResult<Option<GroupRow>> {
    let row: Option<(String, i64, Vec<u8>, String)> = tx
        .query_row(
            "SELECT coordinator_id, last_seq, last_hash, state
             FROM groups WHERE group_id = ?1",
            [group_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    row.map(|(coordinator_id, last_seq, last_hash, state)| {
        Ok(GroupRow {
            coordinator_id,
            last_seq: u64::try_from(last_seq).map_err(|_| NodeError::StorageUnavailable)?,
            last_hash: bytes32("last hash", last_hash)?,
            state,
        })
    })
    .transpose()
}

fn load_control(tx: &rusqlite::Transaction<'_>, group_id: &str) -> NodeResult<Option<ControlRow>> {
    let row: Option<(Vec<u8>, i64, i64)> = tx
        .query_row(
            "SELECT control_head, policy_version, mls_epoch FROM group_control_heads
             WHERE group_id = ?1",
            [group_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    row.map(|(control_head, policy_version, mls_epoch)| {
        Ok(ControlRow {
            control_head: bytes32("control head", control_head)?,
            policy_version: u64::try_from(policy_version)
                .map_err(|_| NodeError::StorageUnavailable)?,
            mls_epoch: u64::try_from(mls_epoch).map_err(|_| NodeError::StorageUnavailable)?,
        })
    })
    .transpose()
}

/// 成员窗口：某设备在某群的有效成员记录（joined_seq, left_seq）。
pub(crate) fn load_membership(
    tx: &rusqlite::Transaction<'_>,
    group_id: &str,
    device_id: &str,
    up_to_seq: u64,
) -> NodeResult<Option<(u64, Option<u64>)>> {
    let row: Option<(i64, Option<i64>)> = tx
        .query_row(
            "SELECT joined_seq, left_seq FROM group_members
             WHERE group_id = ?1 AND device_id = ?2 AND joined_seq <= ?3
             ORDER BY joined_seq DESC LIMIT 1",
            params![group_id, device_id, to_i64("head seq", up_to_seq)?],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    row.map(|(joined_seq, left_seq)| {
        Ok((
            u64::try_from(joined_seq).map_err(|_| NodeError::StorageUnavailable)?,
            match left_seq {
                Some(value) => {
                    Some(u64::try_from(value).map_err(|_| NodeError::StorageUnavailable)?)
                }
                None => None,
            },
        ))
    })
    .transpose()
}

fn build_head(
    snapshot: &NodeSnapshot,
    group_id: &str,
    coordinator_id: &str,
    seq: u64,
    log_hash: &[u8; 32],
    control: &ControlRow,
) -> NodeResult<SignedLogHead> {
    let mut head = SignedLogHead {
        group_id: group_id.to_owned(),
        coordinator_id: coordinator_id.to_owned(),
        seq,
        log_hash: hex_encode(log_hash),
        control_head: hex_encode(&control.control_head),
        policy_version: control.policy_version,
        mls_epoch: control.mls_epoch,
        node_id: snapshot.node_id.clone(),
        node_public_key: hex_encode(&snapshot.node_public_key),
        signature: String::new(),
    };
    let signature: Signature = snapshot.signing_key.sign(&head.signing_bytes()?);
    head.signature = hex_encode(&signature.to_bytes());
    Ok(head)
}

impl NodeService {
    /// NODE-005：固定 GroupId/协调节点与初始授权设备集合，幂等返回节点签名回执。
    ///
    /// 前置条件：actor 是该群 owner 账户的已登记有效设备；创建请求由该设备私钥签名。
    /// 同 GroupId 同创建摘要返回同一份持久回执；同 GroupId 换内容即 GROUP_ID_CONFLICT。
    pub fn register_group(
        &mut self,
        request: &SignedGroupCreation,
        actor: &AuthenticatedDevice,
        now: u64,
    ) -> NodeResult<GroupRegistration> {
        validate_timestamp("now", now)?;
        let creation = &request.creation;
        creation.validate_shape(self.policy().group_log.max_members)?;
        let (account_id, state, _version) = self
            .device_account_state(&actor.device_id)?
            .ok_or(NodeError::InvalidCredential)?;
        if state != "active" {
            return Err(NodeError::DeviceRevoked);
        }
        if account_id != actor.account_id || account_id != creation.owner_account_id {
            return Err(NodeError::PermissionDenied);
        }
        let device_key = self.registered_device_key(&actor.device_id)?;
        request.verify(&device_key)?;
        let creation_hash = request.digest()?;
        let control_head = hex_decode::<32>("initial control head", &creation.initial_control_head)?;
        let policy_version = creation.policy_version;
        let mls_epoch = creation.mls_epoch;
        let creation_hash_hex = hex_encode(&creation_hash);
        let snapshot = NodeSnapshot::take(self);
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // 幂等/冲突：同 GroupId 已登记。
        let existing: Option<(Vec<u8>, Vec<u8>)> = tx
            .query_row(
                "SELECT creation_hash, receipt FROM group_registrations WHERE group_id = ?1",
                [&creation.group_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((stored_hash, receipt)) = existing {
            if stored_hash.as_slice() != creation_hash.as_slice() {
                return Err(NodeError::GroupIdConflict);
            }
            let registration: GroupRegistration = serde_json::from_slice(&receipt)?;
            registration.verify(&snapshot.node_public_key)?;
            if registration.group_id != creation.group_id
                || registration.owner_account_id != creation.owner_account_id
                || registration.creation_hash != creation_hash_hex
            {
                return Err(NodeError::StorageUnavailable);
            }
            tx.commit()?;
            return Ok(registration);
        }
        // 初始成员必须是 owner 账户下的已登记有效设备。
        for member in &creation.members {
            let row: Option<(String, String)> = tx
                .query_row(
                    "SELECT account_id, state FROM devices WHERE device_id = ?1",
                    [&member.device_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let (member_account, member_state) = row.ok_or(NodeError::InvalidCredential)?;
            if member_state != "active" {
                return Err(NodeError::DeviceRevoked);
            }
            if member_account != creation.owner_account_id {
                return Err(NodeError::PermissionDenied);
            }
        }
        tx.execute(
            "INSERT INTO groups (group_id, coordinator_id, owner_account_id, last_seq, last_hash, state)
             VALUES (?1, ?2, ?3, 0, ?4, 'active')",
            params![
                creation.group_id,
                snapshot.node_id,
                creation.owner_account_id,
                ZERO_HASH.as_slice()
            ],
        )?;
        let public_policy = serde_json::to_vec(&creation.members)?;
        tx.execute(
            "INSERT INTO group_control_heads (group_id, control_head, policy_version, mls_epoch, public_policy)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                creation.group_id,
                control_head.as_slice(),
                to_i64("policy version", policy_version)?,
                to_i64("mls epoch", mls_epoch)?,
                public_policy
            ],
        )?;
        for member in &creation.members {
            tx.execute(
                "INSERT INTO group_members (group_id, device_id, account_id, role, muted, joined_seq, left_seq)
                 VALUES (?1, ?2, ?3, ?4, 0, 0, NULL)",
                params![
                    creation.group_id,
                    member.device_id,
                    creation.owner_account_id,
                    member.role
                ],
            )?;
        }
        let mut registration = GroupRegistration {
            group_id: creation.group_id.clone(),
            owner_account_id: creation.owner_account_id.clone(),
            coordinator_id: snapshot.node_id.clone(),
            initial_control_head: creation.initial_control_head.clone(),
            policy_version,
            mls_epoch,
            creation_hash: creation_hash_hex,
            members: creation.members.clone(),
            registered_at: now,
            node_id: snapshot.node_id.clone(),
            node_public_key: hex_encode(&snapshot.node_public_key),
            signature: String::new(),
        };
        let signature: Signature = snapshot.signing_key.sign(&registration.signing_bytes()?);
        registration.signature = hex_encode(&signature.to_bytes());
        tx.execute(
            "INSERT INTO group_registrations (group_id, creation_hash, registered_at, receipt)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                creation.group_id,
                creation_hash.as_slice(),
                to_i64("now", now)?,
                serde_json::to_vec(&registration)?
            ],
        )?;
        tx.commit()?;
        Ok(registration)
    }

    /// NODE-006：为合法候选持久分配唯一 seq 与签名回执（单事务、幂等、哈希链）。
    pub fn append_group_entry(
        &mut self,
        group: &str,
        candidate: &SignedCandidate,
        actor: &AuthenticatedDevice,
        now: u64,
    ) -> NodeResult<AppendResult> {
        self.append_group_entry_inner(group, candidate, None, actor, now, |_| {})
    }

    /// NODE-006：带**可选领域操作声明**的追加（PROTOCOL-004 字段 55/56）。
    ///
    /// 声明参与作者签名原文与候选摘要（因此“只改声明不改签名”会 `INVALID_SIGNATURE`），
    /// 并写入日志条目的元数据投影：`group_log.domain_operation` / `domain_target_digest`，
    /// 读日志时通过 `LogEntry.domain_operation` 暴露。旧客户端不传 → 行为与旧路径逐字节一致。
    pub fn append_group_entry_with_domain(
        &mut self,
        group: &str,
        candidate: &SignedCandidate,
        domain: Option<&CandidateDomainOperation>,
        actor: &AuthenticatedDevice,
        now: u64,
    ) -> NodeResult<AppendResult> {
        self.append_group_entry_inner(group, candidate, domain, actor, now, |_| {})
    }

    /// 与 [`Self::append_group_entry`] 完全一致，只是允许注入崩溃/审计检查点。
    ///
    /// 该钩子只用于“进程在事务中途被杀死时数据库不留半条日志”的证据，不改变生产语义：
    /// 检查点闭包之外没有任何额外写路径。
    #[doc(hidden)]
    pub fn append_group_entry_with_checkpoint(
        &mut self,
        group: &str,
        candidate: &SignedCandidate,
        actor: &AuthenticatedDevice,
        now: u64,
        checkpoint: impl Fn(AppendCheckpoint),
    ) -> NodeResult<AppendResult> {
        self.append_group_entry_inner(group, candidate, None, actor, now, checkpoint)
    }

    /// 追加路径的唯一实现：可选领域声明在**验签之前**参与签名原文重建。
    fn append_group_entry_inner(
        &mut self,
        group: &str,
        candidate: &SignedCandidate,
        domain: Option<&CandidateDomainOperation>,
        actor: &AuthenticatedDevice,
        now: u64,
        checkpoint: impl Fn(AppendCheckpoint),
    ) -> NodeResult<AppendResult> {
        validate_timestamp("now", now)?;
        validate_bounded_id("group id", group, MAX_GROUP_ID_BYTES)?;
        let group_key = GroupId::parse(group).map_err(|_| NodeError::InvalidRequest("group id"))?;
        let max_ciphertext = self.policy().group_log.max_candidate_ciphertext_bytes;
        // 路由、作者与演员必须一致：不允许代别人追加，也不允许跨群重放。
        if candidate.candidate.author_device_id != actor.device_id
            || candidate.candidate.group_id != group
        {
            return Err(NodeError::PermissionDenied);
        }
        let author_key = self.registered_device_key(&actor.device_id)?;
        // 签名原文与候选摘要都按“含领域声明”的形态重建：只改声明不改签名 → 验签失败。
        let unsigned = candidate
            .candidate
            .to_unsigned_with_domain(max_ciphertext, domain)?;
        let signing_bytes = candidate_signing_bytes(&unsigned)
            .map_err(|_| NodeError::InvalidCandidate)?;
        let signature = hex_decode::<64>("author signature", &candidate.author_signature)?;
        verify_ed25519(&author_key, signing_bytes.as_bytes(), &signature, || {
            NodeError::InvalidSignature
        })?;
        let candidate_hash = sha256(signing_bytes.as_bytes());
        let submission_id = candidate.candidate.submission_id.clone();
        let kind = candidate.candidate.kind.clone();
        // 领域操作投影（PROTOCOL-004 字段 55/56）：非法声明在写库之前就失败。
        let domain_projection = candidate.domain_projection(domain)?;
        let declared_control_head = hex_decode::<32>(
            "expected control head",
            &candidate.candidate.expected_control_head,
        )?;
        let declared_policy = candidate.candidate.policy_version;
        let declared_epoch = candidate.candidate.mls_epoch;
        let immutable_entry = serde_json::to_vec(candidate)?;
        let snapshot = NodeSnapshot::take(self);
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let group_row = load_group(&tx, group)?.ok_or(NodeError::GroupNotFound)?;
        if group_row.state != "active" || group_row.coordinator_id != snapshot.node_id {
            return Err(NodeError::PermissionDenied);
        }
        // 同 submission：同摘要返回旧结果，相异摘要显式拒绝。
        let existing: Option<(Vec<u8>, Vec<u8>, Vec<u8>, i64)> = tx
            .query_row(
                "SELECT candidate_hash, previous_hash, signed_receipt, group_seq
                 FROM group_log WHERE group_id = ?1 AND submission_id = ?2",
                params![group, submission_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        if let Some((stored_hash, stored_previous, stored_receipt, stored_seq)) = existing {
            if stored_hash.as_slice() != candidate_hash.as_slice() {
                return Err(NodeError::SubmissionIdConflict);
            }
            let receipt: AppendResult = serde_json::from_slice(&stored_receipt)?;
            receipt.verify(&snapshot.node_public_key)?;
            receipt.verify_candidate(candidate, max_ciphertext)?;
            let stored_seq =
                u64::try_from(stored_seq).map_err(|_| NodeError::StorageUnavailable)?;
            if receipt.seq != stored_seq || receipt.previous_hash != hex_encode(&stored_previous) {
                return Err(NodeError::StorageUnavailable);
            }
            tx.commit()?;
            return Ok(receipt);
        }
        // 权限：演员必须是该群当前有效成员（窗口按 joined_seq/left_seq）。
        let membership = load_membership(&tx, group, &actor.device_id, group_row.last_seq)?
            .ok_or(NodeError::PermissionDenied)?;
        if membership.1.is_some() {
            return Err(NodeError::PermissionDenied);
        }
        // 公开元数据 CAS：head/policy/epoch 必须等于节点当前值。
        let control = load_control(&tx, group)?.ok_or(NodeError::StorageUnavailable)?;
        if declared_policy != control.policy_version || declared_control_head != control.control_head {
            return Err(NodeError::StaleControlHead);
        }
        if declared_epoch != control.mls_epoch {
            return Err(NodeError::StaleEpoch);
        }
        let seq = group_row
            .last_seq
            .checked_add(1)
            .ok_or(NodeError::StorageUnavailable)?;
        let log_hash = log_chain_hash(group, seq, &group_row.last_hash, &candidate_hash);
        let mut receipt = AppendResult {
            group_id: group.to_owned(),
            submission_id,
            author_device_id: actor.device_id.clone(),
            seq,
            candidate_hash: hex_encode(&candidate_hash),
            previous_hash: hex_encode(&group_row.last_hash),
            log_hash: hex_encode(&log_hash),
            accepted_at: now,
            node_id: snapshot.node_id.clone(),
            node_public_key: hex_encode(&snapshot.node_public_key),
            signature: String::new(),
        };
        let receipt_bytes = receipt_signing_bytes(
            Digest::from_bytes(candidate_hash),
            seq,
            Digest::from_bytes(group_row.last_hash),
            group_key,
        )
        .map_err(|_| NodeError::InvalidRequest("candidate"))?;
        let signature: Signature = snapshot.signing_key.sign(receipt_bytes.as_bytes());
        receipt.signature = hex_encode(&signature.to_bytes());
        tx.execute(
            "INSERT INTO group_log
                 (group_id, group_seq, submission_id, author_device_id, kind, candidate_hash,
                  previous_hash, log_hash, immutable_entry, signed_receipt,
                  domain_operation, domain_target_digest)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                group,
                to_i64("seq", seq)?,
                receipt.submission_id,
                receipt.author_device_id,
                kind,
                candidate_hash.as_slice(),
                group_row.last_hash.as_slice(),
                log_hash.as_slice(),
                immutable_entry,
                serde_json::to_vec(&receipt)?,
                domain_projection.as_ref().map(|(operation, _)| operation),
                domain_projection.as_ref().map(|(_, digest)| digest)
            ],
        )?;
        checkpoint(AppendCheckpoint::AfterLogInsert);
        // NODE-006b：控制提交里声明的**新增成员**（字段 59）同步进节点成员表。
        // 这是“成员变更提交”这条既有载体的落地：节点不解析正文，只用已验签的公开元数据。
        if kind == KIND_CONTROL {
            if let Some(metadata) = candidate.candidate.control_metadata.as_ref() {
                for added in &metadata.added_credentials {
                    if added.is_empty() {
                        continue;
                    }
                    let existing: Option<i64> = tx
                        .query_row(
                            "SELECT joined_seq FROM group_members WHERE group_id = ?1 AND device_id = ?2",
                            params![group, added],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if existing.is_some() {
                        continue;
                    }
                    // 成员必须是本节点**已注册且 active** 的设备（显式拒绝，不让外键报成存储故障）。
                    let device: Option<(String, String)> = tx
                        .query_row(
                            "SELECT account_id, state FROM devices WHERE device_id = ?1",
                            [added],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .optional()?;
                    let (member_account, member_state) =
                        device.ok_or(NodeError::InvalidCredential)?;
                    if member_state != "active" {
                        return Err(NodeError::DeviceRevoked);
                    }
                    if member_account != actor.account_id {
                        return Err(NodeError::PermissionDenied);
                    }
                    tx.execute(
                        "INSERT INTO group_members (group_id, device_id, account_id, role, muted, joined_seq, left_seq)
                         VALUES (?1, ?2, ?3, ?4, 0, ?5, NULL)",
                        params![
                            group,
                            added,
                            actor.account_id.clone(),
                            "member",
                            to_i64("joined seq", seq)?
                        ],
                    )?;
                }
            }
        }
        // 控制候选推进“节点可观察的控制链头”。公开 policy/epoch 不由节点猜测推进。
        if kind == KIND_CONTROL {
            tx.execute(
                "UPDATE group_control_heads SET control_head = ?2 WHERE group_id = ?1",
                params![group, log_hash.as_slice()],
            )?;
        }
        let updated = tx.execute(
            "UPDATE groups SET last_seq = ?2, last_hash = ?3 WHERE group_id = ?1",
            params![group, to_i64("seq", seq)?, log_hash.as_slice()],
        )?;
        if updated != 1 {
            return Err(NodeError::StorageUnavailable);
        }
        checkpoint(AppendCheckpoint::BeforeCommit);
        tx.commit()?;
        checkpoint(AppendCheckpoint::AfterCommit);
        Ok(receipt)
    }

    /// NODE-007：按 seq 分页返回成员窗口内的日志与签名 head。
    pub fn fetch_group_log(
        &mut self,
        group: &str,
        after: u64,
        limit: u32,
        actor: &AuthenticatedDevice,
    ) -> NodeResult<LogPage> {
        validate_bounded_id("group id", group, MAX_GROUP_ID_BYTES)?;
        GroupId::parse(group).map_err(|_| NodeError::InvalidRequest("group id"))?;
        validate_timestamp("after", after)?;
        let max_entries = self.policy().group_log.max_page_entries;
        if limit == 0 || limit > max_entries {
            return Err(NodeError::InvalidRequest("log page limit"));
        }
        let snapshot = NodeSnapshot::take(self);
        let tx = self.connection_mut().transaction()?;
        let group_row = load_group(&tx, group)?.ok_or(NodeError::GroupNotFound)?;
        let control = load_control(&tx, group)?.ok_or(NodeError::StorageUnavailable)?;
        let membership = load_membership(&tx, group, &actor.device_id, group_row.last_seq)?
            .ok_or(NodeError::HistoryNotAuthorized)?;
        let joined_seq = membership.0;
        let left_seq = membership.1;
        let from = after.saturating_add(1).max(joined_seq);
        let upper = match left_seq {
            Some(left) => Some(to_i64("left seq", left)?),
            None => None,
        };
        let mut entries: Vec<LogEntry> = Vec::new();
        {
            let mut statement = tx.prepare(
                "SELECT group_seq, candidate_hash, previous_hash, log_hash, immutable_entry,
                        signed_receipt, domain_operation, domain_target_digest
                 FROM group_log
                 WHERE group_id = ?1 AND group_seq >= ?2 AND (?3 IS NULL OR group_seq < ?3)
                 ORDER BY group_seq LIMIT ?4",
            )?;
            let mut rows = statement.query(params![
                group,
                to_i64("after", from)?,
                upper,
                i64::from(limit)
            ])?;
            while let Some(row) = rows.next()? {
                let seq = u64::try_from(row.get::<_, i64>(0)?)
                    .map_err(|_| NodeError::StorageUnavailable)?;
                let candidate_hash = bytes32("candidate hash", row.get(1)?)?;
                let previous_hash = bytes32("previous hash", row.get(2)?)?;
                let log_hash = bytes32("log hash", row.get(3)?)?;
                let candidate: SignedCandidate = serde_json::from_slice(&row.get::<_, Vec<u8>>(4)?)?;
                let receipt: AppendResult = serde_json::from_slice(&row.get::<_, Vec<u8>>(5)?)?;
                receipt.verify(&snapshot.node_public_key)?;
                if receipt.seq != seq
                    || receipt.candidate_hash != hex_encode(&candidate_hash)
                    || receipt.previous_hash != hex_encode(&previous_hash)
                    || receipt.log_hash != hex_encode(&log_hash)
                    || log_hash != log_chain_hash(group, seq, &previous_hash, &candidate_hash)
                {
                    return Err(NodeError::StorageUnavailable);
                }
                let domain_operation: Option<String> = row.get(6)?;
                let domain_target_digest: Option<String> = row.get(7)?;
                entries.push(LogEntry {
                    candidate,
                    receipt,
                    domain_operation,
                    domain_target_digest,
                });
            }
        }
        let next_after = entries
            .last()
            .map(|entry| entry.receipt.seq)
            .unwrap_or_else(|| from.saturating_sub(1));
        let window_end = match left_seq {
            Some(left) => group_row.last_seq.min(left.saturating_sub(1)),
            None => group_row.last_seq,
        };
        let has_more = entries.len() == limit as usize && next_after < window_end;
        let head = build_head(
            &snapshot,
            group,
            &group_row.coordinator_id,
            group_row.last_seq,
            &group_row.last_hash,
            &control,
        )?;
        tx.commit()?;
        Ok(LogPage {
            group_id: group.to_owned(),
            head,
            entries,
            next_after,
            has_more,
        })
    }

    /// NODE-008：返回绑定 group/seq/hash/control_head/policy/epoch 的节点签名头。
    pub fn get_group_head(
        &mut self,
        group: &str,
        actor: &AuthenticatedDevice,
    ) -> NodeResult<SignedLogHead> {
        validate_bounded_id("group id", group, MAX_GROUP_ID_BYTES)?;
        GroupId::parse(group).map_err(|_| NodeError::InvalidRequest("group id"))?;
        let snapshot = NodeSnapshot::take(self);
        let tx = self.connection_mut().transaction()?;
        let group_row = load_group(&tx, group)?.ok_or(NodeError::GroupNotFound)?;
        let control = load_control(&tx, group)?.ok_or(NodeError::StorageUnavailable)?;
        let membership = load_membership(&tx, group, &actor.device_id, group_row.last_seq)?
            .ok_or(NodeError::PermissionDenied)?;
        if membership.1.is_some() {
            return Err(NodeError::PermissionDenied);
        }
        let head = build_head(
            &snapshot,
            group,
            &group_row.coordinator_id,
            group_row.last_seq,
            &group_row.last_hash,
            &control,
        )?;
        tx.commit()?;
        Ok(head)
    }

    /// 节点运维自检：逐条复核日志链、回执签名与 head 一致（没有 HTTP 端点）。
    pub fn audit_group_log(&mut self, group: &str) -> NodeResult<u64> {
        validate_bounded_id("group id", group, MAX_GROUP_ID_BYTES)?;
        GroupId::parse(group).map_err(|_| NodeError::InvalidRequest("group id"))?;
        let max_ciphertext = self.policy().group_log.max_candidate_ciphertext_bytes;
        let snapshot = NodeSnapshot::take(self);
        let tx = self.connection_mut().transaction()?;
        let group_row = load_group(&tx, group)?.ok_or(NodeError::GroupNotFound)?;
        let mut expected_seq = 0_u64;
        let mut previous = ZERO_HASH;
        {
            let mut statement = tx.prepare(
                "SELECT group_seq, candidate_hash, previous_hash, log_hash, immutable_entry,
                        signed_receipt, domain_operation, domain_target_digest
                 FROM group_log WHERE group_id = ?1 ORDER BY group_seq",
            )?;
            let mut rows = statement.query([group])?;
            while let Some(row) = rows.next()? {
                let seq = u64::try_from(row.get::<_, i64>(0)?)
                    .map_err(|_| NodeError::StorageUnavailable)?;
                let candidate_hash = bytes32("candidate hash", row.get(1)?)?;
                let stored_previous = bytes32("previous hash", row.get(2)?)?;
                let log_hash = bytes32("log hash", row.get(3)?)?;
                let immutable_entry: Vec<u8> = row.get(4)?;
                let signed_receipt: Vec<u8> = row.get(5)?;
                let candidate: SignedCandidate = serde_json::from_slice(&immutable_entry)?;
                let receipt: AppendResult = serde_json::from_slice(&signed_receipt)?;
                receipt.verify(&snapshot.node_public_key)?;
                if seq != expected_seq + 1 || stored_previous != previous {
                    return Err(NodeError::StorageUnavailable);
                }
                // 领域声明参与签名原文/链摘要：自检必须按日志里记录的声明重建，否则会误判。
                let domain = match (
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ) {
                    (Some(operation), Some(target_digest)) => Some(CandidateDomainOperation {
                        operation,
                        target_digest,
                    }),
                    _ => None,
                };
                let rebuilt = candidate
                    .candidate
                    .to_unsigned_with_domain(max_ciphertext, domain.as_ref())?;
                let rebuilt_bytes = candidate_signing_bytes(&rebuilt)
                    .map_err(|_| NodeError::InvalidCandidate)?;
                if sha256(rebuilt_bytes.as_bytes()) != candidate_hash {
                    return Err(NodeError::StorageUnavailable);
                }
                if receipt.seq != seq
                    || receipt.candidate_hash != hex_encode(&candidate_hash)
                    || receipt.previous_hash != hex_encode(&stored_previous)
                    || receipt.log_hash != hex_encode(&log_hash)
                    || log_hash != log_chain_hash(group, seq, &stored_previous, &candidate_hash)
                {
                    return Err(NodeError::StorageUnavailable);
                }
                previous = log_hash;
                expected_seq = seq;
            }
        }
        // 日志末尾必须与群行的 last_seq/last_hash 一致（否则自检结论不可信）。
        if expected_seq != group_row.last_seq || previous != group_row.last_hash {
            return Err(NodeError::StorageUnavailable);
        }
        // head 必须能验签并与末尾日志/控制头一致。
        let control = load_control(&tx, group)?.ok_or(NodeError::StorageUnavailable)?;
        let head = build_head(
            &snapshot,
            group,
            &group_row.coordinator_id,
            group_row.last_seq,
            &group_row.last_hash,
            &control,
        )?;
        head.verify(&snapshot.node_public_key)?;
        if head.seq != expected_seq || head.log_hash != hex_encode(&previous) {
            return Err(NodeError::StorageUnavailable);
        }
        tx.commit()?;
        Ok(expected_seq)
    }
}
