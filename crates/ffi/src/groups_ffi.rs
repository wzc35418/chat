//! 群治理的 UniFFI 对象（GROUPS-001~006 + 控制提交接受/丢弃）。
//!
//! 薄封装：建群、快照、角色、禁言、移除、改名、接受合并、丢弃回滚都在
//! `chat_core::groups`；本层只做「打开一个独占写库 → 调 core 方法 → 投影成 FFI 记录」。
//!
//! 契约要点（与 docs/implementation/bridge.md 的记录一致）：
//!
//! - `GroupFfi::open(database_path, master_key_hex, node)`：打开**独占单写者**的正式加密库
//!   （与 `LocalStoreFfi` / `AttachmentFfi` 不能同时打开同一库）；`node` 本轮只保存，不发请求；
//! - 所有群方法都用**从库恢复**的本机 MLS 设备（`MlsDevice::import_from_store`），不跨调用持有设备；
//! - 位置/角色/状态都是字符串或数字，不暴露密钥；错误一律 `FfiError::Store { code, reason }`，
//!   `code` 是 groups 的冻结码（`PERMISSION_DENIED` / `INVALID_TARGET` / `INVALID_INPUT` /
//!   `INVALID_ID` / `PENDING_MISMATCH` / `STATE_CORRUPTED` / `STALE_EPOCH` / `CORE_CLOSED` …）；
//! - **本地接受证据 ≠ 节点回执**：`group_accept_pending` 只收口本地状态（epoch/成员/pending），
//!   不证明节点已接受；真实部署需先拿节点回执（NODE 侧），见 groups.md。
use std::sync::{Arc, Mutex, MutexGuard};

use chat_core::groups::{
    GroupControlClearedView, GroupMemberKeyPackage, GroupMemberView, GroupPendingView, GroupView,
    MAX_GROUP_MEMBERS, MAX_GROUP_NAME_BYTES,
};
use chat_core::service::{
    ControlAppendView, ControlCandidateView, ControlHeadSnapshotView, ControlReceiptView,
    ConversationCore, group_role_code,
};

use crate::local_store::FfiError;

/// 把内核错误投影成 FFI 错误（冻结码）。
///
/// 控制提交回执（NODE-006）有自己的稳定错误码集合（`NODE_UNAVAILABLE` / `INVALID_RECEIPT_SCOPE` /
/// `INVALID_SIGNATURE` / `PENDING_MISMATCH` …），因此先单独取；其余交给 service 的通用投影
/// （groups/storage/sync 的冻结码都透传）。
macro_rules! failure {
    ($error:expr) => {{
        if let Some(control) = $error.downcast_ref::<chat_core::group_node::ControlReceiptError>() {
            FfiError::Store {
                code: control.code().to_owned(),
                reason: control.detail().to_owned(),
            }
        } else {
            let (code, reason) = chat_core::service::error_projection(&$error);
            FfiError::Store {
                code: code.to_owned(),
                reason,
            }
        }
    }};
}

fn poisoned(what: &str) -> FfiError {
    FfiError::Store {
        code: "INTERNAL_FAILURE".to_owned(),
        reason: format!("{what} is poisoned"),
    }
}

fn invalid_input(reason: &str) -> FfiError {
    FfiError::Store {
        code: "INVALID_INPUT".to_owned(),
        reason: reason.to_owned(),
    }
}

/// 群成员投影（不含密钥材料）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GroupMemberFfi {
    /// 成员凭据身份（创建/移除时用的同一个字符串）。
    pub credential: String,
    /// 角色：`owner` / `admin` / `member`。
    pub role: String,
    /// 是否被禁言。
    pub muted: bool,
    /// 是否本机设备。
    pub is_local: bool,
    /// 是否仍在 pending（追加成员被接受前为 true）。
    pub pending: bool,
}

/// 未决控制提交投影。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GroupPendingFfi {
    /// 操作种类：`add_members` / `remove_members` / `self_update`。
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

