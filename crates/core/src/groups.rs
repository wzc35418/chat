//! GROUPS-001~004：建群、正式群快照与成员治理（真实 OpenMLS + DOMAIN-004 策略层，不做任何模拟）。
//!
//! 本切片的四件事都建立在既有原语上：
//!
//! - **建群**（GROUPS-001）：P0 引导（`MlsDevice::create_and_invite`）建立含创建者与首个成员的
//!   真实 MLS 群，其余成员用 CRYPTO-005 `prepare_control(AddMembers)` 生成真实 Commit/Welcome（pending）；
//! - **快照**（GROUPS-002）：从正式库读持久快照，用 `MlsDevice::import_from_store` 恢复真实 MLS 状态，
//!   epoch / pending 取权威值；未知群显式失败；
//! - **角色治理**（GROUPS-003）：DOMAIN-002 `authorize_group_operation` 判权限、DOMAIN-004
//!   `apply_policy_operation` 计算新的 `policy_version` 与 `control_head`（不本地臆造），再用
//!   CRYPTO-005 `prepare_control(SelfUpdate)` 生成真实 MLS 控制提交并写 pending；
//! - **禁言**（GROUPS-004）：纯策略层（无 MLS），同样经 DOMAIN-002/004，快照的 `muted` 与
//!   `policy_version` 更新。
//!
//! 账户/设备标识：本切片还没有账户目录接入，正式快照需要的 `AccountId`/`DeviceId` 由成员
//! **凭据文本确定性派生**（`sha256(域 || credential)`，域分离），同一个凭据恒得同一个标识；
//! 这不是真实账户 ID，接入 IDENTITY 账户目录后应替换（见文末限制）。
//!
//! 持久化边界：正式库目前没有群元数据表，群快照（名称/成员/角色/禁言/策略头/版本）以**独立
//! storage group_id 命名空间**（`grpmeta/<sha256(协议群 ID)>`）写入 `mls_group_state` 的
//! `state` 槽位（storage 负责密封、版本与 epoch 单调校验）；MLS 自己的群状态仍按 MLS group id
//! 存，两者互不覆盖。
//!
//! 已知限制：
//! - crypto 只有“双人引导”能建群（`create_and_invite` 需要同进程对端设备句柄），没有自建空群 API；
//! - 没有群列表 API：所有入口都必须给出 `group_id`（协议群 ID 由 MLS 群键确定性派生，与发送链路一致）；
//! - 控制提交的**接受与合并**（CRYPTO-006 `merge_accepted_pending` / 节点回执）尚未接线：
//!   本切片把“已写入策略快照”视为本地生效，pending 提交仍会被如实报告；
//! - 有未决控制提交时 GROUPS-004 显式拒绝，GROUPS-003 由 crypto 的 `prepare_control` 拒绝。

use chat_crypto::control::{
    CommitExpectation, ControlError, ControlOperation, PendingControl, ProcessedEntry,
    VerifiedGroupEntry, WelcomeAcceptance,
};
use chat_crypto::engine::MlsDevice;
use chat_domain::{
    Actor, ConversationKind, DomainError, GroupMember, GroupOperation, GroupSnapshot, MemberRole,
    MembershipWindow, ProposedPolicy, apply_policy_operation, authorize_group_operation,
};
use chat_protocol::{AccountId, ControlHead, CoordinatorId, DeviceId, Digest, GroupId};
use chat_storage::formal::{MlsGroupStateInput, StorageError, Store};
use serde::{Deserialize, Serialize};
use sha2::{Digest as Sha2Digest, Sha256};

use crate::send_runtime::protocol_group_id_for;

/// 群名上限（字节）。
pub const MAX_GROUP_NAME_BYTES: usize = 64;
/// 群成员上限（含创建者；与 crypto 的单次 add 上限一致）。
pub const MAX_GROUP_MEMBERS: usize = 32;
/// 成员凭据身份上限（字节）。
pub const MAX_CREDENTIAL_BYTES: usize = 128;
/// 本切片支持的群策略版本（尚无策略表；固定为 1 并随快照持久化）。
pub const GROUP_POLICY_VERSION: u32 = 1;
/// 快照记录格式版本（进入持久记录体，不可静默兼容旧版本）。
///
/// v3：增加 `pending_removed`（已本地生效但尚未被接受合并的移除）与
/// `last_control_cleared`（最近一次被丢弃的控制提交，CRYPTO-008）。
const SNAPSHOT_VERSION: u8 = 6;
/// 快照记录的 storage group_id 命名空间前缀。
const SNAPSHOT_GROUP_PREFIX: &str = "grpmeta/";
/// 快照记录体上限（字节）。
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024;
/// 账户标识派生域（本切片的本地投影，不是真实账户 ID）。
const ACCOUNT_DOMAIN: &[u8] = b"chat/groups/account-id/v1";
/// 设备标识派生域。
const DEVICE_DOMAIN: &[u8] = b"chat/groups/device-id/v1";
/// 协调节点标识派生域（本切片未接节点，按 MLS 群键派生一个稳定占位）。
const COORDINATOR_DOMAIN: &[u8] = b"chat/groups/coordinator-id/v1";

/// 群操作稳定错误（码全部来自 PROTOCOL-014 冻结表或既有层；不新造协议码）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupError {
    code: &'static str,
    detail: String,
}

impl GroupError {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }

    /// 稳定错误码字符串。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 脱敏消息（不含正文/密钥）。
    pub fn detail(&self) -> &str {
        &self.detail
    }

    fn invalid_input(detail: &'static str) -> Self {
        Self::new("INVALID_INPUT", detail)
    }

    fn invalid_credential(detail: &'static str) -> Self {
        Self::new("INVALID_CREDENTIAL", detail)
    }

    fn invalid_candidate(detail: &'static str) -> Self {
        Self::new("INVALID_CANDIDATE", detail)
    }

    fn invalid_target(detail: &'static str) -> Self {
        Self::new("INVALID_TARGET", detail)
    }

    fn not_found() -> Self {
        Self::new("INVALID_TARGET", "group is not persisted in this database")
    }

    fn corrupted(detail: &'static str) -> Self {
        Self::new("STATE_CORRUPTED", detail)
    }
}

impl std::fmt::Display for GroupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for GroupError {}

impl From<StorageError> for GroupError {
    fn from(error: StorageError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}

impl From<ControlError> for GroupError {
    fn from(error: ControlError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}

impl From<crate::group_node::ControlReceiptError> for GroupError {
    fn from(error: crate::group_node::ControlReceiptError) -> Self {
        // 节点回执路径的稳定码原样透传（NODE_UNAVAILABLE / INVALID_RECEIPT_SCOPE / …）。
        Self::new(error.code(), error.detail().to_owned())
    }
}

impl From<DomainError> for GroupError {
    fn from(error: DomainError) -> Self {
        Self::new(error.code().as_str(), error.to_string())
    }
}

/// 群结果类型。
pub type GroupResult<T> = Result<T, GroupError>;

/// 一个成员的 KeyPackage（真实 MLS 字节）+ 调用方声明的凭据身份。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupMemberKeyPackage {
    /// 成员在群里的凭据身份（有界文本，不可与其它成员重复）。
    pub credential: String,
    /// 该成员的序列化 KeyPackage。
    pub key_package: Vec<u8>,
}

/// 引导成员：P0 引导要求**同进程**的设备句柄（crypto 目前只有这一条建群路径）。
pub struct GroupBootstrapPeer<'a> {
    /// 该成员在群里的凭据身份。
    pub credential: String,
    /// 同进程的 MLS 设备（其凭据必须与 KeyPackage 绑定一致，由 crypto 校验）。
    pub device: &'a MlsDevice,
}

impl std::fmt::Debug for GroupBootstrapPeer<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GroupBootstrapPeer")
            .field("credential", &self.credential)
            .finish_non_exhaustive()
    }
}

/// 建群请求（GROUPS-001 的本地形式）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateGroupRequest {
    /// 群名（1..=64 字节，不含控制字符，首尾不留空白）。
    pub name: String,
    /// 本机设备在群里的凭据身份（用于“自己加自己”检测与角色投影）。
    pub local_credential: String,
    /// 期望的群策略版本（本切片只接受 1）。
    pub policy_version: u32,
    /// 除引导成员外的其余成员（走 CRYPTO-005 Add 提交，结果仍是 pending）。
    pub members: Vec<GroupMemberKeyPackage>,
}

impl CreateGroupRequest {
    /// 只带名字与本地凭据的最小请求（追加成员为空）。
    pub fn new(name: impl Into<String>, local_credential: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            local_credential: local_credential.into(),
            policy_version: GROUP_POLICY_VERSION,
            members: Vec::new(),
        }
    }
}

/// 群成员投影（不含任何密钥材料）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupMemberView {
    /// 成员凭据身份。
    pub credential: String,
    /// 角色。
    pub role: MemberRole,
    /// 是否被禁言（GROUPS-004 的策略位）。
    pub muted: bool,
    /// 是否本机设备。
    pub is_local: bool,
    /// 是否仍处于 pending（追加成员在被协调者接受前不进入正式成员列表的语义位）。
    pub pending: bool,
}

/// pending 控制提交投影（只含公开绑定信息）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupPendingView {
    /// 操作种类（`add_members` / `self_update`）。
    pub operation: String,
    /// 提交基于的 epoch。
    pub base_epoch: u64,
    /// 接受后应到达的 epoch。
    pub target_epoch: u64,
    /// 预期新增成员数。
    pub added: u32,
    /// 预期移除成员数。
    pub removed: u32,
    /// 提交摘要（小写 hex）。
    pub commit_digest: String,
}

/// 建群结果 / 群快照投影（GROUPS-001~004 共用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupView {
    /// 协议群 ID（PROTOCOL-001 `grp:1:<hex>`，由 MLS 群键确定性派生）。
    pub group_id: String,
    /// 群名。
    pub name: String,
    /// 当前**已提交** MLS epoch（pending 接受前不推进）。
    pub epoch: u64,
    /// 群策略版本（每次被接受的策略操作 +1）。
    pub policy_version: u64,
    /// 正式控制头（小写 hex；下一次治理操作的 CAS 前置值）。
    pub control_head: String,
    /// 本机在群里的角色。
    pub role: MemberRole,
    /// 创建时间（Unix 毫秒）。
    pub created_at_ms: i64,
    /// MLS 群键（MLS group id 的小写十六进制；诊断与内部绑定用）。
    pub mls_group_key: String,
    /// 快照记录版本（每次写入 +1）。
    pub state_version: u64,
    /// 正式（已确认）成员列表，**不含** pending 成员。
    pub members: Vec<GroupMemberView>,
    /// pending 追加成员（声明身份；被接受后才会进入 `members`）。
    pub pending_members: Vec<GroupMemberView>,
    /// 已移除但**尚未合并**的成员：接受移除提交后彻底消失，丢弃则恢复为正式成员。
    pub pending_removed: Vec<GroupMemberView>,
    /// 未决控制提交（没有则为 `None`）。
    pub pending: Option<GroupPendingView>,
    /// 最近一次被丢弃的控制提交（CRYPTO-008 的审计投影）。
    pub last_control_cleared: Option<GroupControlClearedView>,
    /// 群是否已解散（GROUPS-012）。解散后所有治理操作显式拒绝，本地记录保留。
    pub dissolved: bool,
    /// 解散时间（Unix 毫秒；未解散为 `None`）。
    pub dissolved_at_ms: Option<i64>,
    /// 本机退出时间（Unix 毫秒；仍在群里为 `None`）。
    ///
    /// 退出是**本地策略层**语义：本机已从确认成员里移除，但 MLS 叶子仍留在群里
    /// （CRYPTO-005 的自移除护栏禁止 `remove_members` 移除本机设备），需协调者移除。
    pub left_at_ms: Option<i64>,
    /// 本机已提交**自移除提案**的时间（Unix 毫秒；未提交为 `None`）。
    ///
    /// 提案被对端打包成 Commit 并合并后才真正离群（那时 `self_removed` 为真）。
    pub leaving_at_ms: Option<i64>,
}

