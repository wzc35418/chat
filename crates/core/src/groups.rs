//! GROUPS-001/002：建群与正式群快照（真实 OpenMLS + 正式加密库，不做任何模拟）。
//!
//! 本切片只做两件事，且都基于既有原语：
//!
//! - **建群**：用 P0 引导（`MlsDevice::create_and_invite`）建立含创建者与首个成员的
//!   真实 MLS 群，再用 CRYPTO-005 `prepare_control(AddMembers)` 为其余成员准备真实
//!   Commit/Welcome（**pending**，等待协调者接受后才推进 epoch）；
//! - **快照**：从正式库读出持久快照记录，并用 `MlsDevice::import_from_store` 恢复
//!   真实 MLS 状态，取 **epoch 与 pending** 的权威值；未知群显式失败，不臆造字段。
//!
//! 持久化边界（本切片的临时载体，后续由 storage 的正式 groups 表替换）：
//! 正式库目前只有 MLS 状态族（`mls_group_state` / `mls_record`），没有群元数据表。
//! 因此群快照（名称/成员/角色/策略版本/创建时间）以**独立 group_id 命名空间**
//! （`grpmeta/<协议群 ID 的 hex>`）写入 `mls_group_state` 的 `state` 槽位
//! （由 storage 密封、带版本与校验）；MLS 自己的群状态仍用 MLS group id 存，两者互不覆盖。
//! `epoch` 取群当前 epoch（storage 对同一 group_id 做单调校验），并在读取时以
//! MLS 设备恢复出的 epoch 为准。
//!
//! 已知限制：
//! - crypto 目前只有“双人引导”（`create_and_invite` 需要同进程的对端设备句柄）能建立群，
//!   没有“自建空群”的 API（CRYPTO-001 `create_crypto_group` 未实现）。因此 `create_group`
//!   需要传入**首个成员的设备句柄**用于引导，其余成员用 KeyPackage 走真实 Add 提交；
//! - 没有群列表 API：快照查询必须给出 `group_id`（协议群 ID 由 MLS 群键确定性派生，
//!   与发送链路 `send_runtime::protocol_group_id_for` 同一派生）；
//! - 角色/成员是**调用方声明的凭据身份**（MLS 侧的真实校验由 `prepare_control` 完成：
//!   自己加自己/已是成员/重复 KeyPackage 都会以冻结码拒绝）。

use chat_crypto::control::{ControlError, ControlOperation, PendingControl};
use chat_crypto::engine::MlsDevice;
use chat_domain::MemberRole;
use chat_protocol::GroupId;
use chat_storage::formal::{MlsGroupStateInput, StorageError, Store};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
const SNAPSHOT_VERSION: u8 = 1;
/// 快照记录的 storage group_id 命名空间前缀。
const SNAPSHOT_GROUP_PREFIX: &str = "grpmeta/";
/// 快照记录体上限（字节，留足成员列表空间）。
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024;

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

/// 建群结果类型。
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
    /// 是否本机设备。
    pub is_local: bool,
    /// 是否仍处于 pending（追加成员在被协调者接受前不进入正式成员列表的语义位）。
    pub pending: bool,
}

/// pending 控制提交投影（只含公开绑定信息）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupPendingView {
    /// 操作种类（currently `add_members`）。
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

/// 建群结果 / 群快照投影（GROUPS-001/002 共用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupView {
    /// 协议群 ID（PROTOCOL-001 `grp:1:<hex>`，由 MLS 群键确定性派生）。
    pub group_id: String,
    /// 群名。
    pub name: String,
    /// 当前**已提交** MLS epoch（pending 接受前不推进）。
    pub epoch: u64,
    /// 群策略版本。
    pub policy_version: u32,
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
    /// 未决控制提交（没有则为 `None`）。
    pub pending: Option<GroupPendingView>,
}