/// 最近一次被丢弃控制提交的审计投影。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GroupClearedFfi {
    /// 被丢弃的操作种类。
    pub operation: String,
    /// 提交基于的 epoch。
    pub base_epoch: u64,
    /// 提交本应到达的 epoch。
    pub target_epoch: u64,
    /// 提交摘要（小写 hex）。
    pub commit_digest: String,
    /// 稳定原因码。
    pub reason_code: String,
    /// 调用方给的脱敏说明。
    pub detail: String,
    /// 丢弃时间（Unix 毫秒）。
    pub cleared_at_ms: i64,
}

/// 建群时追加成员的输入（真实 KeyPackage 字节 + 凭据身份）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GroupMemberInputFfi {
    /// 成员凭据身份。
    pub credential: String,
    /// 该成员的序列化 MLS KeyPackage。
    pub key_package: Vec<u8>,
}

/// 群投影（快照与每次治理的结果）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GroupViewFfi {
    /// 协议群 ID（`grp:1:<hex>`）。
    pub group_id: String,
    /// 群名。
    pub name: String,
    /// 当前已提交 MLS epoch。
    pub epoch: u64,
    /// 群策略版本（每次被接受的策略操作 +1）。
    pub policy_version: u64,
    /// 本机在群里的角色。
    pub role: String,
    /// 正式控制头（小写 hex；下一次治理的 CAS 前置值）。
    pub control_head: String,
    /// MLS 群键（小写 hex；诊断用）。
    pub mls_group_key: String,
    /// 快照记录版本（每次写入 +1）。
    pub state_version: u64,
    /// 创建时间（Unix 毫秒）。
    pub created_at_ms: i64,
    /// 正式成员（不含 pending）。
    pub members: Vec<GroupMemberFfi>,
    /// pending 追加成员。
    pub pending_members: Vec<GroupMemberFfi>,
    /// 已移除但尚未合并的成员（接受移除提交后消失，丢弃则恢复）。
    pub pending_removed: Vec<GroupMemberFfi>,
    /// 未决控制提交。
    pub pending: Option<GroupPendingFfi>,
    /// 最近一次被丢弃控制提交的审计。
    pub last_control_cleared: Option<GroupClearedFfi>,
    /// 群是否已解散（解散后所有治理都会显式失败）。
    pub dissolved: bool,
    /// 解散时间（Unix 毫秒；未解散为 `None`）。
    pub dissolved_at_ms: Option<i64>,
    /// 本机退出时间（Unix 毫秒；未退出为 `None`）。
    pub left_at_ms: Option<i64>,
    /// 本机已提交自移除提案的时间（Unix 毫秒；未提交为 `None`）。
    pub leaving_at_ms: Option<i64>,
}

fn member_view(member: &GroupMemberView) -> GroupMemberFfi {
    GroupMemberFfi {
        credential: member.credential.clone(),
        role: group_role_code(member.role).to_owned(),
        muted: member.muted,
        is_local: member.is_local,
        pending: member.pending,
    }
}

fn pending_view(pending: &GroupPendingView) -> GroupPendingFfi {
    GroupPendingFfi {
        operation: pending.operation.clone(),
        base_epoch: pending.base_epoch,
        target_epoch: pending.target_epoch,
        added: pending.added,
        removed: pending.removed,
        commit_digest: pending.commit_digest.clone(),
    }
}

fn cleared_view(cleared: &GroupControlClearedView) -> GroupClearedFfi {
    GroupClearedFfi {
        operation: cleared.operation.clone(),
        base_epoch: cleared.base_epoch,
        target_epoch: cleared.target_epoch,
        commit_digest: cleared.commit_digest.clone(),
        reason_code: cleared.reason_code.clone(),
        detail: cleared.detail.clone(),
        cleared_at_ms: cleared.cleared_at_ms,
    }
}