/// 被丢弃控制提交的投影（只含公开绑定信息与稳定原因码）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupControlClearedView {
    /// 被丢弃的操作种类（`add_members` / `remove_members` / `self_update`）。
    pub operation: String,
    /// 提交基于的 epoch。
    pub base_epoch: u64,
    /// 提交本应到达的 epoch。
    pub target_epoch: u64,
    /// 提交摘要（小写 hex，可与节点/协调者记录核对）。
    pub commit_digest: String,
    /// 稳定原因码（`operator_discarded` / `node_rejected` / `coordinator_timeout` / `operator_cancelled` / `stale_control_head`）。
    pub reason_code: String,
    /// 调用方给的脱敏说明。
    pub detail: String,
    /// 丢弃时间（Unix 毫秒）。
    pub cleared_at_ms: i64,
}

/// 持久快照记录体（storage 密封；本模块独占 `grpmeta/` 命名空间）。
#[derive(Clone, Debug, Serialize, Deserialize)]
struct GroupRecord {
    version: u8,
    group_id: String,
    mls_group_key: String,
    name: String,
    created_at_ms: i64,
    control_head: String,
    policy_version: u64,
    mls_epoch: u64,
    verified_sequence: u64,
    verified_log_hash: String,
    members: Vec<RecordMember>,
    pending_members: Vec<RecordMember>,
    /// 已被移除（本地策略已生效）但**尚未合并**的成员：接受移除提交后清空，丢弃则恢复。
    #[serde(default)]
    pending_removed: Vec<RecordMember>,
    /// 最近一次被丢弃的控制提交（CRYPTO-008）；只做审计，不参与策略版本。
    #[serde(default)]
    last_control_cleared: Option<RecordControlCleared>,
    /// 已解散（GROUPS-012）：治理全部拒绝，本地记录保留。
    #[serde(default)]
    dissolved: bool,
    /// 解散时间（Unix 毫秒；未解散为 0）。
    #[serde(default)]
    dissolved_at_ms: i64,
    /// 本机退出时间（Unix 毫秒；未退出为 0）。
    #[serde(default)]
    left_at_ms: i64,
    /// 本机已提交自移除提案的时间（Unix 毫秒；未提交为 0）。
    #[serde(default)]
    leaving_at_ms: i64,
    /// 待提交的独立提案（本机发起或对端收下；打包成 Commit 后清空）。
    #[serde(default)]
    pending_proposal: Option<RecordProposal>,
}

/// 待提交独立提案（公开绑定 + 可转发字节）。
#[derive(Clone, Debug, Serialize, Deserialize)]
struct RecordProposal {
    /// 提案作者凭据身份。
    author: String,
    /// 提案目标凭据身份。
    target: String,
    /// 提案所属 epoch。
    base_epoch: u64,
    /// 提案摘要（小写 hex）。
    digest: String,
    /// 序列化提案字节（小写 hex；提交到节点时使用）。
    proposal_hex: String,
    /// 记录时间（Unix 毫秒）。
    recorded_at_ms: i64,
}

/// 被丢弃的控制提交（只含公开绑定信息与稳定原因码）。
#[derive(Clone, Debug, Serialize, Deserialize)]
struct RecordControlCleared {
    operation: String,
    base_epoch: u64,
    target_epoch: u64,
    commit_digest: String,
    reason_code: String,
    detail: String,
    cleared_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RecordMember {
    credential: String,
    role: u64,
    muted: bool,
    is_local: bool,
    joined_sequence: u64,
}

impl GroupRecord {
    fn validate(&self) -> GroupResult<()> {
        if self.version != SNAPSHOT_VERSION {
            return Err(GroupError::corrupted(
                "group snapshot has an unsupported version",
            ));
        }
        validate_name(&self.name)?;
        if self.mls_group_key.is_empty() || self.mls_group_key.len() > 128 {
            return Err(GroupError::corrupted(
                "persisted group has an invalid MLS group key",
            ));
        }
        parse_group_id(&self.group_id)?;
        ControlHead::from_hex(&self.control_head)
            .map_err(|_| GroupError::corrupted("persisted control head is malformed"))?;
        Digest::from_hex(&self.verified_log_hash)
            .map_err(|_| GroupError::corrupted("persisted log hash is malformed"))?;
        if self.members.is_empty()
            || self.members.len() + self.pending_members.len() + self.pending_removed.len()
                > MAX_GROUP_MEMBERS
        {
            return Err(GroupError::corrupted(
                "persisted group member list is empty or oversized",
            ));
        }
        if self.dissolved != (self.dissolved_at_ms > 0) {
            return Err(GroupError::corrupted(
                "persisted group dissolve state and timestamp disagree",
            ));
        }
        if self.left_at_ms < 0 || self.leaving_at_ms < 0 {
            return Err(GroupError::corrupted(
                "persisted group has a negative leave timestamp",
            ));
        }
        if let Some(proposal) = &self.pending_proposal {
            validate_credential(&proposal.author)?;
            validate_credential(&proposal.target)?;
            if proposal.digest.len() != 64 {
                return Err(GroupError::corrupted(
                    "persisted pending proposal has a malformed digest",
                ));
            }
            if proposal.proposal_hex.is_empty() || proposal.proposal_hex.len() % 2 != 0 {
                return Err(GroupError::corrupted(
                    "persisted pending proposal has a malformed payload",
                ));
            }
        }
        if let Some(cleared) = &self.last_control_cleared {
            if cleared.operation.is_empty() || cleared.operation.len() > 32 {
                return Err(GroupError::corrupted(
                    "persisted control-clear note has an unknown operation",
                ));
            }
            if cleared.reason_code.is_empty() || cleared.reason_code.len() > 64 {
                return Err(GroupError::corrupted(
                    "persisted control-clear note has an invalid reason code",
                ));
            }
            if cleared.commit_digest.len() != 64 {
                return Err(GroupError::corrupted(
                    "persisted control-clear note has a malformed commit digest",
                ));
            }
        }
        let mut seen: Vec<&str> = Vec::new();
        let mut owners = 0usize;
        for member in self
            .members
            .iter()
            .chain(self.pending_members.iter())
            .chain(self.pending_removed.iter())
        {
            validate_credential(&member.credential)?;
            if seen.contains(&member.credential.as_str()) {
                return Err(GroupError::corrupted(
                    "persisted group repeats a member credential",
                ));
            }
            seen.push(&member.credential);
            let Some(role) = MemberRole::from_u64(member.role) else {
                return Err(GroupError::corrupted(
                    "persisted group member has an unknown role",
                ));
            };
            if role.is_owner() {
                owners += 1;
            }
            if member.joined_sequence == 0 {
                return Err(GroupError::corrupted(
                    "persisted group member joined at sequence 0",
                ));
            }
        }
        if owners != 1 {
            return Err(GroupError::corrupted(
                "persisted group must have exactly one owner",
            ));
        }
        Ok(())
    }
}

fn now_ms() -> i64 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    i64::try_from(millis).unwrap_or(i64::MAX)
}

fn validate_name(name: &str) -> GroupResult<()> {
    if name.is_empty() || name.len() > MAX_GROUP_NAME_BYTES {
        return Err(GroupError::invalid_input("group name must be 1..=64 bytes"));
    }
    if name.trim() != name {
        return Err(GroupError::invalid_input(
            "group name must not start or end with whitespace",
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(GroupError::invalid_input(
            "group name must not contain control characters",
        ));
    }
    Ok(())
}

fn validate_credential(credential: &str) -> GroupResult<()> {
    if credential.is_empty() || credential.len() > MAX_CREDENTIAL_BYTES {
        return Err(GroupError::invalid_credential(
            "member credential must be 1..=128 bytes",
        ));
    }
    if credential.chars().any(char::is_control) {
        return Err(GroupError::invalid_credential(
            "member credential must not contain control characters",
        ));
    }
    Ok(())
}

/// 快照记录在 storage 里的键（独立命名空间 + 协议群 ID 的摘要）。
///
/// 对外开放只用于**诊断/测试**：正常读写请走本模块的函数。
pub fn snapshot_key_for(group_id: &str) -> String {
    snapshot_group_id(group_id)
}

/// 快照记录的 storage group_id（独立命名空间 + 协议群 ID 的摘要）。
fn snapshot_group_id(group_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(group_id.as_bytes());
    let digest: [u8; 32] = hasher.finalize().into();
    format!("{SNAPSHOT_GROUP_PREFIX}{}", hex_lower(&digest))
}

fn domain_digest(domain: &[u8], text: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(text.as_bytes());
    hasher.finalize().into()
}

/// 成员凭据 → 本地账户标识（本切片的投影，不是真实账户 ID）。
fn account_id_for(credential: &str) -> AccountId {
    AccountId::from_bytes(domain_digest(ACCOUNT_DOMAIN, credential))
}

/// 成员凭据 → 本地设备标识（每成员一台派生设备）。
fn device_id_for(credential: &str) -> DeviceId {
    DeviceId::from_bytes(domain_digest(DEVICE_DOMAIN, credential))
}

/// MLS 群键 → 协调节点标识占位（本切片未接节点）。
fn coordinator_for(mls_group_key: &str) -> CoordinatorId {
    CoordinatorId::from_bytes(domain_digest(COORDINATOR_DOMAIN, mls_group_key))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: [u8; 16] = *b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[(byte >> 4) as usize]));
        text.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    text
}

/// 校验 PROTOCOL-001 `grp:1:<64 hex>` 群文本。
fn parse_group_id(group_id: &str) -> GroupResult<()> {
    GroupId::parse(group_id)
        .map(|_| ())
        .map_err(|_| GroupError::new("INVALID_ID", "group id must be PROTOCOL-001 group text"))
}

fn pending_view(pending: &PendingControl) -> GroupPendingView {
    GroupPendingView {
        operation: pending.operation.code().to_owned(),
        base_epoch: pending.base_epoch,
        target_epoch: pending.target_epoch,
        added: pending.added,
        removed: pending.removed,
        commit_digest: hex_lower(&pending.commit_digest),
    }
}

fn role_of(code: u64) -> MemberRole {
    MemberRole::from_u64(code).unwrap_or(MemberRole::Member)
}

/// 持久记录 → DOMAIN-001 正式快照（策略层只认这一份；不接受调用方自报字段）。
fn domain_snapshot(record: &GroupRecord) -> GroupResult<GroupSnapshot> {
    let group_id = GroupId::parse(&record.group_id)
        .map_err(|_| GroupError::corrupted("persisted group id is malformed"))?;
    let control_head = ControlHead::from_hex(&record.control_head)
        .map_err(|_| GroupError::corrupted("persisted control head is malformed"))?;
    let verified_log_hash = Digest::from_hex(&record.verified_log_hash)
        .map_err(|_| GroupError::corrupted("persisted log hash is malformed"))?;
    let mut members = Vec::with_capacity(record.members.len());
    for member in &record.members {
        members.push(GroupMember {
            account_id: account_id_for(&member.credential),
            role: role_of(member.role),
            devices: vec![device_id_for(&member.credential)],
            muted: member.muted,
            account_active: true,
            window: MembershipWindow::active(member.joined_sequence),
        });
    }
    let owner = members
        .iter()
        .find(|member| member.role.is_owner())
        .map(|member| member.account_id)
        .ok_or_else(|| GroupError::corrupted("persisted group has no owner member"))?;
    let snapshot = GroupSnapshot {
        id: group_id,
        kind: ConversationKind::Group,
        owner,
        members,
        control_head,
        policy_version: record.policy_version,
        mls_epoch: record.mls_epoch,
        verified_sequence: record.verified_sequence,
        verified_log_hash,
        coordinator: coordinator_for(&record.mls_group_key),
        dissolved: record.dissolved,
        paused_reason: None,
    };
    snapshot
        .validate_invariants()
        .map_err(|_| GroupError::corrupted("persisted group snapshot violates invariants"))?;
    Ok(snapshot)
}

