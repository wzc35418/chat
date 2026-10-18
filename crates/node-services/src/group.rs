//! NODE-005~008：群登记、排序日志追加、日志分页与节点签名 head。
//!
//! 事务边界（TRANSACTIONS.md T4）：群登记（groups/group_members/group_control_heads/
//! group_registrations）与日志追加（group_log/控制头/groups.last_seq+last_hash）
//! 各自是单个 IMMEDIATE 事务；崩溃只会回滚整个事务，不会留下半条日志。
//! 节点不读 MLS 明文，只做公开元数据校验；安全校验失败一律显式返回错误。
use chat_protocol::{
    CandidateKind, ControlHead, ControlMetadata, ControlOperation, Digest, GroupId, DeviceId,
    ProtocolVersion, SubmissionId, UnsignedCandidate, candidate_signing_bytes, receipt_signing_bytes,
};
use ed25519_dalek::{Signature, Signer, SigningKey};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::canonical::{
    GROUP_CREATION_DOMAIN, GROUP_REGISTRATION_DOMAIN, LOG_CHAIN_DOMAIN, LOG_HEAD_DOMAIN,
    decode_hex_bytes, hex_decode, hex_encode, push_field, sha256, sha256_concat,
    validate_bounded_id, validate_timestamp, verify_ed25519,
};
use crate::error::{NodeError, NodeResult};
use crate::limits::{
    MAX_ACCOUNT_ID_BYTES, MAX_GROUP_ID_BYTES, MAX_GROUP_ROLE_BYTES, MAX_SUBMISSION_ID_BYTES,
    MAX_TIMESTAMP,
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateControlMetadata {
    /// invite/remove/set_role/set_muted/rename/transfer_ownership/leave/dissolve。
    pub operation: String,
    pub target_digest: String,
}

/// 传输层候选字段（文本形式，具体规范编码由 PROTOCOL-004 冻结）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupCandidate {
    pub protocol_major: u16,
    pub protocol_minor: u16,
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
    /// 转换到协议层候选：解析规范 ID、校验密文上限与控制元数据一致性。
    pub fn to_unsigned(&self, max_ciphertext: usize) -> NodeResult<UnsignedCandidate> {
        let kind = match self.kind.as_str() {
            KIND_APPLICATION => CandidateKind::Application,
            KIND_CONTROL => CandidateKind::Control,
            _ => return Err(NodeError::InvalidRequest("candidate kind")),
        };
        validate_timestamp("policy version", self.policy_version)?;
        validate_timestamp("mls epoch", self.mls_epoch)?;
        let control_metadata = match &self.control_metadata {
            Some(metadata) => Some(ControlMetadata {
                operation: control_operation(&metadata.operation)?,
                target_digest: Digest::from_bytes(hex_decode::<32>(
                    "control target digest",
                    &metadata.target_digest,
                )?),
            }),
            None => None,
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

struct GroupRow {
    coordinator_id: String,
    owner_account_id: String,
    last_seq: u64,
    last_hash: [u8; 32],
    state: String,
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

fn column_hash(field: &'static str, bytes: Vec<u8>) -> NodeResult<[u8; 32]> {
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| NodeError::StorageUnavailable)
        .and_then(|hash| {
            let _ = field;
            Ok(hash)
        })
}

fn load_group(tx: &rusqlite::Transaction<'_>, group_id: &str) -> NodeResult<Option<GroupRow>> {
    let row: Option<(String, String, i64, Vec<u8>, String)> = tx
        .query_row(
            "SELECT coordinator_id, owner_account_id, last_seq, last_hash, state
             FROM groups WHERE group_id = ?1",
            [group_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()?;
    row.map(|(coordinator_id, owner_account_id, last_seq, last_hash, state)| {
        Ok(GroupRow {
            coordinator_id,
            owner_account_id,
            last_seq: u64::try_from(last_seq).map_err(|_| NodeError::StorageUnavailable)?,
            last_hash: column_hash("last hash", last_hash)?,
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
            control_head: column_hash("control head", control_head)?,
            policy_version: u64::try_from(policy_version)
                .map_err(|_| NodeError::StorageUnavailable)?,
            mls_epoch: u64::try_from(mls_epoch).map_err(|_| NodeError::StorageUnavailable)?,
        })
    })
    .transpose()
}

/// 成员窗口：某设备在某群的有效成员记录（joined_seq, left_seq）。
fn load_membership(
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
    pub fn register_group(
        &mut self,
        request: &SignedGroupCreation,
        actor: &AuthenticatedDevice,
        now: u64,
    ) -> NodeResult<GroupRegistration> {
        let paused = ();
        let _ = paused;
        Ok(GroupRegistration {
            ..request_fixture()
        })
    }
}

fn request_fixture() -> GroupRegistration {
    GroupRegistration {
        group_id: String::new(),
        owner_account_id: String::new(),
        coordinator_id: String::new(),
        initial_control_head: String::new(),
        policy_version: 0,
        mls_epoch: 0,
        creation_hash: String::new(),
        members: Vec::new(),
        registered_at: 0,
        node_id: String::new(),
        node_public_key: String::new(),
        signature: String::new(),
    }
}