fn group_view(view: GroupView) -> GroupViewFfi {
    GroupViewFfi {
        group_id: view.group_id,
        name: view.name,
        epoch: view.epoch,
        policy_version: view.policy_version,
        role: group_role_code(view.role).to_owned(),
        control_head: view.control_head,
        mls_group_key: view.mls_group_key,
        state_version: view.state_version,
        created_at_ms: view.created_at_ms,
        members: view.members.iter().map(member_view).collect(),
        pending_members: view.pending_members.iter().map(member_view).collect(),
        pending_removed: view.pending_removed.iter().map(member_view).collect(),
        pending: view.pending.as_ref().map(pending_view),
        last_control_cleared: view.last_control_cleared.as_ref().map(cleared_view),
        dissolved: view.dissolved,
        dissolved_at_ms: view.dissolved_at_ms,
        left_at_ms: view.left_at_ms,
        leaving_at_ms: view.leaving_at_ms,
    }
}


/// 节点签名回执（原样字段；全部是公开材料，不含任何私钥）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ControlAppendFfi {
    /// 群。
    pub group_id: String,
    /// 提交标识。
    pub submission_id: String,
    /// 作者设备。
    pub author_device_id: String,
    /// 节点日志序号（从 1 开始）。
    pub seq: u64,
    /// 候选全文摘要。
    pub candidate_hash: String,
    /// 前序日志摘要。
    pub previous_hash: String,
    /// 本条之后的日志链摘要。
    pub log_hash: String,
    /// 节点接受时间（Unix 秒）。
    pub accepted_at: u64,
    /// 节点身份。
    pub node_id: String,
    /// 节点公钥。
    pub node_public_key: String,
    /// 节点签名（小写 hex）。
    pub signature: String,
}

/// 被提交的控制候选原文（原样字段）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ControlCandidateFfi {
    /// 协议主版本。
    pub protocol_major: u8,
    /// 协议次版本。
    pub protocol_minor: u8,
    /// 提交标识。
    pub submission_id: String,
    /// 群。
    pub group_id: String,
    /// 作者设备。
    pub author_device_id: String,
    /// 候选类别（本路径恒为 `control`）。
    pub kind: String,
    /// 期望控制头（CAS）。
    pub expected_control_head: String,
    /// 期望策略版本。
    pub policy_version: u64,
    /// 期望 MLS epoch。
    pub mls_epoch: u64,
    /// 密文（pending Commit 的小写 hex）。
    pub encrypted_payload: String,
    /// 公开控制操作种类（invite/remove/set_role…）。
    pub operation: Option<String>,
    /// 操作目标摘要。
    pub target_digest: Option<String>,
}

/// 提交时读到的节点 head 快照（原样字段）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ControlHeadFfi {
    /// 群。
    pub group_id: String,
    /// 协调节点身份。
    pub coordinator_id: String,
    /// 日志序号。
    pub seq: u64,
    /// 链尾摘要。
    pub log_hash: String,
    /// 控制头。
    pub control_head: String,
    /// 策略版本。
    pub policy_version: u64,
    /// MLS epoch。
    pub mls_epoch: u64,
    /// 节点身份。
    pub node_id: String,
    /// 节点公钥。
    pub node_public_key: String,
    /// 节点签名（小写 hex）。
    pub signature: String,
}

/// 一次控制提交的完整证据：节点回执 + 候选原文 + 提交时的 head。
///
/// 三者必须一起回传给 `group_accept_with_receipt`：只凭回执无法证明“它绑定的就是这个 pending
/// 提交”，group_node 会用这三段重做绑定校验（含节点验签与日志链摘要）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ControlReceiptFfi {
    /// 节点签名回执。
    pub receipt: ControlAppendFfi,
    /// 被提交的候选原文。
    pub candidate: ControlCandidateFfi,
    /// 提交时读到的节点 head。
    pub head: ControlHeadFfi,
}