/// DOMAIN-004 的候选策略 → 持久记录（角色/禁言/增减成员/版本/控制头全部来自候选快照）。
///
/// 候选策略里没有的确认成员视为“已被移除”（GROUPS-005）；pending 成员不在候选快照里，
/// 因此原样保留（它们还没被接受，不参与策略层）。
fn apply_domain_policy(record: &mut GroupRecord, policy: &GroupSnapshot) -> GroupResult<()> {
    record.control_head = policy.control_head.to_hex();
    record.policy_version = policy.policy_version;
    record.mls_epoch = policy.mls_epoch;
    record.verified_sequence = policy.verified_sequence;
    record.verified_log_hash = policy.verified_log_hash.to_hex();
    let mut confirmed = Vec::with_capacity(record.members.len());
    let mut removed = Vec::new();
    for member in &record.members {
        let account = account_id_for(&member.credential);
        let Some(updated) = policy.member(account) else {
            // 已被移除：本地策略立即生效（不出现在正式成员里），但保留在 pending_removed，
            // 以便控制提交被接受后彻底删除、被丢弃时恢复（CRYPTO-007/008）。
            removed.push(member.clone());
            continue;
        };
        let mut updated_member = member.clone();
        updated_member.role = updated.role.as_u64();
        updated_member.muted = updated.muted;
        confirmed.push(updated_member);
    }
    record.members = confirmed;
    if !removed.is_empty() {
        record.pending_removed = removed;
    }
    record.validate()
}

/// 提案摘要：域分隔 + 作者 + epoch + 提案字节（提交侧/对端核对用）。
fn proposal_digest(author: &str, epoch: u64, payload: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"chat/core/proposal-entry/v1");
    hasher.update(author.as_bytes());
    hasher.update(epoch.to_be_bytes());
    hasher.update(payload);
    hasher.finalize().into()
}

/// 已解散的群：所有治理操作显式 `INVALID_STATE_TRANSITION`（记录保留，绝不悄悄改状态）。
fn reject_dissolved(record: &GroupRecord) -> GroupResult<()> {
    if record.dissolved {
        return Err(GroupError::new(
            "INVALID_STATE_TRANSITION",
            "the group is dissolved; no further governance is possible",
        ));
    }
    Ok(())
}

/// 在已确认成员里按凭据定位下标（pending 成员不可治理）。
fn member_index(record: &GroupRecord, credential: &str) -> GroupResult<usize> {
    validate_credential(credential)?;
    record
        .members
        .iter()
        .position(|member| member.credential == credential)
        .ok_or_else(|| GroupError::invalid_target("target credential is not a confirmed member"))
}

fn actor_of(record: &GroupRecord, index: usize) -> Actor {
    let credential = record.members[index].credential.clone();
    Actor {
        account_id: account_id_for(&credential),
        device_id: device_id_for(&credential),
    }
}

/// 把持久记录与真实 MLS 状态合成对外投影。
fn project(
    record: &GroupRecord,
    epoch: u64,
    pending: Option<&PendingControl>,
    state_version: u64,
) -> GroupView {
    let members = record
        .members
        .iter()
        .map(|member| GroupMemberView {
            credential: member.credential.clone(),
            role: role_of(member.role),
            muted: member.muted,
            is_local: member.is_local,
            pending: false,
        })
        .collect();
    let pending_members = record
        .pending_members
        .iter()
        .map(|member| GroupMemberView {
            credential: member.credential.clone(),
            role: role_of(member.role),
            muted: member.muted,
            is_local: member.is_local,
            pending: true,
        })
        .collect();
    let pending_removed = record
        .pending_removed
        .iter()
        .map(|member| GroupMemberView {
            credential: member.credential.clone(),
            role: role_of(member.role),
            muted: member.muted,
            is_local: member.is_local,
            pending: false,
        })
        .collect();
    GroupView {
        group_id: record.group_id.clone(),
        name: record.name.clone(),
        epoch,
        policy_version: record.policy_version,
        control_head: record.control_head.clone(),
        role: record
            .members
            .iter()
            .find(|member| member.is_local)
            .map(|member| role_of(member.role))
            .unwrap_or(MemberRole::Member),
        created_at_ms: record.created_at_ms,
        mls_group_key: record.mls_group_key.clone(),
        state_version,
        members,
        pending_members,
        pending_removed,
        pending: pending.map(pending_view),
        dissolved: record.dissolved,
        dissolved_at_ms: (record.dissolved_at_ms > 0).then_some(record.dissolved_at_ms),
        left_at_ms: (record.left_at_ms > 0).then_some(record.left_at_ms),
        leaving_at_ms: (record.leaving_at_ms > 0).then_some(record.leaving_at_ms),
        last_control_cleared: record
            .last_control_cleared
            .as_ref()
            .map(|cleared| GroupControlClearedView {
                operation: cleared.operation.clone(),
                base_epoch: cleared.base_epoch,
                target_epoch: cleared.target_epoch,
                commit_digest: cleared.commit_digest.clone(),
                reason_code: cleared.reason_code.clone(),
                detail: cleared.detail.clone(),
                cleared_at_ms: cleared.cleared_at_ms,
            }),
    }
}

/// 读回持久快照记录（不存在 → INVALID_TARGET；损坏 → STATE_CORRUPTED）。
fn load_record(store: &Store, group_id: &str) -> GroupResult<(GroupRecord, u64)> {
    parse_group_id(group_id)?;
    let stored = store
        .load_mls_group_state(&snapshot_group_id(group_id))
        .map_err(GroupError::from)?
        .ok_or_else(GroupError::not_found)?;
    let record: GroupRecord = serde_json::from_slice(&stored.state)
        .map_err(|_| GroupError::corrupted("persisted group snapshot is not readable"))?;
    record.validate()?;
    if record.group_id != group_id {
        return Err(GroupError::corrupted(
            "persisted group snapshot belongs to another group id",
        ));
    }
    Ok((record, stored.state_version))
}

/// 写入快照记录（同一命名空间内单调 epoch；返回 storage 的 state_version）。
fn persist_snapshot(store: &mut Store, record: &GroupRecord, epoch: u64) -> GroupResult<u64> {
    record.validate()?;
    let payload = serde_json::to_vec(record)
        .map_err(|_| GroupError::corrupted("group snapshot could not be encoded"))?;
    if payload.len() > MAX_SNAPSHOT_BYTES {
        return Err(GroupError::new(
            "CONTENT_TOO_LARGE",
            "group snapshot exceeds the supported size",
        ));
    }
    let saved = store
        .save_mls_group_state(MlsGroupStateInput {
            group_id: snapshot_group_id(&record.group_id),
            epoch,
            state: payload,
            records: Vec::new(),
        })
        .map_err(GroupError::from)?;
    Ok(saved.state_version)
}

/// GROUPS-001：建立一个小群（创建者 + 引导成员），并为其余成员准备真实 Add 提交。
///
/// 语义：
/// 1. 名称/凭据/成员上限/自我加法/重复成员在**任何 MLS 或存储写入之前**校验；
/// 2. 用 P0 引导（`create_and_invite`）建立含创建者与引导成员的真实 MLS 群；
/// 3. 有其余成员时用 CRYPTO-005 `prepare_control(AddMembers)` 生成真实 Commit/Welcome，**pending 落库**；
/// 4. 快照（名称/成员/角色/禁言/策略头/版本）写入正式库的独立命名空间记录；
/// 5. 返回真实 epoch 与 pending 的群投影。
///
/// 本变体额外返回**引导成员的 Welcome 字节**：投递给对方后由 [group_join_from_welcome] 加入
/// （投递与重试属 NODE/SYNC 层，本切片只负责产出真实 Welcome）。
pub fn create_group_with_welcome(
    device: &mut MlsDevice,
    bootstrap_peer: &GroupBootstrapPeer<'_>,
    store: &mut Store,
    request: &CreateGroupRequest,
) -> GroupResult<(GroupView, Vec<u8>)> {
    // ---- 1) 写入前的全部校验 -------------------------------------------------
    validate_name(&request.name)?;
    validate_credential(&request.local_credential)?;
    validate_credential(&bootstrap_peer.credential)?;
    if request.policy_version != GROUP_POLICY_VERSION {
        return Err(GroupError::new(
            "INVALID_POLICY_TRANSITION",
            "only the current group policy version is supported",
        ));
    }
    if bootstrap_peer.credential == request.local_credential {
        return Err(GroupError::invalid_input(
            "the bootstrap member must not be the local device",
        ));
    }
    let total = 2usize
        .checked_add(request.members.len())
        .ok_or_else(|| GroupError::new("GROUP_LIMIT_EXCEEDED", "group member count overflows"))?;
    if total > MAX_GROUP_MEMBERS {
        return Err(GroupError::new(
            "GROUP_LIMIT_EXCEEDED",
            "group member count exceeds the configured limit",
        ));
    }
    let mut seen_credentials: Vec<&str> = vec![
        request.local_credential.as_str(),
        bootstrap_peer.credential.as_str(),
    ];
    let mut seen_packages: Vec<&[u8]> = Vec::new();
    for member in &request.members {
        validate_credential(&member.credential)?;
        if member.key_package.is_empty() {
            return Err(GroupError::invalid_candidate(
                "member key package must not be empty",
            ));
        }
        if seen_credentials.contains(&member.credential.as_str()) {
            return Err(GroupError::invalid_input(
                "a member credential appears more than once",
            ));
        }
        if seen_packages.contains(&member.key_package.as_slice()) {
            return Err(GroupError::invalid_input(
                "the same key package appears more than once",
            ));
        }
        seen_credentials.push(&member.credential);
        seen_packages.push(&member.key_package);
    }

    // ---- 2) P0 引导：创建者 + 首个成员 --------------------------------------
    let peer_key_package = bootstrap_peer.device.key_package().map_err(|_| {
        GroupError::invalid_candidate("bootstrap peer key package could not be built")
    })?;
    if seen_packages.contains(&peer_key_package.as_slice()) {
        return Err(GroupError::invalid_input(
            "the bootstrap peer is also listed as an additional member",
        ));
    }
    let welcome = device
        .create_and_invite(&peer_key_package, bootstrap_peer.device)
        .map_err(|_| {
            GroupError::invalid_candidate(
                "the MLS group could not be bootstrapped with the bootstrap peer",
            )
        })?;
    let mls_group_key = device
        .group_key()
        .map_err(|_| GroupError::corrupted("bootstrapped MLS group has no group key"))?;

    // ---- 3) 其余成员：真实 CRYPTO-005 Add 提交（pending） --------------------
    let pending = if request.members.is_empty() {
        device.export_to_store(store).map_err(|_| {
            GroupError::new(
                "STORAGE_FAILURE",
                "the MLS group state could not be persisted",
            )
        })?;
        None
    } else {
        let key_packages = request
            .members
            .iter()
            .map(|member| member.key_package.clone())
            .collect::<Vec<_>>();
        let pending = device
            .prepare_control(ControlOperation::AddMembers { key_packages }, store)
            .map_err(GroupError::from)?;
        Some(pending)
    };

    // ---- 4) 快照落库（独立命名空间；pending 不混入正式成员） ----------------
    let epoch = device
        .epoch()
        .map_err(|_| GroupError::corrupted("MLS group epoch is unavailable"))?;
    let group_id = protocol_group_id_for(&mls_group_key)
        .map_err(|_| GroupError::corrupted("protocol group id could not be derived"))?;
    let created_at_ms = now_ms();
    let record = GroupRecord {
        version: SNAPSHOT_VERSION,
        group_id: group_id.clone(),
        mls_group_key: mls_group_key.clone(),
        name: request.name.clone(),
        created_at_ms,
        // 新群的初始控制头是冻结的全零值；第一次治理操作由 DOMAIN-004 派生新头。
        control_head: ControlHead::ZERO.to_hex(),
        policy_version: 1,
        mls_epoch: epoch,
        verified_sequence: 0,
        verified_log_hash: Digest::ZERO.to_hex(),
        members: vec![
            RecordMember {
                credential: request.local_credential.clone(),
                role: MemberRole::Owner.as_u64(),
                muted: false,
                is_local: true,
                joined_sequence: 1,
            },
            RecordMember {
                credential: bootstrap_peer.credential.clone(),
                role: MemberRole::Member.as_u64(),
                muted: false,
                is_local: false,
                joined_sequence: 1,
            },
        ],
        pending_members: request
            .members
            .iter()
            .map(|member| RecordMember {
                credential: member.credential.clone(),
                role: MemberRole::Member.as_u64(),
                muted: false,
                is_local: false,
                joined_sequence: 1,
            })
            .collect(),
        pending_removed: Vec::new(),
        last_control_cleared: None,
        dissolved: false,
        dissolved_at_ms: 0,
        left_at_ms: 0,
        leaving_at_ms: 0,
        pending_proposal: None,
    };
    record.validate()?;
    // 建群时先自检策略快照不变量（全零头 + 唯一群主）。
    let _ = domain_snapshot(&record)?;
    let saved = persist_snapshot(store, &record, epoch)?;

    Ok((project(&record, epoch, pending.as_ref(), saved), welcome))
}