/// 持久快照记录体（storage 密封；本模块独占 `grpmeta/` 命名空间）。
#[derive(Clone, Debug, Serialize, Deserialize)]
struct GroupSnapshotRecord {
    version: u8,
    group_id: String,
    mls_group_key: String,
    name: String,
    policy_version: u32,
    created_at_ms: i64,
    local_credential: String,
    members: Vec<RecordMember>,
    pending_members: Vec<RecordMember>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RecordMember {
    credential: String,
    role: u64,
    is_local: bool,
}

impl GroupSnapshotRecord {
    fn validate(&self) -> GroupResult<()> {
        if self.version != SNAPSHOT_VERSION {
            return Err(GroupError::corrupted("group snapshot has an unsupported version"));
        }
        validate_name(&self.name)?;
        if self.policy_version != GROUP_POLICY_VERSION {
            return Err(GroupError::new(
                "INVALID_POLICY_TRANSITION",
                "persisted group policy version is not supported",
            ));
        }
        if self.mls_group_key.is_empty() || self.mls_group_key.len() > 128 {
            return Err(GroupError::corrupted("persisted group has an invalid MLS group key"));
        }
        if self.members.is_empty() || self.members.len() + self.pending_members.len() > MAX_GROUP_MEMBERS
        {
            return Err(GroupError::corrupted(
                "persisted group member list is empty or oversized",
            ));
        }
        let mut seen: Vec<&str> = Vec::new();
        for member in self.members.iter().chain(self.pending_members.iter()) {
            validate_credential(&member.credential)?;
            if seen.contains(&member.credential.as_str()) {
                return Err(GroupError::corrupted(
                    "persisted group repeats a member credential",
                ));
            }
            seen.push(&member.credential);
            if MemberRole::from_u64(member.role).is_none() {
                return Err(GroupError::corrupted(
                    "persisted group member has an unknown role",
                ));
            }
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
        return Err(GroupError::invalid_input(
            "group name must be 1..=64 bytes",
        ));
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

/// 快照记录的 storage group_id（独立命名空间 + 协议群 ID 的 hex）。
fn snapshot_group_id(group_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(group_id.as_bytes());
    let digest: [u8; 32] = hasher.finalize().into();
    format!("{SNAPSHOT_GROUP_PREFIX}{}", hex_lower(&digest))
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

/// pdf-合法群 ID 校验（PROTOCOL-001 `grp:1:<64 hex>`）。
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

/// 把持久快照与真实 MLS 状态合成对外投影。
fn project(
    record: &GroupSnapshotRecord,
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
            is_local: member.is_local,
            pending: true,
        })
        .collect();
    GroupView {
        group_id: record.group_id.clone(),
        name: record.name.clone(),
        epoch,
        policy_version: record.policy_version,
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
        pending: pending.map(pending_view),
    }
}

/// GROUPS-001：建立一个小群（创建者 + 引导成员），并为其余成员准备真实 Add 提交。
///
/// 语义：
/// 1. 名称/凭据/成员上限/自我加法/重复成员在**任何 MLS 或存储写入之前**校验；
/// 2. 用 P0 引导（`create_and_invite`）建立含创建者与引导成员的真实 MLS 群；
/// 3. 有其余成员时用 CRYPTO-005 `prepare_control(AddMembers)` 生成真实 Commit/Welcome，
///    **pending 落库**（不推进 epoch）；没有其余成员时只导出群状态；
/// 4. 快照（名称/成员/角色/策略版本/创建时间）写入正式库的独立命名空间记录；
/// 5. 返回真实 epoch 与 pending 的群投影。
///
/// 失败：参数校验失败不写任何状态；MLS 级拒绝（crypto 的冻结码）在引导之后发生，
/// 此时本地设备保留“只含引导成员的有效群”，但**没有 pending、没有快照记录**。
pub fn create_group(
    device: &mut MlsDevice,
    bootstrap_peer: &GroupBootstrapPeer<'_>,
    store: &mut Store,
    request: &CreateGroupRequest,
) -> GroupResult<GroupView> {
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
    let peer_key_package = bootstrap_peer
        .device
        .key_package()
        .map_err(|_| GroupError::invalid_candidate("bootstrap peer key package could not be built"))?;
    if seen_packages.contains(&peer_key_package.as_slice()) {
        return Err(GroupError::invalid_input(
            "the bootstrap peer is also listed as an additional member",
        ));
    }
    device
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
            GroupError::new("STORAGE_FAILURE", "the MLS group state could not be persisted")
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
    let record = GroupSnapshotRecord {
        version: SNAPSHOT_VERSION,
        group_id: group_id.clone(),
        mls_group_key: mls_group_key.clone(),
        name: request.name.clone(),
        policy_version: request.policy_version,
        created_at_ms,
        local_credential: request.local_credential.clone(),
        members: vec![
            RecordMember {
                credential: request.local_credential.clone(),
                role: MemberRole::Owner.as_u64(),
                is_local: true,
            },
            RecordMember {
                credential: bootstrap_peer.credential.clone(),
                role: MemberRole::Member.as_u64(),
                is_local: false,
            },
        ],
        pending_members: request
            .members
            .iter()
            .map(|member| RecordMember {
                credential: member.credential.clone(),
                role: MemberRole::Member.as_u64(),
                is_local: false,
            })
            .collect(),
    };
    record.validate()?;
    let saved = persist_snapshot(store, &record, epoch)?;

    Ok(project(&record, epoch, pending.as_ref(), saved))
}

/// GROUPS-002：读取正式群快照（库离线也返回最后正式状态，不联网）。
///
/// - 快照记录不存在 → `INVALID_TARGET`（未知群，绝不臆造）；
/// - 记录存在但 MLS 状态无法恢复 → `STATE_CORRUPTED`；
/// - epoch 与 pending 以**真实 MLS 状态**为准（记录里的 epoch 只是写入时的值）。
pub fn group_snapshot(store: &Store, group_id: &str) -> GroupResult<GroupView> {
    parse_group_id(group_id)?;
    let storage_id = snapshot_group_id(group_id);
    let stored = store
        .load_mls_group_state(&storage_id)
        .map_err(GroupError::from)?
        .ok_or_else(GroupError::not_found)?;
    let record: GroupSnapshotRecord = serde_json::from_slice(&stored.state)
        .map_err(|_| GroupError::corrupted("persisted group snapshot is not readable"))?;
    record.validate()?;
    if record.group_id != group_id {
        return Err(GroupError::corrupted(
            "persisted group snapshot belongs to another group id",
        ));
    }
    let device = MlsDevice::import_from_store(store, &record.mls_group_key).map_err(|_| {
        GroupError::corrupted("the persisted MLS group state could not be restored")
    })?;
    let epoch = device
        .epoch()
        .map_err(|_| GroupError::corrupted("restored MLS group epoch is unavailable"))?;
    let pending = device
        .pending_control()
        .map_err(GroupError::from)?
        .map(|pending| (pending.operation.code().to_owned(), pending));
    if let Some((operation, pending)) = &pending {
        if operation != "add_members" && record.pending_members.is_empty() {
            return Err(GroupError::corrupted(
                "persisted MLS pending commit does not match the group snapshot",
            ));
        }
        if pending.base_epoch != epoch {
            return Err(GroupError::corrupted(
                "persisted MLS pending commit is bound to another epoch",
            ));
        }
    }
    Ok(project(
        &record,
        epoch,
        pending.as_ref().map(|(_, pending)| pending),
        stored.state_version,
    ))
}

/// 写入快照记录（同一命名空间内单调 epoch；返回 storage 的 state_version）。
fn persist_snapshot(
    store: &mut Store,
    record: &GroupSnapshotRecord,
    epoch: u64,
) -> GroupResult<u64> {
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