fn receipt_ffi(view: &ControlReceiptView) -> ControlReceiptFfi {
    ControlReceiptFfi {
        receipt: ControlAppendFfi {
            group_id: view.receipt.group_id.clone(),
            submission_id: view.receipt.submission_id.clone(),
            author_device_id: view.receipt.author_device_id.clone(),
            seq: view.receipt.seq,
            candidate_hash: view.receipt.candidate_hash.clone(),
            previous_hash: view.receipt.previous_hash.clone(),
            log_hash: view.receipt.log_hash.clone(),
            accepted_at: view.receipt.accepted_at,
            node_id: view.receipt.node_id.clone(),
            node_public_key: view.receipt.node_public_key.clone(),
            signature: view.receipt.signature.clone(),
        },
        candidate: ControlCandidateFfi {
            protocol_major: view.candidate.protocol_major,
            protocol_minor: view.candidate.protocol_minor,
            submission_id: view.candidate.submission_id.clone(),
            group_id: view.candidate.group_id.clone(),
            author_device_id: view.candidate.author_device_id.clone(),
            kind: view.candidate.kind.clone(),
            expected_control_head: view.candidate.expected_control_head.clone(),
            policy_version: view.candidate.policy_version,
            mls_epoch: view.candidate.mls_epoch,
            encrypted_payload: view.candidate.encrypted_payload.clone(),
            operation: view.candidate.operation.clone(),
            target_digest: view.candidate.target_digest.clone(),
        },
        head: ControlHeadFfi {
            group_id: view.head.group_id.clone(),
            coordinator_id: view.head.coordinator_id.clone(),
            seq: view.head.seq,
            log_hash: view.head.log_hash.clone(),
            control_head: view.head.control_head.clone(),
            policy_version: view.head.policy_version,
            mls_epoch: view.head.mls_epoch,
            node_id: view.head.node_id.clone(),
            node_public_key: view.head.node_public_key.clone(),
            signature: view.head.signature.clone(),
        },
    }
}

fn receipt_back(record: &ControlReceiptFfi) -> ControlReceiptView {
    ControlReceiptView {
        receipt: ControlAppendView {
            group_id: record.receipt.group_id.clone(),
            submission_id: record.receipt.submission_id.clone(),
            author_device_id: record.receipt.author_device_id.clone(),
            seq: record.receipt.seq,
            candidate_hash: record.receipt.candidate_hash.clone(),
            previous_hash: record.receipt.previous_hash.clone(),
            log_hash: record.receipt.log_hash.clone(),
            accepted_at: record.receipt.accepted_at,
            node_id: record.receipt.node_id.clone(),
            node_public_key: record.receipt.node_public_key.clone(),
            signature: record.receipt.signature.clone(),
        },
        candidate: ControlCandidateView {
            protocol_major: record.candidate.protocol_major,
            protocol_minor: record.candidate.protocol_minor,
            submission_id: record.candidate.submission_id.clone(),
            group_id: record.candidate.group_id.clone(),
            author_device_id: record.candidate.author_device_id.clone(),
            kind: record.candidate.kind.clone(),
            expected_control_head: record.candidate.expected_control_head.clone(),
            policy_version: record.candidate.policy_version,
            mls_epoch: record.candidate.mls_epoch,
            encrypted_payload: record.candidate.encrypted_payload.clone(),
            operation: record.candidate.operation.clone(),
            target_digest: record.candidate.target_digest.clone(),
        },
        head: ControlHeadSnapshotView {
            group_id: record.head.group_id.clone(),
            coordinator_id: record.head.coordinator_id.clone(),
            seq: record.head.seq,
            log_hash: record.head.log_hash.clone(),
            control_head: record.head.control_head.clone(),
            policy_version: record.head.policy_version,
            mls_epoch: record.head.mls_epoch,
            node_id: record.head.node_id.clone(),
            node_public_key: record.head.node_public_key.clone(),
            signature: record.head.signature.clone(),
        },
    }
}