/// GROUPS-001（无 Welcome 版本）：建群并丢弃引导 Welcome（投递由上层负责）。
pub fn create_group(
    device: &mut MlsDevice,
    bootstrap_peer: &GroupBootstrapPeer<'_>,
    store: &mut Store,
    request: &CreateGroupRequest,
) -> GroupResult<GroupView> {
    create_group_with_welcome(device, bootstrap_peer, store, request).map(|(view, _)| view)
}

/// GROUPS-002：读取正式群快照（库离线也返回最后正式状态，不联网）。
///
/// - 快照记录不存在 → `INVALID_TARGET`（未知群，绝不臆造）；
/// - 记录存在但 MLS 状态无法恢复 → `STATE_CORRUPTED`；
/// - epoch 与 pending 以**真实 MLS 状态**为准（记录里的 epoch 只是写入时的值）。
pub fn group_snapshot(store: &Store, group_id: &str) -> GroupResult<GroupView> {
    let (record, state_version) = load_record(store, group_id)?;
    let device = MlsDevice::import_from_store(store, &record.mls_group_key).map_err(|_| {
        GroupError::corrupted("the persisted MLS group state could not be restored")
    })?;
    let epoch = device
        .epoch()
        .map_err(|_| GroupError::corrupted("restored MLS group epoch is unavailable"))?;
    let pending = device.pending_control().map_err(GroupError::from)?;
    if let Some(pending) = &pending
        && pending.base_epoch != epoch
    {
        return Err(GroupError::corrupted(
            "persisted MLS pending commit is bound to another epoch",
        ));
    }
    Ok(project(&record, epoch, pending.as_ref(), state_version))
}

/// GROUPS-003：变更成员角色（仅群主；不能改群主；Admin/Member 迁移白名单）。
///
/// 顺序（**非法操作在任何写入之前被拒**）：
/// 1. 读持久快照 → 构造 DOMAIN-001 正式快照（不变量校验）；
/// 2. 本机 MLS 设备必须是同一个群、同一 epoch（否则 `STALE_EPOCH` / `INVALID_TARGET`）；
/// 3. DOMAIN-002 `authorize_group_operation` 判权限（非群主 → `PERMISSION_DENIED`；
///    改群主/改自己 → `PERMISSION_DENIED`；目标不是成员 → `INVALID_TARGET`）；
/// 4. DOMAIN-004 `apply_policy_operation` 计算新 `policy_version` 与 `control_head`；
/// 5. CRYPTO-005 `prepare_control(SelfUpdate)` 生成真实 MLS 控制提交并写 pending；
/// 6. 快照写回（角色 + 策略版本 + 控制头），返回真实投影。
pub fn set_role(
    device: &mut MlsDevice,
    store: &mut Store,
    group_id: &str,
    actor_credential: &str,
    target_credential: &str,
    role: MemberRole,
) -> GroupResult<GroupView> {
    let (mut record, _) = load_record(store, group_id)?;
    reject_dissolved(&record)?;
    let snapshot = domain_snapshot(&record)?;
    let group_key = device
        .group_key()
        .map_err(|_| GroupError::invalid_target("the local device has no MLS group"))?;
    if group_key != record.mls_group_key {
        return Err(GroupError::invalid_target(
            "the local device is not in this group",
        ));
    }
    let epoch = device
        .epoch()
        .map_err(|_| GroupError::corrupted("MLS group epoch is unavailable"))?;
    if epoch != snapshot.mls_epoch {
        return Err(GroupError::new(
            "STALE_EPOCH",
            "the local MLS epoch does not match the persisted group",
        ));
    }
    let actor_index = member_index(&record, actor_credential)?;
    let target_index = member_index(&record, target_credential)?;
    let actor = actor_of(&record, actor_index);
    let target_account = account_id_for(&record.members[target_index].credential);
    let operation = GroupOperation::SetRole {
        account: target_account,
        role,
    };
    let authorized =
        authorize_group_operation(&snapshot, actor, operation).map_err(GroupError::from)?;
    let proposed: ProposedPolicy =
        apply_policy_operation(&snapshot, &authorized).map_err(GroupError::from)?;
    // 真实 MLS 控制提交（角色变更由控制提交承载；epoch 只在下一次接受合并时推进）。
    let pending = device
        .prepare_control(ControlOperation::SelfUpdate, store)
        .map_err(GroupError::from)?;
    apply_domain_policy(&mut record, &proposed.policy)?;
    let saved = persist_snapshot(store, &record, record.mls_epoch)?;
    Ok(project(&record, record.mls_epoch, Some(&pending), saved))
}

/// GROUPS-004：设置成员禁言（纯策略层，无 MLS 控制提交）。
///
/// 顺序：读快照 → 拒绝“有未决控制提交”（避免策略头与 commit 顺序不一致）→
/// DOMAIN-002 判权限（群主/管理员；不能禁言自己；目标必须是成员）→
/// DOMAIN-004 计算新 `policy_version`/`control_head` → 快照写回 `muted`。
pub fn set_muted(
    store: &mut Store,
    group_id: &str,
    actor_credential: &str,
    target_credential: &str,
    muted: bool,
) -> GroupResult<GroupView> {
    let (mut record, _) = load_record(store, group_id)?;
    reject_dissolved(&record)?;
    let snapshot = domain_snapshot(&record)?;
    let device = MlsDevice::import_from_store(store, &record.mls_group_key).map_err(|_| {
        GroupError::corrupted("the persisted MLS group state could not be restored")
    })?;
    if device
        .pending_control()
        .map_err(GroupError::from)?
        .is_some()
    {
        return Err(GroupError::new(
            "INVALID_STATE_TRANSITION",
            "a control commit is pending; wait for its acceptance before policy writes",
        ));
    }
    let actor_index = member_index(&record, actor_credential)?;
    let target_index = member_index(&record, target_credential)?;
    let actor = actor_of(&record, actor_index);
    let target_account = account_id_for(&record.members[target_index].credential);
    let operation = GroupOperation::SetMuted {
        account: target_account,
        muted,
    };
    let authorized =
        authorize_group_operation(&snapshot, actor, operation).map_err(GroupError::from)?;
    let proposed = apply_policy_operation(&snapshot, &authorized).map_err(GroupError::from)?;
    apply_domain_policy(&mut record, &proposed.policy)?;
    let saved = persist_snapshot(store, &record, record.mls_epoch)?;
    Ok(project(&record, record.mls_epoch, None, saved))
}

/// GROUPS-005：移除一名确认成员（群主或管理员；不能移除群主、不能移除自己）。
///
/// 顺序（**非法操作在任何写入之前被拒**）：
/// 1. 读持久快照 → DOMAIN-001 快照（不变量校验）；
/// 2. 本机 MLS 设备必须是同一个群、同一 epoch（`INVALID_TARGET` / `STALE_EPOCH`）；
/// 3. DOMAIN-002 授权（非群主/管理员 → `PERMISSION_DENIED`；移除自己 → `PERMISSION_DENIED`；
///    移除群主 → `PERMISSION_DENIED`；目标不是确认成员 → `INVALID_TARGET`）；
/// 4. DOMAIN-004 计算新 `policy_version`/`control_head`；
/// 5. CRYPTO-005 `prepare_control(RemoveMembers)` 生成真实 MLS 移除提交并写 pending；
/// 6. 快照写回（目标从正式成员里移除），返回真实投影。
pub fn remove_member(
    device: &mut MlsDevice,
    store: &mut Store,
    group_id: &str,
    actor_credential: &str,
    target_credential: &str,
) -> GroupResult<GroupView> {
    let (mut record, _) = load_record(store, group_id)?;
    reject_dissolved(&record)?;
    let snapshot = domain_snapshot(&record)?;
    let group_key = device
        .group_key()
        .map_err(|_| GroupError::invalid_target("the local device has no MLS group"))?;
    if group_key != record.mls_group_key {
        return Err(GroupError::invalid_target(
            "the local device is not in this group",
        ));
    }
    let epoch = device
        .epoch()
        .map_err(|_| GroupError::corrupted("MLS group epoch is unavailable"))?;
    if epoch != snapshot.mls_epoch {
        return Err(GroupError::new(
            "STALE_EPOCH",
            "the local MLS epoch does not match the persisted group",
        ));
    }
    let actor_index = member_index(&record, actor_credential)?;
    let target_index = member_index(&record, target_credential)?;
    let actor = actor_of(&record, actor_index);
    let target_account = account_id_for(&record.members[target_index].credential);
    let operation = GroupOperation::RemoveMember {
        account: target_account,
    };
    let authorized =
        authorize_group_operation(&snapshot, actor, operation).map_err(GroupError::from)?;
    let proposed: ProposedPolicy =
        apply_policy_operation(&snapshot, &authorized).map_err(GroupError::from)?;
    // 真实 MLS 移除提交：按凭据身份定位成员（crypto 内部会再校验唯一匹配/不能移除自己）。
    let pending = device
        .prepare_control(
            ControlOperation::RemoveMembers {
                members: vec![record.members[target_index].credential.clone()],
            },
            store,
        )
        .map_err(GroupError::from)?;
    apply_domain_policy(&mut record, &proposed.policy)?;
    let saved = persist_snapshot(store, &record, record.mls_epoch)?;
    Ok(project(&record, record.mls_epoch, Some(&pending), saved))
}

/// GROUPS-006：修改群名（群主或管理员；纯策略层，无 MLS 成员变化）。
///
/// - 名称 1..=64 字节、无控制字符、首尾无空白（否则 `INVALID_INPUT`）；
/// - **同名幂等无操作**：名称未变时直接返回当前快照，不写库、不推进策略版本；
/// - 有未决控制提交时显式拒绝（避免策略头与提交顺序不一致）；
/// - 否则 DOMAIN-002/004 计算新 `policy_version`/`control_head`，快照写回新名称。
pub fn rename_group(
    store: &mut Store,
    group_id: &str,
    actor_credential: &str,
    new_name: &str,
) -> GroupResult<GroupView> {
    validate_name(new_name)?;
    let (mut record, state_version) = load_record(store, group_id)?;
    reject_dissolved(&record)?;
    let snapshot = domain_snapshot(&record)?;
    let device = MlsDevice::import_from_store(store, &record.mls_group_key).map_err(|_| {
        GroupError::corrupted("the persisted MLS group state could not be restored")
    })?;
    if device
        .pending_control()
        .map_err(GroupError::from)?
        .is_some()
    {
        return Err(GroupError::new(
            "INVALID_STATE_TRANSITION",
            "a control commit is pending; wait for its acceptance before policy writes",
        ));
    }
    let actor_index = member_index(&record, actor_credential)?;
    // 同名幂等：不写库、不推进策略版本（调用方重放不会产生额外版本）。
    if record.name == new_name {
        return Ok(project(&record, record.mls_epoch, None, state_version));
    }
    let actor = actor_of(&record, actor_index);
    let operation = GroupOperation::Rename {
        name_digest: Digest::sha256(new_name.as_bytes()),
    };
    let authorized =
        authorize_group_operation(&snapshot, actor, operation).map_err(GroupError::from)?;
    let proposed: ProposedPolicy =
        apply_policy_operation(&snapshot, &authorized).map_err(GroupError::from)?;
    apply_domain_policy(&mut record, &proposed.policy)?;
    record.name = new_name.to_owned();
    let saved = persist_snapshot(store, &record, record.mls_epoch)?;
    Ok(project(&record, record.mls_epoch, None, saved))
}

/// 从正式库恢复该群的本地 MLS 设备。
///
/// 接受/丢弃之后，调用方手里的内存设备可能已过期（丢弃只改库与恢复出的设备）：
/// 需要继续治理时用本函数重新装载，绝不复用手持的旧状态。
pub fn load_group_device(store: &Store, group_id: &str) -> GroupResult<MlsDevice> {
    let (record, _) = load_record(store, group_id)?;
    MlsDevice::import_from_store(store, &record.mls_group_key).map_err(|_| {
        GroupError::corrupted("the persisted MLS group state could not be restored")
    })
}

/// 丢弃原因 → 稳定原因码（crypto 的拒绝证据只接受静态短语）。
fn rejection_code(reason: &str) -> &'static str {
    match reason {
        "node_rejected" | "coordinator_rejected" => "node_rejected",
        "coordinator_timeout" | "timeout" => "coordinator_timeout",
        "stale_control_head" | "stale_head" => "stale_control_head",
        "operator_cancelled" | "cancelled" => "operator_cancelled",
        _ => "operator_discarded",
    }
}

/// 校验调用方给的丢弃说明（有界、无控制字符；只做诊断，绝不进入节点公开元数据）。
fn validate_reason_detail(reason: &str) -> GroupResult<String> {
    let trimmed = reason.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_CREDENTIAL_BYTES {
        return Err(GroupError::invalid_input(
            "discard reason must be 1..=128 bytes",
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(GroupError::invalid_input(
            "discard reason must not contain control characters",
        ));
    }
    Ok(trimmed.to_owned())
}

/// 从正式库恢复的 pending 证据（与调用方设备交叉校验后才用于合并/丢弃）。
fn persisted_pending(
    store: &Store,
    record: &GroupRecord,
) -> GroupResult<(MlsDevice, PendingControl)> {
    let device = MlsDevice::import_from_store(store, &record.mls_group_key).map_err(|_| {
        GroupError::corrupted("the persisted MLS group state could not be restored")
    })?;
    let pending = device
        .pending_control()
        .map_err(GroupError::from)?
        .ok_or_else(|| {
            GroupError::new(
                "PENDING_MISMATCH",
                "no control commit is persisted for this group",
            )
        })?;
    Ok((device, pending))
}

/// CRYPTO-007 接线：接受并合并该群当前的控制提交。
///
/// 语义（**本地接受证据 ≠ 节点回执**）：
/// - 接受证据**只来自正式库**的持久 pending 记录（group/base_epoch/target_epoch/提交摘要），
///   不臆造字段；只有在调用方设备持有**同一个**提交时才合并（不一致 → `STATE_CORRUPTED`）；
/// - 合并成功后 epoch 前进到 `target_epoch`、pending 清空；
///   `add_members` 的 pending 成员在此刻**正式入群**，`remove_members` 的待移除成员被彻底删除；
/// - 合并失败（含第二次接受）保持 pending 与旧 epoch（无半更新）；
/// - 本函数**不代表节点已接受**：真实部署必须先拿到节点/协调者的接受回执（NODE 侧），
///   本轮把它作为本地一致性收口（见 groups.md 的限制说明）。
pub fn accept_pending_control(
    device: &mut MlsDevice,
    store: &mut Store,
    group_id: &str,
) -> GroupResult<GroupView> {
    let (mut record, _) = load_record(store, group_id)?;
    reject_dissolved(&record)?;
    let group_key = device
        .group_key()
        .map_err(|_| GroupError::invalid_target("the local device has no MLS group"))?;
    if group_key != record.mls_group_key {
        return Err(GroupError::invalid_target(
            "the local device is not in this group",
        ));
    }
    let (_persisted, stored_pending) = persisted_pending(store, &record)?;
    let live_pending = device
        .pending_control()
        .map_err(GroupError::from)?
        .ok_or_else(|| {
            GroupError::new(
                "PENDING_MISMATCH",
                "the local device holds no pending control commit",
            )
        })?;
    if live_pending != stored_pending {
        return Err(GroupError::corrupted(
            "persisted and local pending control commits differ",
        ));
    }
    let acceptance = stored_pending.acceptance();
    let merged = device
        .merge_accepted_pending(&acceptance, store)
        .map_err(GroupError::from)?;
    // 合并后的真实值：epoch 前进；pending 清空（记录里也不需要 pending 副本）。
    record.mls_epoch = merged.new_epoch;
    match merged.operation.code() {
        "add_members" => {
            // 追加成员在此刻正式入群（角色按创建时的声明）。
            let promoted: Vec<RecordMember> = record.pending_members.drain(..).collect();
            record.members.extend(promoted);
        }
        "remove_members" => {
            // 移除在此刻彻底生效：待移除列表清空（成员不再回来）。
            record.pending_removed.clear();
        }
        _ => {}
    }
    // 接受/合并是对既有策略记录的确认，不改变 policy_version（也不回退）。
    let saved = persist_snapshot(store, &record, record.mls_epoch)?;
    Ok(project(&record, merged.new_epoch, None, saved))
}

/// CRYPTO-008 接线：丢弃当前 pending 控制提交，回滚到上一完整状态。
///
/// 语义：
/// - 拒绝证据同样只来自正式库的持久 pending 记录（绑定 group/base_epoch/提交摘要）；
/// - 丢弃后 epoch **不变**，pending 清空；`policy_version 不回退`（本地策略记录保留），
///   并写入 `last_control_cleared` 审计记录（操作/epoch/摘要/原因码/说明/时间）；
/// - `add_members` 的 pending 成员从未入群 → 直接丢弃；`remove_members` 的待移除成员**恢复**为正式成员；
/// - 丢弃失败（含没有 pending 时）保持现状，不留半更新。
pub fn discard_pending_control(
    store: &mut Store,
    group_id: &str,
    reason: &str,
) -> GroupResult<GroupView> {
    let detail = validate_reason_detail(reason)?;
    let (mut record, _) = load_record(store, group_id)?;
    reject_dissolved(&record)?;
    let (mut device, stored_pending) = persisted_pending(store, &record)?;
    let rejection = stored_pending.rejection(rejection_code(reason));
    let discarded = device
        .discard_rejected_pending(&rejection, store)
        .map_err(GroupError::from)?;
    if discarded.epoch != stored_pending.base_epoch {
        return Err(GroupError::corrupted(
            "discarding a control commit changed the local epoch",
        ));
    }
    match discarded.operation.code() {
        "add_members" => record.pending_members.clear(),
        "remove_members" => {
            // 回滚：本地策略已生效的移除被撤销，成员恢复为正式成员。
            let restored: Vec<RecordMember> = record.pending_removed.drain(..).collect();
            record.members.extend(restored);
        }
        _ => {}
    }
    record.last_control_cleared = Some(RecordControlCleared {
        operation: discarded.operation.code().to_owned(),
        base_epoch: stored_pending.base_epoch,
        target_epoch: stored_pending.target_epoch,
        commit_digest: hex_lower(&stored_pending.commit_digest),
        reason_code: discarded.reason.to_owned(),
        detail,
        cleared_at_ms: now_ms(),
    });
    let saved = persist_snapshot(store, &record, record.mls_epoch)?;
    Ok(project(&record, discarded.epoch, None, saved))
}
/// 对端条目的输入（外层已验证的公开绑定字段；不含任何密钥）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerEntryInput {
    /// 协议群 ID（PROTOCOL-001 `grp:1:<hex>`，与本机快照一致）。
    pub group_id: String,
    /// 外层已验证的条目 epoch（必须等于本机快照 epoch）。
    pub epoch: u64,
    /// 外层已验证的作者凭据身份（crypto 会与 MLS 认证作者比对）。
    pub sender_identity: String,
    /// 对端 Commit 的预期成员变化（应用消息为 `None`）。
    pub expectation: Option<CommitExpectation>,
    /// MLS 消息字节（应用消息或 Commit）。
    pub payload: Vec<u8>,
    /// 外层已验证控制元数据里的**新增成员身份**（数量必须等于真实 Commit 的新增数）。
    pub added_members: Vec<String>,
    /// 外层已验证控制元数据里的**移除成员身份**（数量必须等于真实 Commit 的移除数）。
    pub removed_members: Vec<String>,
}

impl PeerEntryInput {
    /// 只带绑定字段的构造（成员增删默认空）。
    pub fn new(
        group_id: impl Into<String>,
        epoch: u64,
        sender_identity: impl Into<String>,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            group_id: group_id.into(),
            epoch,
            sender_identity: sender_identity.into(),
            expectation: None,
            payload,
            added_members: Vec::new(),
            removed_members: Vec::new(),
        }
    }
}

/// 对端条目的处理结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerEntryOutcome {
    /// 对端 Commit 已合并：epoch 恰好 +1，成员增删来自真实 StagedCommit 计数。
    Commit {
        /// 合并前 epoch。
        old_epoch: u64,
        /// 合并后 epoch。
        new_epoch: u64,
        /// 真实新增成员数。
        added: u32,
        /// 真实移除成员数。
        removed: u32,
        /// 真实更新数。
        updated: u32,
        /// 该 Commit 是否移除了本设备。
        self_removed: bool,
        /// 合并后的群投影。
        view: GroupView,
    },
    /// 应用消息已认证解密（明文只交给可信核心路径，不进入普通 DTO）。
    Application {
        /// 已认证作者。
        sender_identity: String,
        /// 所属 epoch。
        epoch: u64,
        /// 明文。
        plaintext: Vec<u8>,
    },
    /// 独立提案已暂存（等待后续 Commit）。
    Proposal {
        /// 已认证作者。
        sender_identity: String,
        /// 所属 epoch。
        epoch: u64,
        /// 当前群投影（未变化）。
        view: GroupView,
    },
    /// 收到的是本机 pending 提交的回显：应改用 `accept_pending_control`。
    OwnPendingCommit {
        /// 该提交所属 epoch。
        epoch: u64,
    },
}

/// 把真实 MLS 成员身份写回持久记录（只用于加入/对端 Commit 的真实成员集合）。
fn record_members_from_identities(
    identities: &[String],
    owner_credential: &str,
    local_credential: &str,
    joined_sequence: u64,
) -> Vec<RecordMember> {
    identities
        .iter()
        .map(|identity| RecordMember {
            credential: identity.clone(),
            role: if identity == owner_credential {
                MemberRole::Owner.as_u64()
            } else {
                MemberRole::Member.as_u64()
            },
            muted: false,
            is_local: identity == local_credential,
            joined_sequence: joined_sequence.max(1),
        })
        .collect()
}