/// 自移除提案投影（纯数据：可转发字节 + 公开绑定信息，不含密钥）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LeaveProposalFfi {
    /// 协议群 ID。
    pub group_id: String,
    /// 生成提案时的本地 epoch。
    pub base_epoch: u64,
    /// 当前真实 epoch（生成提案不推进 epoch）。
    pub epoch: u64,
    /// 预期移除的成员数（恒为 1：只有本机）。
    pub removed: u32,
    /// 是否携带 Welcome（恒为 false）。
    pub has_welcome: bool,
    /// 序列化的 MLS 提案字节（交给另一名成员提交）。
    pub proposal: Vec<u8>,
    /// 提案摘要（小写 hex）。
    pub proposal_digest: String,
    /// 本机标记离开的时间（Unix 毫秒）。
    pub leaving_at_ms: i64,
    /// 写入后的快照版本。
    pub state_version: u64,
}

/// 对端独立提案的处理结果（纯数据）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ProposalViewFfi {
    /// 提案种类（P0 控制切片恒为 `remove`）。
    pub kind: String,
    /// 已认证的提案作者身份。
    pub by: String,
    /// 提案所属 epoch。
    pub epoch: u64,
    /// 提案目标成员身份。
    pub targets: Vec<String>,
    /// 本地待提交的提案数。
    pub proposals_pending: u32,
}

fn leave_proposal_ffi(view: &chat_core::groups::LeaveProposalView) -> LeaveProposalFfi {
    LeaveProposalFfi {
        group_id: view.group_id.clone(),
        base_epoch: view.base_epoch,
        epoch: view.epoch,
        removed: view.removed,
        has_welcome: view.has_welcome,
        proposal: view.proposal.clone(),
        proposal_digest: view.proposal_digest.clone(),
        leaving_at_ms: view.leaving_at_ms,
        state_version: view.state_version,
    }
}

fn proposal_view_ffi(view: &chat_core::groups::ProposalOutcomeView) -> ProposalViewFfi {
    ProposalViewFfi {
        kind: view.kind.clone(),
        by: view.by.clone(),
        epoch: view.epoch,
        targets: view.targets.clone(),
        proposals_pending: view.proposals_pending,
    }
}

/// 群治理 FFI 门面。
#[derive(uniffi::Object)]
pub struct GroupFfi {
    core: Mutex<ConversationCore>,
    node: Option<String>,
}

impl GroupFfi {
    fn lock(&self) -> Result<MutexGuard<'_, ConversationCore>, FfiError> {
        self.core.lock().map_err(|_| poisoned("group core"))
    }
}

#[uniffi::export]
impl GroupFfi {
    /// 打开群治理对象（独占单写者的正式加密库）。
    ///
    /// 数据目录必须已存在；主密钥是 64 位十六进制；`node` 本轮只保存（不联网），
    /// 供上层记录“该群绑定哪个协调节点”。与 `LocalStoreFfi` / `AttachmentFfi` 互斥。
    #[uniffi::constructor]
    pub fn open(
        database_path: String,
        master_key_hex: String,
        node: Option<String>,
    ) -> Result<Arc<Self>, FfiError> {
        let core = ConversationCore::open(&database_path, &master_key_hex)
            .map_err(|error| failure!(error))?;
        Ok(Arc::new(Self {
            core: Mutex::new(core),
            node,
        }))
    }

    /// 构造时保存的节点标识（本轮不联网，仅诊断/展示）。
    pub fn node(&self) -> Option<String> {
        self.node.clone()
    }

    /// 是否仍处于打开状态。
    pub fn is_open(&self) -> bool {
        match self.core.lock() {
            Ok(core) => core.is_open(),
            Err(_) => false,
        }
    }

    /// 关闭并释放独占写锁；之后所有调用返回 `CORE_CLOSED`。
    pub fn shutdown(&self) {
        if let Ok(core) = self.core.lock() {
            core.close();
        }
    }