/// GROUPS-013：处理对端条目（CRYPTO-006 `process_verified_entry`）。
///
/// 语义：
/// - 外层已验证的绑定字段（群 ID/epoch/作者/预期成员变化/成员身份）由调用方提供，
///   crypto 会用独密的 MLS 认证复核**群、epoch、作者与真实成员变化计数**；
/// - 本层额外要求「声明的成员身份数量 == 预期计数」，因此成员增删既是真实的（来自
///   StagedCommit 计数）又可按身份写回快照；
/// - 对端 Commit 合并后 epoch **恰好 +1**，随后把 MLS 状态导出到正式库并更新快照的
///   epoch/成员（不臆造任何字段）；应用消息与提案不改变快照；
/// - 任何失败（群不符/epoch 不符或乱序/作者不符/成员变化不符/认证失败）都显式返回冻结码，
///   快照保持上一次完整状态（不留半状态）。
pub fn group_process_peer_entry(
    device: &mut MlsDevice,
    store: &mut Store,
    input: &PeerEntryInput,
) -> GroupResult<PeerEntryOutcome> {
    let (mut record, state_version) = load_record(store, &input.group_id)?;
    reject_dissolved(&record)?;
    let group_key = device
        .group_key()
        .map_err(|_| GroupError::invalid_target("the local device has no MLS group"))?;
    if group_key != record.mls_group_key {
        return Err(GroupError::invalid_target(
            "the local device is not in this group",
        ));
    }
    validate_credential(&input.sender_identity)?;
    if input.payload.is_empty() {
        return Err(GroupError::invalid_candidate("entry payload must not be empty"));
    }
    if input.epoch != record.mls_epoch {
        let code = if input.epoch < record.mls_epoch {
            "STALE_EPOCH"
        } else {
            "MISSING_LOG_ENTRIES"
        };
        return Err(GroupError::new(code, "entry epoch does not match the local snapshot"));
    }
    // 预期计数与声明身份必须在合并之前一致（否则会在合并后才发现不一致 → 半状态）。
    let (expected_added, expected_removed) = match input.expectation {
        Some(expectation) => (expectation.added as usize, expectation.removed as usize),
        None => (0, 0),
    };
    if expected_added != input.added_members.len() || expected_removed != input.removed_members.len() {
        return Err(GroupError::new(
            "INVALID_CONTROL_RESULT",
            "declared member identities do not match the expected member changes",
        ));
    }
    for credential in input.added_members.iter().chain(input.removed_members.iter()) {
        validate_credential(credential)?;
    }
    let entry = VerifiedGroupEntry {
        group_id: record.mls_group_key.clone(),
        epoch: input.epoch,
        sender_identity: input.sender_identity.clone(),
        expectation: input.expectation,
        payload: input.payload.clone(),
    };
    let processed = device
        .process_verified_entry(&entry)
        .map_err(GroupError::from)?;
    match processed {
        ProcessedEntry::Commit {
            old_epoch,
            new_epoch,
            added,
            removed,
            updated,
            self_removed,
            ..
        } => {
            if new_epoch != old_epoch.saturating_add(1) {
                return Err(GroupError::corrupted(
                    "a merged peer commit must advance the epoch by exactly one",
                ));
            }
            if added as usize != input.added_members.len()
                || removed as usize != input.removed_members.len()
            {
                return Err(GroupError::new(
                    "INVALID_CONTROL_RESULT",
                    "the merged commit member changes do not match the declared identities",
                ));
            }
            // MLS 状态已在内存合并：先导出到正式库，再更新快照（失败时快照保持旧值）。
            device.export_to_store(store).map_err(|_| {
                GroupError::new(
                    "STORAGE_FAILURE",
                    "the merged MLS group state could not be persisted",
                )
            })?;
            for credential in &input.removed_members {
                record.members.retain(|member| &member.credential != credential);
            }
            if self_removed {
                record.members.retain(|member| !member.is_local);
            }
            let joined_sequence = new_epoch.max(1);
            for credential in &input.added_members {
                if record
                    .members
                    .iter()
                    .any(|member| &member.credential == credential)
                {
                    return Err(GroupError::corrupted(
                        "the merged commit adds a member that already exists locally",
                    ));
                }
                record.members.push(RecordMember {
                    credential: credential.clone(),
                    role: MemberRole::Member.as_u64(),
                    muted: false,
                    is_local: false,
                    joined_sequence,
                });
            }
            record.mls_epoch = new_epoch;
            record.validate()?;
            let saved = persist_snapshot(store, &record, record.mls_epoch)?;
            Ok(PeerEntryOutcome::Commit {
                old_epoch,
                new_epoch,
                added,
                removed,
                updated,
                self_removed,
                view: project(&record, new_epoch, None, saved),
            })
        }
        ProcessedEntry::Application {
            sender_identity,
            epoch,
            plaintext,
        } => Ok(PeerEntryOutcome::Application {
            sender_identity,
            epoch,
            plaintext,
        }),
        ProcessedEntry::Proposal {
            sender_identity,
            epoch,
        } => Ok(PeerEntryOutcome::Proposal {
            sender_identity,
            epoch,
            view: project(&record, record.mls_epoch, None, state_version),
        }),
        ProcessedEntry::OwnPendingCommit { epoch } => {
            Ok(PeerEntryOutcome::OwnPendingCommit { epoch })
        }
    }
}

/// GROUPS-004（加入侧）：用 Welcome 加入群并写入正式快照（CRYPTO-009 `join_from_welcome`）。
///
/// - `acceptance` 是外层已验证的邀请/日志绑定（群 ID/epoch/成员集合），crypto 会逐项复核；
/// - 名称由调用方给（邀请里没有规范化群名），成员与 epoch 全部取**真实 MLS 值**；
/// - `owner_credential` 来自已验证的邀请元数据（群主是谁），本层校验它确实在成员集合里；
/// - 同一群重复加入显式 `INVALID_STATE_TRANSITION`；Welcome 绑定不符 → `WELCOME_NOT_AUTHORIZED`
///   且不写入任何快照（不留半加入状态）。
#[allow(clippy::too_many_arguments)]
pub fn group_join_from_welcome(
    device: &mut MlsDevice,
    store: &mut Store,
    name: &str,
    welcome: &[u8],
    acceptance: &WelcomeAcceptance,
    local_credential: &str,
    owner_credential: &str,
) -> GroupResult<GroupView> {
    validate_name(name)?;
    validate_credential(local_credential)?;
    validate_credential(owner_credential)?;
    let joined = device
        .join_from_welcome(welcome, acceptance, store)
        .map_err(GroupError::from)?;
    let group_id = protocol_group_id_for(&joined.group_id)
        .map_err(|_| GroupError::corrupted("protocol group id could not be derived"))?;
    if load_record(store, &group_id).is_ok() {
        return Err(GroupError::new(
            "INVALID_STATE_TRANSITION",
            "this device already joined the group",
        ));
    }
    if joined.members.is_empty() {
        return Err(GroupError::corrupted("joined MLS group reports no members"));
    }
    let local_present = joined
        .members
        .iter()
        .any(|identity| identity == local_credential);
    if !local_present {
        return Err(GroupError::corrupted(
            "the local credential is not an MLS member of the joined group",
        ));
    }
    let owner_present = joined
        .members
        .iter()
        .any(|identity| identity == owner_credential);
    if !owner_present {
        return Err(GroupError::new(
            "INVALID_CONTROL_RESULT",
            "the declared owner is not an MLS member of the joined group",
        ));
    }
    let created_at_ms = now_ms();
    let record = GroupRecord {
        version: SNAPSHOT_VERSION,
        group_id: group_id.clone(),
        mls_group_key: joined.group_id.clone(),
        name: name.to_owned(),
        created_at_ms,
        control_head: ControlHead::ZERO.to_hex(),
        policy_version: 1,
        mls_epoch: joined.epoch,
        verified_sequence: 0,
        verified_log_hash: Digest::ZERO.to_hex(),
        members: record_members_from_identities(
            &joined.members,
            owner_credential,
            local_credential,
            joined.epoch,
        ),
        pending_members: Vec::new(),
        pending_removed: Vec::new(),
        last_control_cleared: None,
        dissolved: false,
        dissolved_at_ms: 0,
        left_at_ms: 0,
        leaving_at_ms: 0,
        pending_proposal: None,
    };
    record.validate()?;
    let _ = domain_snapshot(&record)?;
    let saved = persist_snapshot(store, &record, record.mls_epoch)?;
    Ok(project(&record, joined.epoch, None, saved))
}
/// GROUPS-010：转让群主（仅现群主；目标是确认成员且不是自己）。
///
/// 顺序（**非法操作在任何写入之前被拒**）：
/// 1. 读持久快照 → 拒绝已解散的群（`INVALID_STATE_TRANSITION`）；
/// 2. 本机 MLS 设备必须是同一个群、同一 epoch（`INVALID_TARGET` / `STALE_EPOCH`）；
/// 3. DOMAIN-007 授权（非群主 / 转给自己 / 目标不是成员 / 目标已撤销 → 冻结码）；
/// 4. DOMAIN-004 计算新 `policy_version`/`control_head` 并校验**唯一群主**不变量；
/// 5. CRYPTO-005 `prepare_control(SelfUpdate)` 生成真实 MLS 控制提交并落 pending；
/// 6. 快照角色互换（目标 = owner，原群主 = member）后写回，返回真实投影。
///
/// 注意：MLS 侧把这次转让表达为 `SelfUpdate`（成员叶子不变、只换策略角色）。节点公开元数据
/// 目前把 `SelfUpdate` 映射为 `set_role`，因此转让/退出的节点元数据需要后续扩展（见 groups.md 限制）。
pub fn transfer_ownership(
    device: &mut MlsDevice,
    store: &mut Store,
    group_id: &str,
    actor_credential: &str,
    target_credential: &str,
) -> GroupResult<GroupView> {
    let (mut record, _) = load_record(store, group_id)?;
    reject_dissolved(&record)?;
    let snapshot = domain_snapshot(&record)?;
    let group_key = device
        .group_key()
        .map_err(|_| GroupError::invalid_target("the local device has no MLS group"))?;
    if group_key != record.mls_group_key {
        return Err(GroupError::invalid_target(
            "the local device is not in this group",
        ));
    }
    let epoch = device
        .epoch()
        .map_err(|_| GroupError::corrupted("MLS group epoch is unavailable"))?;
    if epoch != snapshot.mls_epoch {
        return Err(GroupError::new(
            "STALE_EPOCH",
            "the local MLS epoch does not match the persisted group",
        ));
    }
    let actor_index = member_index(&record, actor_credential)?;
    let target_index = member_index(&record, target_credential)?;
    let actor = actor_of(&record, actor_index);
    let target_account = account_id_for(&record.members[target_index].credential);
    let operation = GroupOperation::TransferOwnership {
        target: target_account,
    };
    let authorized =
        authorize_group_operation(&snapshot, actor, operation).map_err(GroupError::from)?;
    let proposed: ProposedPolicy =
        apply_policy_operation(&snapshot, &authorized).map_err(GroupError::from)?;
    let pending = device
        .prepare_control(ControlOperation::SelfUpdate, store)
        .map_err(GroupError::from)?;
    apply_domain_policy(&mut record, &proposed.policy)?;
    let saved = persist_snapshot(store, &record, record.mls_epoch)?;
    Ok(project(&record, record.mls_epoch, Some(&pending), saved))
}