    /// GROUPS-001：建群（本机 + 引导成员），追加成员走 pending Add 提交。
    ///
    /// `local_credential` 传空串时使用**持久身份**的设备 ID（未入库身份 → `INVALID_STATE_TRANSITION`）。
    /// `bootstrap_credential` 的 MLS 设备由本进程临时创建（crypto 目前只有双人引导路径，
    /// 真实邀请加入属 GROUPS-004，未实现）。
    pub fn group_create(
        &self,
        name: String,
        local_credential: String,
        bootstrap_credential: String,
        members: Vec<GroupMemberInputFfi>,
    ) -> Result<GroupViewFfi, FfiError> {
        if name.trim().is_empty() || name.len() > MAX_GROUP_NAME_BYTES {
            return Err(invalid_input("group name must be 1..=64 bytes"));
        }
        if bootstrap_credential.trim().is_empty() {
            return Err(invalid_input("bootstrap credential must not be empty"));
        }
        if members.len() + 2 > MAX_GROUP_MEMBERS {
            return Err(FfiError::Store {
                code: "GROUP_LIMIT_EXCEEDED".to_owned(),
                reason: "group member count exceeds the configured limit".to_owned(),
            });
        }
        let members = members
            .into_iter()
            .map(|member| GroupMemberKeyPackage {
                credential: member.credential,
                key_package: member.key_package,
            })
            .collect();
        let mut core = self.lock()?;
        let view = core
            .group_create(&name, &local_credential, &bootstrap_credential, members)
            .map_err(|error| failure!(error))?;
        Ok(group_view(view))
    }

    /// GROUPS-002：读取正式群快照。
    pub fn group_snapshot(&self, group_id: String) -> Result<GroupViewFfi, FfiError> {
        let core = self.lock()?;
        let view = core
            .group_snapshot(&group_id)
            .map_err(|error| failure!(error))?;
        Ok(group_view(view))
    }

    /// GROUPS-003：改角色（`owner`/`admin`/`member`；群主才能改，不能改群主）。
    pub fn group_set_role(
        &self,
        group_id: String,
        actor_credential: String,
        target_credential: String,
        role: String,
    ) -> Result<GroupViewFfi, FfiError> {
        let mut core = self.lock()?;
        let view = core
            .group_set_role(&group_id, &actor_credential, &target_credential, &role)
            .map_err(|error| failure!(error))?;
        Ok(group_view(view))
    }

    /// GROUPS-004：设置禁言（纯策略层）。
    pub fn group_set_muted(
        &self,
        group_id: String,
        actor_credential: String,
        target_credential: String,
        muted: bool,
    ) -> Result<GroupViewFfi, FfiError> {
        let mut core = self.lock()?;
        let view = core
            .group_set_muted(&group_id, &actor_credential, &target_credential, muted)
            .map_err(|error| failure!(error))?;
        Ok(group_view(view))
    }

    /// GROUPS-005：移除确认成员（真实 MLS 移除提交 + pending）。
    pub fn group_remove_member(
        &self,
        group_id: String,
        actor_credential: String,
        target_credential: String,
    ) -> Result<GroupViewFfi, FfiError> {
        let mut core = self.lock()?;
        let view = core
            .group_remove_member(&group_id, &actor_credential, &target_credential)
            .map_err(|error| failure!(error))?;
        Ok(group_view(view))
    }

    /// GROUPS-006：改名（同名幂等无操作）。
    pub fn group_rename(
        &self,
        group_id: String,
        actor_credential: String,
        new_name: String,
    ) -> Result<GroupViewFfi, FfiError> {
        let mut core = self.lock()?;
        let view = core
            .group_rename(&group_id, &actor_credential, &new_name)
            .map_err(|error| failure!(error))?;
        Ok(group_view(view))
    }

    /// 接受并合并当前控制提交（CRYPTO-007）。**本地接受证据 ≠ 节点回执**。
    pub fn group_accept_pending(&self, group_id: String) -> Result<GroupViewFfi, FfiError> {
        let mut core = self.lock()?;
        let view = core
            .group_accept_pending(&group_id)
            .map_err(|error| failure!(error))?;
        Ok(group_view(view))
    }