/// GROUPS-011：退出群（非群主；群主必须先转让，否则 `PERMISSION_DENIED`）。
///
/// **本地策略层退出**：把本机从确认成员里移除、记录 `left_at_ms`，此后本机对该群的治理一律
/// `INVALID_TARGET`（不再是确认成员）。MLS 自移除提交被 CRYPTO-005 的自移除护栏拒绝
/// （`remove_members` 不允许移除本机设备），因此本函数**不生成** MLS 提交：
/// 本机 MLS 叶子仍在群里，需要协调者/其他群主发起移除才能彻底离开（见 groups.md 限制）。
pub fn leave_group(
    device: &mut MlsDevice,
    store: &mut Store,
    group_id: &str,
    actor_credential: &str,
) -> GroupResult<GroupView> {
    let mut record = load_record(store, group_id)?.0;
    reject_dissolved(&record)?;
    if record.left_at_ms > 0 {
        return Err(GroupError::new(
            "INVALID_STATE_TRANSITION",
            "the local device has already left this group",
        ));
    }
    let snapshot = domain_snapshot(&record)?;
    let group_key = device
        .group_key()
        .map_err(|_| GroupError::invalid_target("the local device has no MLS group"))?;
    if group_key != record.mls_group_key {
        return Err(GroupError::invalid_target(
            "the local device is not in this group",
        ));
    }
    let epoch = device
        .epoch()
        .map_err(|_| GroupError::corrupted("MLS group epoch is unavailable"))?;
    if epoch != snapshot.mls_epoch {
        return Err(GroupError::new(
            "STALE_EPOCH",
            "the local MLS epoch does not match the persisted group",
        ));
    }
    let actor_index = member_index(&record, actor_credential)?;
    let actor = actor_of(&record, actor_index);
    // 群主必须先转让：这里显式给出冻结码 PERMISSION_DENIED（domain 的 OWNER_TRANSFER_REQUIRED
    // 也会拒绝，但对外契约要求这一个码）。
    if record.members[actor_index].role == MemberRole::Owner.as_u64() {
        return Err(GroupError::new(
            "PERMISSION_DENIED",
            "the owner must transfer ownership before leaving the group",
        ));
    }
    let operation = GroupOperation::Leave;
    let authorized =
        authorize_group_operation(&snapshot, actor, operation).map_err(GroupError::from)?;
    let proposed: ProposedPolicy =
        apply_policy_operation(&snapshot, &authorized).map_err(GroupError::from)?;
    // 本地策略立即生效：本机从确认成员里消失（pending_removed 保留以便审计/恢复）。
    apply_domain_policy(&mut record, &proposed.policy)?;
    record.left_at_ms = now_ms();
    record.validate()?;
    let saved = persist_snapshot(store, &record, record.mls_epoch)?;
    Ok(project(&record, record.mls_epoch, None, saved))
}

/// GROUPS-012：解散群（仅群主；纯策略层，不删本地记录）。
///
/// - 只有群主能解散（其他成员 `PERMISSION_DENIED`）；
/// - 已解散 → `INVALID_STATE_TRANSITION`（幂等调用也显式失败，不谎报"又解散了一次"）；
/// - 有未决控制提交时拒绝（先用接受/丢弃收口，避免留下悬空提交）；
/// - DOMAIN-004 计算新 `policy_version`/`control_head`（`dissolve` 操作会把快照标为
///   dissolved），随后写入 `dissolved = true` 与 `dissolved_at_ms`；
/// - 记录**保留**（成员、名称、epoch 都还在），只是所有治理从此显式拒绝。
pub fn dissolve_group(
    store: &mut Store,
    group_id: &str,
    actor_credential: &str,
) -> GroupResult<GroupView> {
    let (mut record, _) = load_record(store, group_id)?;
    reject_dissolved(&record)?;
    let snapshot = domain_snapshot(&record)?;
    let device = MlsDevice::import_from_store(store, &record.mls_group_key).map_err(|_| {
        GroupError::corrupted("the persisted MLS group state could not be restored")
    })?;
    if device
        .pending_control()
        .map_err(GroupError::from)?
        .is_some()
    {
        return Err(GroupError::new(
            "INVALID_STATE_TRANSITION",
            "a control commit is pending; resolve it before dissolving the group",
        ));
    }
    let actor_index = member_index(&record, actor_credential)?;
    let actor = actor_of(&record, actor_index);
    let authorized = authorize_group_operation(&snapshot, actor, GroupOperation::Dissolve)
        .map_err(GroupError::from)?;
    let proposed: ProposedPolicy =
        apply_policy_operation(&snapshot, &authorized).map_err(GroupError::from)?;
    apply_domain_policy(&mut record, &proposed.policy)?;
    record.dissolved = true;
    record.dissolved_at_ms = now_ms();
    record.validate()?;
    let saved = persist_snapshot(store, &record, record.mls_epoch)?;
    Ok(project(&record, record.mls_epoch, None, saved))
}
/// 自移除提案的投影（只含可转发字节与公开绑定信息）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaveProposalView {
    /// 协议群 ID。
    pub group_id: String,
    /// 生成提案时的本地 epoch（提案必须在该 epoch 被提交覆盖）。
    pub base_epoch: u64,
    /// 当前真实 epoch（生成提案不推进 epoch）。
    pub epoch: u64,
    /// 预期移除的成员数（恒为 1：只有本机）。
    pub removed: u32,
    /// 是否携带 Welcome（恒为 false）。
    pub has_welcome: bool,
    /// 序列化的 MLS 提案字节（交给另一名成员提交）。
    pub proposal: Vec<u8>,
    /// 提案摘要（小写 hex，绑定 group/base_epoch/提案字节）。
    pub proposal_digest: String,
    /// 本机标记离开的时间（Unix 毫秒）。
    pub leaving_at_ms: i64,
    /// 写入后的快照版本。
    pub state_version: u64,
}

/// 对端独立提案的输入（外层已验证的公开绑定字段）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalEntryInput {
    /// 协议群 ID。
    pub group_id: String,
    /// 外层已验证的条目 epoch（必须等于本机快照 epoch）。
    pub epoch: u64,
    /// 外层已验证的作者凭据身份。
    pub sender_identity: String,
    /// MLS 提案字节。
    pub payload: Vec<u8>,
}

impl ProposalEntryInput {
    /// 构造输入。
    pub fn new(
        group_id: impl Into<String>,
        epoch: u64,
        sender_identity: impl Into<String>,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            group_id: group_id.into(),
            epoch,
            sender_identity: sender_identity.into(),
            payload,
        }
    }
}

/// 对端独立提案的处理结果（epoch 不变）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalOutcomeView {
    /// 提案种类（P0 控制切片恒为 `remove`）。
    pub kind: String,
    /// 已认证的提案作者身份。
    pub by: String,
    /// 提案所属 epoch。
    pub epoch: u64,
    /// 提案目标成员身份（自移除提案的目标就是作者本机）。
    pub targets: Vec<String>,
    /// 本地 proposal store 里待提交的提案数。
    pub proposals_pending: u32,
    /// 处理后的群投影（epoch 不变）。
    pub view: GroupView,
}

/// GROUPS-013：本机发起"自移除提案"（离开群的第一步）。
///
/// 为什么是提案而不是提交：MLS 1.0 禁止一条 Commit 移除提交者自己，唯一合法的自移除原语是
/// "本机对自己发起 Remove 提案，再由群里另一名成员把提案包进 Commit"。
///
/// 语义：
/// - 群主必须先转让群主，否则 `PERMISSION_DENIED`；
/// - 已解散 `INVALID_STATE_TRANSITION`；本机已发起过自移除提案 `INVALID_STATE_TRANSITION`；
/// - 有未决控制提交时拒绝（语义与 crypto 的 `PENDING_COMMIT_EXISTS` 一致）；
/// - 生成提案**不推进 epoch、不产生 pending 控制提交**，本机仍在群里（能收发），
///   并被标记 `leaving_at_ms`（快照如实报告）；
/// - 返回可转发的提案字节；对端处理后由对端 `group_commit_proposals` 打包成 Commit。
pub fn group_prepare_leave(
    device: &mut MlsDevice,
    store: &mut Store,
    group_id: &str,
) -> GroupResult<LeaveProposalView> {
    let (mut record, _) = load_record(store, group_id)?;
    reject_dissolved(&record)?;
    if record.leaving_at_ms > 0 {
        return Err(GroupError::new(
            "INVALID_STATE_TRANSITION",
            "a self-removal proposal is already pending for this device",
        ));
    }
    if record.left_at_ms > 0 {
        return Err(GroupError::new(
            "INVALID_STATE_TRANSITION",
            "the local device has already left this group",
        ));
    }
    let group_key = device
        .group_key()
        .map_err(|_| GroupError::invalid_target("the local device has no MLS group"))?;
    if group_key != record.mls_group_key {
        return Err(GroupError::invalid_target(
            "the local device is not in this group",
        ));
    }
    let epoch = device
        .epoch()
        .map_err(|_| GroupError::corrupted("MLS group epoch is unavailable"))?;
    if epoch != record.mls_epoch {
        return Err(GroupError::new(
            "STALE_EPOCH",
            "the local MLS epoch does not match the persisted group",
        ));
    }
    let local = record
        .members
        .iter()
        .find(|member| member.is_local)
        .ok_or_else(|| GroupError::invalid_target("the local device is not a confirmed member"))?;
    if local.role == MemberRole::Owner.as_u64() {
        return Err(GroupError::new(
            "PERMISSION_DENIED",
            "the owner must transfer ownership before leaving the group",
        ));
    }
    if device.pending_control().map_err(GroupError::from)?.is_some() {
        return Err(GroupError::new(
            "INVALID_STATE_TRANSITION",
            "a control commit is pending; resolve it before leaving the group",
        ));
    }
    let proposal = device.prepare_self_removal(store).map_err(GroupError::from)?;
    // 生成提案不推进 epoch，也不改策略版本；只记录"本机已发起离开"与待提交提案。
    record.leaving_at_ms = now_ms();
    let digest_hex = hex_lower(&proposal.proposal_digest);
    record.pending_proposal = Some(RecordProposal {
        author: local.credential.clone(),
        target: local.credential.clone(),
        base_epoch: proposal.base_epoch,
        digest: digest_hex.clone(),
        proposal_hex: hex_lower(&proposal.proposal),
        recorded_at_ms: record.leaving_at_ms,
    });
    record.validate()?;
    let saved = persist_snapshot(store, &record, record.mls_epoch)?;
    Ok(LeaveProposalView {
        group_id: record.group_id.clone(),
        base_epoch: proposal.base_epoch,
        epoch: epoch,
        removed: proposal.removed,
        has_welcome: proposal.has_welcome,
        proposal_digest: hex_lower(&proposal.proposal_digest),
        proposal: proposal.proposal,
        leaving_at_ms: record.leaving_at_ms,
        state_version: saved,
    })
}

/// GROUPS-013（收侧）：处理对端的独立提案（CRYPTO-005 `process_proposal_entry`）。
///
/// - 已解散 / 群 ID / epoch 不符 / 作者与 MLS 认证作者不符 → 显式冻结码，**epoch 不变**；
/// - 提案进入本地 proposal store（随群状态一起落库），等待后续 Commit 覆盖；
/// - 重复提案 → `DUPLICATE_PROPOSAL`；非 Remove 提案 → `UNSUPPORTED_ENTRY`。
pub fn group_process_proposal(
    device: &mut MlsDevice,
    store: &mut Store,
    input: &ProposalEntryInput,
) -> GroupResult<ProposalOutcomeView> {
    let (record, _) = load_record(store, &input.group_id)?;
    reject_dissolved(&record)?;
    let group_key = device
        .group_key()
        .map_err(|_| GroupError::invalid_target("the local device has no MLS group"))?;
    if group_key != record.mls_group_key {
        return Err(GroupError::invalid_target(
            "the local device is not in this group",
        ));
    }
    validate_credential(&input.sender_identity)?;
    if input.payload.is_empty() {
        return Err(GroupError::invalid_candidate("proposal payload must not be empty"));
    }
    if input.epoch != record.mls_epoch {
        let code = if input.epoch < record.mls_epoch {
            "STALE_EPOCH"
        } else {
            "MISSING_LOG_ENTRIES"
        };
        return Err(GroupError::new(
            code,
            "proposal epoch does not match the local snapshot",
        ));
    }
    let entry = VerifiedGroupEntry {
        group_id: record.mls_group_key.clone(),
        epoch: input.epoch,
        sender_identity: input.sender_identity.clone(),
        expectation: None,
        payload: input.payload.clone(),
    };
    let outcome = device
        .process_proposal_entry(&entry)
        .map_err(GroupError::from)?;
    // 提案进了 provider storage：导出到正式库才真正持久化（失败则不改变快照）。
    device.export_to_store(store).map_err(|_| {
        GroupError::new(
            "STORAGE_FAILURE",
            "the proposal could not be persisted",
        )
    })?;
    // 把提案的公开绑定（作者/目标/epoch/字节）记进快照，供提交侧做权限校验与节点提交。
    let mut record = record;
    let target = outcome
        .targets
        .first()
        .cloned()
        .ok_or_else(|| GroupError::corrupted("proposal outcome has no target"))?;
    record.pending_proposal = Some(RecordProposal {
        author: outcome.by.clone(),
        target,
        base_epoch: outcome.epoch,
        digest: hex_lower(&proposal_digest(&outcome.by, outcome.epoch, &input.payload)),
        proposal_hex: hex_lower(&input.payload),
        recorded_at_ms: now_ms(),
    });
    record.validate()?;
    let state_version = persist_snapshot(store, &record, record.mls_epoch)?;
    let epoch = device
        .epoch()
        .map_err(|_| GroupError::corrupted("MLS group epoch is unavailable"))?;
    if epoch != record.mls_epoch {
        return Err(GroupError::corrupted(
            "processing a proposal must not change the epoch",
        ));
    }
    Ok(ProposalOutcomeView {
        kind: outcome.kind.code().to_owned(),
        by: outcome.by,
        epoch: outcome.epoch,
        targets: outcome.targets,
        proposals_pending: outcome.proposals_pending,
        view: project(&record, epoch, None, state_version),
    })
}

/// GROUPS-013（提交侧）：把本地待提交提案打包成一条控制提交（CRYPTO-005
/// `commit_pending_proposals`）。
///
/// - 已解散 → `INVALID_STATE_TRANSITION`；没有待提交提案 → `NO_PENDING_PROPOSALS`；
/// - 已有 pending 控制提交 → `PENDING_COMMIT_EXISTS`；
/// - 生成本机 pending 控制提交但**不推进 epoch**；快照如实报告 `pending`（成员不变），
///   接受/合并之后 epoch 才前进、成员才真的减少。
pub fn group_commit_proposals(
    device: &mut MlsDevice,
    store: &mut Store,
    group_id: &str,
) -> GroupResult<GroupView> {
    let (mut record, _) = load_record(store, group_id)?;
    reject_dissolved(&record)?;
    // ---- 提交侧权限（在任何 crypto/写入之前校验） --------------------------
    let proposal = record.pending_proposal.clone().ok_or_else(|| {
        GroupError::new(
            "NO_PENDING_PROPOSALS",
            "there is no pending proposal to commit",
        )
    })?;
    let local = record
        .members
        .iter()
        .find(|member| member.is_local)
        .ok_or_else(|| GroupError::invalid_target("the local device is not a confirmed member"))?;
    if local.role == MemberRole::Member.as_u64() {
        return Err(GroupError::new(
            "PERMISSION_DENIED",
            "only the owner or an admin may commit another member's proposal",
        ));
    }
    if proposal.author == local.credential {
        return Err(GroupError::new(
            "PERMISSION_DENIED",
            "a member cannot commit their own removal proposal",
        ));
    }
    if proposal.target == local.credential {
        return Err(GroupError::new(
            "PERMISSION_DENIED",
            "a member cannot commit a proposal that removes themselves",
        ));
    }
    if !record
        .members
        .iter()
        .any(|member| member.credential == proposal.target)
    {
        return Err(GroupError::invalid_target(
            "the proposal target is not a confirmed member",
        ));
    }
    if proposal.base_epoch != record.mls_epoch {
        return Err(GroupError::new(
            "STALE_EPOCH",
            "the pending proposal belongs to another epoch",
        ));
    }
    let group_key = device
        .group_key()
        .map_err(|_| GroupError::invalid_target("the local device has no MLS group"))?;
    if group_key != record.mls_group_key {
        return Err(GroupError::invalid_target(
            "the local device is not in this group",
        ));
    }
    let epoch = device
        .epoch()
        .map_err(|_| GroupError::corrupted("MLS group epoch is unavailable"))?;
    if epoch != record.mls_epoch {
        return Err(GroupError::new(
            "STALE_EPOCH",
            "the local MLS epoch does not match the persisted group",
        ));
    }
    let pending = device
        .commit_pending_proposals(store)
        .map_err(GroupError::from)?;
    // 提交只产生 pending：epoch 必须没变（crypto 已保证，这里再明确断言一次）。
    let epoch_after = device
        .epoch()
        .map_err(|_| GroupError::corrupted("MLS group epoch is unavailable"))?;
    if epoch_after != record.mls_epoch {
        return Err(GroupError::corrupted(
            "committing proposals must not advance the epoch",
        ));
    }
    // 提案已打包成提交：清空记录（后续由接受/丢弃收口）。
    record.pending_proposal = None;
    record.validate()?;
    let saved = persist_snapshot(store, &record, record.mls_epoch)?;
    Ok(project(&record, epoch_after, Some(&pending), saved))
}
/// 提案提交到节点的回执投影（只含公开绑定信息）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalReceiptView {
    /// 协议群 ID。
    pub group_id: String,
    /// 节点分配的提交标识（PROTOCOL-001 submission 文本）。
    pub submission_id: String,
    /// 节点日志序号（从 1 开始）。
    pub seq: u64,
    /// 该条目之后的节点日志链摘要（小写 hex）。
    pub log_hash: String,
    /// 节点身份。
    pub node_id: String,
    /// 被提交的提案摘要（与本机记录一致）。
    pub proposal_digest: String,
    /// 写入后的快照版本。
    pub state_version: u64,
}

/// GROUPS-013（投递侧）：把本机待提交的独立提案作为**节点条目**提交，返回真实回执。
///
/// - 没有待提交提案（本机既没发起也没收下） → `NO_PENDING_PROPOSALS`；
/// - 已解散 / 已关闭 / 设备不属于该群 / epoch 不符 → 既有冻结码；
/// - 候选以**应用条目**的形式写入节点日志（节点不解析提案内容；提案的 MLS 认证由收侧
///   `group_process_proposal` 完成），回执来自 node-client 的真实签名响应，绝不伪造；
/// - 传输/超时统一 `NODE_UNAVAILABLE`；失败不改变本地快照。
pub fn group_submit_proposal(
    device: &mut MlsDevice,
    store: &mut Store,
    group_id: &str,
    endpoint_url: &str,
    node_id: &str,
    node_public_key_hex: Option<&str>,
) -> GroupResult<ProposalReceiptView> {
    let (record, state_version) = load_record(store, group_id)?;
    reject_dissolved(&record)?;
    let proposal = record.pending_proposal.as_ref().ok_or_else(|| {
        GroupError::new(
            "NO_PENDING_PROPOSALS",
            "there is no pending proposal to submit",
        )
    })?;
    let group_key = device
        .group_key()
        .map_err(|_| GroupError::invalid_target("the local device has no MLS group"))?;
    if group_key != record.mls_group_key {
        return Err(GroupError::invalid_target(
            "the local device is not in this group",
        ));
    }
    let epoch = device
        .epoch()
        .map_err(|_| GroupError::corrupted("MLS group epoch is unavailable"))?;
    if epoch != record.mls_epoch {
        return Err(GroupError::new(
            "STALE_EPOCH",
            "the local MLS epoch does not match the persisted group",
        ));
    }
    let proposal_bytes = decode_hex(&proposal.proposal_hex)
        .ok_or_else(|| GroupError::corrupted("persisted pending proposal is malformed"))?;
    let client = crate::group_node::build_node_client(
        store,
        endpoint_url,
        node_id,
        node_public_key_hex,
    )
    .map_err(GroupError::from)?;
    let receipt = crate::group_node::submit_proposal_entry(
        &client,
        group_id,
        &proposal_bytes,
        proposal.base_epoch,
    )?;
    Ok(ProposalReceiptView {
        group_id: record.group_id.clone(),
        submission_id: receipt.submission_id,
        seq: receipt.seq,
        log_hash: receipt.log_hash,
        node_id: receipt.node_id,
        proposal_digest: proposal.digest.clone(),
        state_version,
    })
}

/// 小写 hex → 字节（非法输入返回 None）。
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().chunks(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        bytes.push((high as u8) << 4 | low as u8);
    }
    Some(bytes)
}
/// 群列表上限（与 storage 的 `MAX_GROUP_STATE_LIST` 保持一致）。
fn storage_max_group_list() -> usize {
    chat_storage::formal::MAX_GROUP_STATE_LIST
}

/// 群列表里的一条摘要（列表页只读快照记录，不解密 MLS 状态）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupSummaryView {
    /// 协议群 ID。
    pub group_id: String,
    /// 群名（改名后立即反映）。
    pub name: String,
    /// 快照里记录的 MLS epoch（**写入时的值**；精确值请用 `group_snapshot`）。
    pub epoch: u64,
    /// 本机在群里的角色（本机已不是成员时是 `Member`）。
    pub role: MemberRole,
    /// 群策略版本。
    pub policy_version: u64,
    /// 已解散。
    pub dissolved: bool,
    /// 本机已退出（本地策略层）。
    pub left: bool,
    /// 本机有待收口的线索：已发起自移除提案（`leaving_at_ms`）或已收下待打包的提案。
    pub pending: bool,
}

/// 群列表结果（`skipped` 是被跳过的损坏条目数，绝不静默丢弃）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupListView {
    /// 摘要（按最近写入时间倒序）。
    pub groups: Vec<GroupSummaryView>,
    /// 读不出来的条目数（快照损坏/不是本模块记录/越界）。
    pub skipped: u32,
}

/// GROUPS-002（列表侧）：列出本机所有群的摘要，不解密 MLS 状态。
///
/// - 只扫 `grpmeta/` 命名空间（本模块独占），最多 `MAX_GROUP_STATE_LIST`(256) 条；
/// - 每条读快照记录并**只做本地投影**（不导入设备、不解密），因此很快；
/// - 损坏/不认识的条目不抛出、也不静默吞掉：计入 `skipped` 并在返回里报告；
/// - 排序由 storage 给出（最近写入时间倒序）。
pub fn group_list(store: &Store) -> GroupResult<GroupListView> {
    let keys = store
        .list_mls_group_state_keys(SNAPSHOT_GROUP_PREFIX, storage_max_group_list())
        .map_err(GroupError::from)?;
    let mut groups = Vec::with_capacity(keys.len());
    let mut skipped = 0_u32;
    for key in keys {
        match summary_from_key(store, &key) {
            Ok(Some(summary)) => groups.push(summary),
            // 读不出来 / 不是本模块记录：计数跳过，绝不静默丢弃。
            Ok(None) | Err(_) => skipped = skipped.saturating_add(1),
        }
    }
    Ok(GroupListView { groups, skipped })
}

fn summary_from_key(
    store: &Store,
    key: &chat_storage::formal::MlsGroupStateKey,
) -> GroupResult<Option<GroupSummaryView>> {
    let Some(stored) = store.load_mls_group_state(&key.group_id).map_err(GroupError::from)? else {
        return Ok(None);
    };
    let Ok(record) = serde_json::from_slice::<GroupRecord>(&stored.state) else {
        return Ok(None);
    };
    // 记录必须自洽且真的属于本命名空间（防止别的模块写进同名空间）。
    record.validate()?;
    if record.group_id.is_empty() {
        return Ok(None);
    }
    let role = record
        .members
        .iter()
        .find(|member| member.is_local)
        .map(|member| role_of(member.role))
        .unwrap_or(MemberRole::Member);
    Ok(Some(GroupSummaryView {
        group_id: record.group_id.clone(),
        name: record.name.clone(),
        epoch: record.mls_epoch,
        role,
        policy_version: record.policy_version,
        dissolved: record.dissolved,
        left: record.left_at_ms > 0,
        pending: record.leaving_at_ms > 0 || record.pending_proposal.is_some(),
    }))
}