    /// 丢弃当前控制提交并回滚（CRYPTO-008；`policy_version` 不回退）。
    pub fn group_discard_pending(
        &self,
        group_id: String,
        reason: String,
    ) -> Result<GroupViewFfi, FfiError> {
        let mut core = self.lock()?;
        let view = core
            .group_discard_pending(&group_id, &reason)
            .map_err(|error| failure!(error))?;
        Ok(group_view(view))
    }

    /// NODE-006：把 pending 控制提交送到节点，返回真实回执（seq 从 1 开始）。
    ///
    /// 设备签名钥复用库里**持久身份**的派生（未入库身份 → `INVALID_STATE_TRANSITION`）；
    /// `node_public_key_hex` 可选（给了就固定节点公钥）。节点不可达统一 `NODE_UNAVAILABLE`。
    pub fn group_submit_control(
        &self,
        group_id: String,
        endpoint_url: String,
        node_id: String,
        node_public_key_hex: Option<String>,
    ) -> Result<ControlReceiptFfi, FfiError> {
        let mut core = self.lock()?;
        let receipt = core
            .group_submit_control(
                &group_id,
                &endpoint_url,
                &node_id,
                node_public_key_hex.as_deref(),
            )
            .map_err(|error| failure!(error))?;
        Ok(receipt_ffi(&receipt))
    }

    /// NODE-006 接受侧：用真实回执接受并合并 pending 控制提交（epoch 前进、pending 清空）。
    ///
    /// 回执与候选/head 的绑定会被重做校验（节点验签 + 日志链摘要 + 密文逐字节比对）；
    /// 伪造或错绑定回执显式失败且 epoch/成员不变。
    pub fn group_accept_with_receipt(
        &self,
        group_id: String,
        receipt: ControlReceiptFfi,
    ) -> Result<GroupViewFfi, FfiError> {
        let view = receipt_back(&receipt);
        let mut core = self.lock()?;
        let accepted = core
            .group_accept_with_receipt(&group_id, &view)
            .map_err(|error| failure!(error))?;
        Ok(group_view(accepted))
    }

    /// GROUPS-013：本机发起自移除提案（离开群的第一步）。
    ///
    /// 群主必须先转让（`PERMISSION_DENIED`）；重复发起 `INVALID_STATE_TRANSITION`；
    /// 不推进 epoch、不产生 pending 控制提交，返回可转发的提案字节。
    pub fn group_prepare_leave(&self, group_id: String) -> Result<LeaveProposalFfi, FfiError> {
        let mut core = self.lock()?;
        let view = core
            .group_prepare_leave(&group_id)
            .map_err(|error| failure!(error))?;
        Ok(leave_proposal_ffi(&view))
    }

    /// GROUPS-013（收侧）：处理对端独立提案（epoch 不变，提案入本地 store）。
    pub fn group_process_proposal(
        &self,
        group_id: String,
        epoch: u64,
        sender_identity: String,
        payload: Vec<u8>,
    ) -> Result<ProposalViewFfi, FfiError> {
        let input = chat_core::groups::ProposalEntryInput::new(
            group_id,
            epoch,
            sender_identity,
            payload,
        );
        let mut core = self.lock()?;
        let view = core
            .group_process_proposal(&input)
            .map_err(|error| failure!(error))?;
        Ok(proposal_view_ffi(&view))
    }

    /// GROUPS-013（提交侧）：把本地待提交提案打包成 pending 控制提交（epoch 不推进）。
    pub fn group_commit_proposals(&self, group_id: String) -> Result<GroupViewFfi, FfiError> {
        let mut core = self.lock()?;
        let view = core
            .group_commit_proposals(&group_id)
            .map_err(|error| failure!(error))?;
        Ok(group_view(view))
    }
}
