//! 单句柄共享核心门面（TASK-128）。
//!
//! 背景：单写者约束让"身份库/消息库/群/同步"在 FFI 层分裂成多个对象，打包冒烟只能靠
//! "开-关-再开"绕。`ChatCoreFfi` 只打开**一个** `ConversationCore`（内部就是一个
//! `Mutex<Option<Store>>` 的唯一句柄），群/消息/同步方法全部转发到既有 core 函数 ——
//! **不新写任何业务逻辑**。
//!
//! 语义：
//!
//! - `open` 同一个库的**第二个实例** → 存储层 `STORAGE_LOCKED` 原样透出（单写者不变）；
//! - `close` 之后所有调用 → `CORE_CLOSED`；
//! - `sync_once` 需要裸 `Store` 句柄，因此临时释放核心、打开唯一句柄跑单轮拉取、
//!   再无条件把核心装回（失败也装回，绝不把对象留在"无核心"状态）。

use std::sync::{Arc, Mutex, MutexGuard};

use chat_core::groups::GroupInviteBlob;
use chat_core::service::{ConversationCore, DraftView};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, StoreOptions, open_store};

use crate::local_store::FfiError;

/// 群投影（列表/快照）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SharedGroupFfi {
    /// 协议群 ID。
    pub group_id: String,
    /// 群名。
    pub name: String,
    /// 已提交 MLS epoch。
    pub epoch: u64,
    /// MLS 群键（小写 hex；`configure_send_group` 要的就是它）。
    pub mls_group_key: String,
    /// 成员凭据（确认成员 ∪ pending 成员）。
    pub members: Vec<String>,
    /// 建群/入群时自动播种的已撤销设备条数。
    pub seeded_revoked_devices: u32,
}

/// 一条可投递邀请（元数据 + Welcome 原文，纯数据无密钥）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SharedInviteFfi {
    /// 协议群 ID。
    pub group_id: String,
    /// MLS 群 ID（小写 hex）。
    pub mls_group_id: String,
    /// 群名。
    pub name: String,
    /// 邀请者凭据。
    pub inviter_credential: String,
    /// 群主凭据。
    pub owner_credential: String,
    /// 被邀请者凭据。
    pub invitee_credential: String,
    /// 预期成员凭据集合。
    pub members: Vec<String>,
    /// 预期 epoch。
    pub epoch: u64,
    /// MLS Welcome 原文。
    pub welcome_bytes: Vec<u8>,
}

/// 建群结果：快照 + 邀请。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SharedCreatedGroupFfi {
    /// 群快照。
    pub group: SharedGroupFfi,
    /// 与快照一致的邀请（含 Welcome 原文）。
    pub invite: SharedInviteFfi,
}

/// 节点提交回执。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SharedReceiptFfi {
    /// 提交标识。
    pub submission_id: String,
    /// 日志序号（从 1 开始）。
    pub seq: u64,
    /// 该条目之后的日志链摘要。
    pub log_hash: String,
    /// 节点身份。
    pub node_id: String,
}

/// 群在节点登记的结果。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SharedRegistrationFfi {
    /// 协议群 ID。
    pub group_id: String,
    /// 登记条的节点日志序号（节点真实返回）。
    pub registered_seq: u64,
    /// 节点身份。
    pub node_id: String,
}

/// 草稿投影。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SharedDraftFfi {
    /// 草稿 ID。
    pub draft_id: String,
    /// 会话 ID。
    pub conversation_id: String,
    /// 幂等键。
    pub operation_id: String,
    /// 状态。
    pub state: String,
}

/// 草稿当前状态（不含正文）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SharedDraftStatusFfi {
    /// 草稿 ID。
    pub draft_id: String,
    /// 有效状态（core 的 `state` 字段：queued / prepared / submitted …）。
    pub status: String,
    /// 最新 SubmissionId（尚无尝试时为 None）。
    pub submission_id: Option<String>,
}

/// 发送单步结果。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SharedSendStepFfi {
    /// 草稿 ID。
    pub draft_id: String,
    /// 有效状态。
    pub status: String,
    /// 最新 SubmissionId。
    pub submission_id: Option<String>,
    /// 最新尝试序号。
    pub attempt_index: Option<u32>,
    /// 静态说明（不含正文/密钥）。
    pub detail: String,
    /// 是否已确定（false = 仍需对账）。
    pub settled: bool,
}

/// 一条消息。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SharedMessageFfi {
    /// 消息 ID（本机待发为草稿 ID）。
    pub message_id: String,
    /// 会话 ID。
    pub conversation_id: String,
    /// 作者账户 ID。
    pub author: String,
    /// 明文正文。
    pub body: String,
    /// 是否本机作者。
    pub mine: bool,
    /// 展示时间（Unix 毫秒）。
    pub created_at_ms: u64,
}

/// 一页消息。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SharedMessagePageFfi {
    /// 本页条目。
    pub items: Vec<SharedMessageFfi>,
    /// 下一页游标。
    pub next_cursor: Option<String>,
}

/// 单轮同步结果。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SharedSyncOnceFfi {
    /// 本次从节点取回并处理的条目数（applied + stale + unsupported）。
    pub fetched: u64,
    /// 真正应用的条目数。
    pub applied: u64,
    /// 因 epoch 过期被跳过的条目数。
    pub stale: u64,
    /// 无法应用的条目数。
    pub unsupported: u64,
    /// 是否需要先追赶（本页不推进游标）。
    pub needs_catchup: bool,
    /// 作者凭据不在目录里的条目数。
    pub unknown_authors: u64,
}

/// 共享核心门面：**一个** `Store` 句柄 + 既有 core 业务函数。
#[derive(uniffi::Object)]
pub struct ChatCoreFfi {
    database_path: String,
    master_key_hex: String,
    core: Mutex<Option<ConversationCore>>,
}

/// 内核错误 → FFI 错误（稳定码 + 脱敏消息；不需要在这里点名 `anyhow` 类型）。
macro_rules! failure {
    ($error:expr) => {{
        let (code, reason) = chat_core::service::error_projection(&$error);
        FfiError::Store {
            code: code.to_owned(),
            reason,
        }
    }};
}

fn storage_failure(error: chat_storage::formal::StorageError) -> FfiError {
    FfiError::Store {
        code: error.code().to_owned(),
        reason: error.to_string(),
    }
}

fn core_closed() -> FfiError {
    FfiError::Store {
        code: "CORE_CLOSED".to_owned(),
        reason: "the shared core has been closed".to_owned(),
    }
}

fn invalid_input(reason: &str) -> FfiError {
    FfiError::Store {
        code: "INVALID_INPUT".to_owned(),
        reason: reason.to_owned(),
    }
}

fn decode_master_key(text: &str) -> Result<[u8; 32], FfiError> {
    if text.len() != 64 {
        return Err(invalid_input("master key must be exactly 64 hex characters"));
    }
    let bytes = text.as_bytes();
    let mut key = [0_u8; 32];
    for (index, slot) in key.iter_mut().enumerate() {
        let high = hex_nibble(bytes[index * 2])?;
        let low = hex_nibble(bytes[index * 2 + 1])?;
        *slot = (high << 4) | low;
    }
    Ok(key)
}

fn hex_nibble(byte: u8) -> Result<u8, FfiError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(invalid_input("master key must be hexadecimal")),
    }
}

fn group_view(view: &chat_core::groups::GroupView) -> SharedGroupFfi {
    let mut members: Vec<String> = Vec::new();
    for member in view.members.iter().chain(view.pending_members.iter()) {
        if !members.contains(&member.credential) {
            members.push(member.credential.clone());
        }
    }
    SharedGroupFfi {
        group_id: view.group_id.clone(),
        name: view.name.clone(),
        epoch: view.epoch,
        mls_group_key: view.mls_group_key.clone(),
        members,
        seeded_revoked_devices: view.seeded_revoked_devices,
    }
}

fn invite_view(
    view: &chat_core::groups::GroupView,
    local_credential: &str,
    invitee_credential: &str,
    welcome_bytes: Vec<u8>,
) -> SharedInviteFfi {
    let mut members: Vec<String> = Vec::new();
    for member in view.members.iter().chain(view.pending_members.iter()) {
        if !members.contains(&member.credential) {
            members.push(member.credential.clone());
        }
    }
    let owner_credential = view
        .members
        .iter()
        .find(|member| chat_core::service::group_role_code(member.role) == "owner")
        .map(|member| member.credential.clone())
        .unwrap_or_else(|| local_credential.to_owned());
    let inviter_credential = view
        .members
        .iter()
        .find(|member| member.is_local)
        .map(|member| member.credential.clone())
        .unwrap_or_else(|| local_credential.to_owned());
    SharedInviteFfi {
        group_id: view.group_id.clone(),
        mls_group_id: view.mls_group_key.clone(),
        name: view.name.clone(),
        inviter_credential,
        owner_credential,
        invitee_credential: invitee_credential.to_owned(),
        members,
        epoch: view.epoch,
        welcome_bytes,
    }
}

fn invite_blob(invite: &SharedInviteFfi) -> Result<GroupInviteBlob, FfiError> {
    let meta = chat_core::groups::GroupInviteMeta {
        group_id: invite.group_id.clone(),
        mls_group_id: invite.mls_group_id.clone(),
        name: invite.name.clone(),
        inviter_credential: invite.inviter_credential.clone(),
        owner_credential: invite.owner_credential.clone(),
        invitee_credential: invite.invitee_credential.clone(),
        members: invite.members.clone(),
        epoch: invite.epoch,
    };
    GroupInviteBlob::new(meta, invite.welcome_bytes.clone()).map_err(|error| FfiError::Store {
        code: error.code().to_owned(),
        reason: error.to_string(),
    })
}

fn draft_view(draft: &DraftView) -> SharedDraftFfi {
    SharedDraftFfi {
        draft_id: draft.draft_id.clone(),
        conversation_id: draft.conversation_id.clone(),
        operation_id: draft.operation_id.clone(),
        state: draft.state.clone(),
    }
}

/// 发送单步投影（core 的 `SendStepView` 是私有类型，只能就地投影）。
macro_rules! send_step {
    ($step:expr) => {
        SharedSendStepFfi {
            draft_id: $step.draft_id,
            status: $step.status,
            submission_id: $step.submission_id,
            attempt_index: $step.attempt_index,
            detail: $step.detail,
            settled: $step.settled,
        }
    };
}


#[uniffi::export]
impl ChatCoreFfi {
    /// 打开共享核心：**一个** Store 句柄（同一库的第二个实例 → `STORAGE_LOCKED`）。
    #[uniffi::constructor]
    pub fn open(database_path: String, master_key_hex: String) -> Result<Arc<Self>, FfiError> {
        let core = ConversationCore::open(&database_path, &master_key_hex)
            .map_err(|error| failure!(error))?;
        Ok(Arc::new(Self {
            database_path,
            master_key_hex,
            core: Mutex::new(Some(core)),
        }))
    }

    /// 是否仍处于打开状态。
    pub fn is_open(&self) -> bool {
        match self.core.lock() {
            Ok(guard) => guard.as_ref().is_some_and(|core| core.is_open()),
            Err(_) => false,
        }
    }

    /// 关闭：释放唯一的 Store 句柄；之后所有方法 `CORE_CLOSED`。幂等。
    pub fn close(&self) {
        if let Ok(mut guard) = self.core.lock()
            && let Some(core) = guard.take()
        {
            core.close();
        }
    }

    // ---------------------------------------------------------------- 群

    /// 建群（本机 + 引导成员），返回快照与**可投递邀请**（含 Welcome 原文）。
    pub fn create_group(
        &self,
        name: String,
        local_credential: String,
        bootstrap_credential: String,
    ) -> Result<SharedCreatedGroupFfi, FfiError> {
        let mut guard = self.lock()?;
        let core = guard.as_mut().ok_or_else(core_closed)?;
        let (view, welcome) = core
            .group_create_with_welcome(&name, &local_credential, &bootstrap_credential, Vec::new())
            .map_err(|error| failure!(error))?;
        let invite = invite_view(&view, &local_credential, &bootstrap_credential, welcome);
        Ok(SharedCreatedGroupFfi {
            group: group_view(&view),
            invite,
        })
    }

    /// 用**真实被邀请者**的 KeyPackage 建群（Welcome 对方真的能用）。
    pub fn create_group_with_invitee_key_package(
        &self,
        name: String,
        invitee_credential: String,
        key_package_bytes: Vec<u8>,
        local_credential: String,
    ) -> Result<SharedCreatedGroupFfi, FfiError> {
        if key_package_bytes.is_empty() {
            return Err(invalid_input("invitee key package must not be empty"));
        }
        let mut guard = self.lock()?;
        let core = guard.as_mut().ok_or_else(core_closed)?;
        let (view, welcome) = core
            .group_create_with_invitee_key_package(
                &name,
                &local_credential,
                &invitee_credential,
                &key_package_bytes,
            )
            .map_err(|error| failure!(error))?;
        let invite = invite_view(&view, &local_credential, &invitee_credential, welcome);
        Ok(SharedCreatedGroupFfi {
            group: group_view(&view),
            invite,
        })
    }

    /// 把本地已建好的群登记到节点（建群后必须做一次，否则消息发不出去）。
    pub fn group_register_on_node(
        &self,
        group_id: String,
        endpoint_url: String,
        node_id: String,
        node_public_key_hex: Option<String>,
    ) -> Result<SharedRegistrationFfi, FfiError> {
        let mut guard = self.lock()?;
        let core = guard.as_mut().ok_or_else(core_closed)?;
        let view = core
            .group_register_on_node(
                &group_id,
                &endpoint_url,
                &node_id,
                node_public_key_hex.as_deref(),
            )
            .map_err(|error| failure!(error))?;
        Ok(SharedRegistrationFfi {
            group_id: view.group_id,
            registered_seq: view.registered_seq,
            node_id: view.node_id,
        })
    }

    /// 提交邀请（应用条目），返回真实节点回执。
    pub fn submit_invite(
        &self,
        group_id: String,
        invite: SharedInviteFfi,
        endpoint_url: String,
        node_id: String,
        node_public_key_hex: Option<String>,
    ) -> Result<SharedReceiptFfi, FfiError> {
        let blob = invite_blob(&invite)?;
        let mut guard = self.lock()?;
        let core = guard.as_mut().ok_or_else(core_closed)?;
        let receipt = core
            .group_submit_invite(
                &group_id,
                &blob,
                &endpoint_url,
                &node_id,
                node_public_key_hex.as_deref(),
            )
            .map_err(|error| failure!(error))?;
        Ok(SharedReceiptFfi {
            submission_id: receipt.submission_id,
            seq: receipt.seq,
            log_hash: receipt.log_hash,
            node_id: receipt.node_id,
        })
    }

    /// 取回最近一条邀请（可按被邀请者过滤；没有 → `None`）。
    pub fn fetch_invite(
        &self,
        group_id: String,
        endpoint_url: String,
        node_id: String,
        node_public_key_hex: Option<String>,
        invitee_credential: Option<String>,
    ) -> Result<Option<SharedInviteFfi>, FfiError> {
        let mut guard = self.lock()?;
        let core = guard.as_mut().ok_or_else(core_closed)?;
        let found = core
            .group_fetch_invite(
                &group_id,
                &endpoint_url,
                &node_id,
                node_public_key_hex.as_deref(),
                invitee_credential.as_deref(),
            )
            .map_err(|error| failure!(error))?;
        Ok(found.map(|blob| SharedInviteFfi {
            group_id: blob.meta.group_id,
            mls_group_id: blob.meta.mls_group_id,
            name: blob.meta.name,
            inviter_credential: blob.meta.inviter_credential,
            owner_credential: blob.meta.owner_credential,
            invitee_credential: blob.meta.invitee_credential,
            members: blob.meta.members,
            epoch: blob.meta.epoch,
            welcome_bytes: blob.welcome,
        }))
    }

    /// 群快照。
    pub fn snapshot(&self, group_id: String) -> Result<SharedGroupFfi, FfiError> {
        let mut guard = self.lock()?;
        let core = guard.as_mut().ok_or_else(core_closed)?;
        let view = core
            .group_snapshot(&group_id)
            .map_err(|error| failure!(error))?;
        Ok(group_view(&view))
    }

    /// 群列表（快照投影，含成员）。
    pub fn list_groups(&self) -> Result<Vec<SharedGroupFfi>, FfiError> {
        let mut guard = self.lock()?;
        let core = guard.as_mut().ok_or_else(core_closed)?;
        let listed = core.group_list().map_err(|error| failure!(error))?;
        let mut groups = Vec::with_capacity(listed.groups.len());
        for summary in listed.groups {
            match core.group_snapshot(&summary.group_id) {
                Ok(view) => groups.push(group_view(&view)),
                Err(_) => groups.push(SharedGroupFfi {
                    group_id: summary.group_id,
                    name: summary.name,
                    epoch: summary.epoch,
                    mls_group_key: String::new(),
                    members: Vec::new(),
                    seeded_revoked_devices: 0,
                }),
            }
        }
        Ok(groups)
    }

    // ---------------------------------------------------------------- 消息

    /// 新建发送意图（`queued` 草稿）。
    pub fn create_draft(
        &self,
        conversation_id: String,
        operation_id: String,
        body: String,
    ) -> Result<SharedDraftFfi, FfiError> {
        let mut guard = self.lock()?;
        let core = guard.as_mut().ok_or_else(core_closed)?;
        let draft = core
            .create_draft(&conversation_id, &operation_id, &body)
            .map_err(|error| failure!(error))?;
        Ok(draft_view(&draft))
    }

    /// 保存发送节点配置（只保存，不发请求）。
    pub fn configure_send_node(
        &self,
        endpoint_url: String,
        node_id: String,
        node_public_key_hex: Option<String>,
    ) -> Result<(), FfiError> {
        let mut guard = self.lock()?;
        let core = guard.as_mut().ok_or_else(core_closed)?;
        core.configure_send_node(&endpoint_url, &node_id, node_public_key_hex.as_deref())
            .map_err(|error| failure!(error))
    }

    /// 指定发送所用的 MLS 群键。
    pub fn configure_send_group(&self, mls_group_key: String) -> Result<(), FfiError> {
        let mut guard = self.lock()?;
        let core = guard.as_mut().ok_or_else(core_closed)?;
        core.configure_send_group(&mls_group_key)
            .map_err(|error| failure!(error))
    }

    /// 发送准备（必要时恰好加密一次）。
    pub fn send_prepare(&self, draft_id: String) -> Result<SharedSendStepFfi, FfiError> {
        let mut guard = self.lock()?;
        let core = guard.as_mut().ok_or_else(core_closed)?;
        let step = core
            .send_prepare(&draft_id)
            .map_err(|error| failure!(error))?;
        Ok(send_step!(step))
    }

    /// 发送（真实节点，三态；未知绝不伪装成功）。
    pub fn send_dispatch(&self, draft_id: String) -> Result<SharedSendStepFfi, FfiError> {
        let mut guard = self.lock()?;
        let core = guard.as_mut().ok_or_else(core_closed)?;
        let step = core
            .send_dispatch(&draft_id)
            .map_err(|error| failure!(error))?;
        Ok(send_step!(step))
    }

    /// 对账（结果未知时按 SubmissionId 查询）。
    pub fn send_reconcile(&self, draft_id: String) -> Result<SharedSendStepFfi, FfiError> {
        let mut guard = self.lock()?;
        let core = guard.as_mut().ok_or_else(core_closed)?;
        let step = core
            .send_reconcile(&draft_id)
            .map_err(|error| failure!(error))?;
        Ok(send_step!(step))
    }

    /// 单会话消息分页。
    pub fn list_messages(
        &self,
        conversation_id: String,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<SharedMessagePageFfi, FfiError> {
        let guard = self.lock()?;
        let core = guard.as_ref().ok_or_else(core_closed)?;
        let page = core
            .list_messages(&conversation_id, cursor.as_deref(), limit)
            .map_err(|error| failure!(error))?;
        Ok(SharedMessagePageFfi {
            items: page
                .items
                .into_iter()
                .map(|item| SharedMessageFfi {
                    message_id: item.message_id,
                    conversation_id: item.conversation_id,
                    author: item.author,
                    body: item.body,
                    mine: item.mine,
                    created_at_ms: item.created_at_ms,
                })
                .collect(),
            next_cursor: page.next_cursor,
        })
    }

    /// 草稿当前状态（未知草稿 → `None`；不含正文）。
    pub fn draft_status(
        &self,
        draft_id: String,
    ) -> Result<Option<SharedDraftStatusFfi>, FfiError> {
        let guard = self.lock()?;
        let core = guard.as_ref().ok_or_else(core_closed)?;
        let status = core
            .draft_status(&draft_id)
            .map_err(|error| failure!(error))?;
        // core 的 draft_status 返回 DraftView（状态字段是 `state`；尝试次数不在这里暴露）。
        Ok(status.map(|draft| SharedDraftStatusFfi {
            draft_id: draft.draft_id,
            status: draft.state,
            submission_id: draft.submission_id,
        }))
    }

    // ---------------------------------------------------------------- 同步

    /// 单轮同步（拉一页并 apply；复用 core 的单轮入口）。
    ///
    /// 需要裸 Store 句柄：临时释放核心 → 打开唯一句柄跑一轮 → **无条件**把核心装回。
    pub fn sync_once(
        &self,
        group_id: String,
        endpoint_url: String,
        node_id: String,
        node_public_key_hex: Option<String>,
    ) -> Result<SharedSyncOnceFfi, FfiError> {
        let mut guard = self.lock()?;
        let Some(core) = guard.take() else {
            return Err(core_closed());
        };
        core.close();
        let outcome = (|| -> Result<SharedSyncOnceFfi, FfiError> {
            let mut store = open_formal_store(&self.database_path, &self.master_key_hex)?;
            let client = chat_core::group_node::build_node_client(
                &store,
                &endpoint_url,
                &node_id,
                node_public_key_hex.as_deref(),
            )
            .map_err(|error| FfiError::Store {
                code: error.code().to_owned(),
                reason: error.detail().to_owned(),
            })?;
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
                .unwrap_or(0);
            let (_outcome, report) = chat_core::sync_worker::run_pull_once_reporting_at(
                &mut store,
                &client,
                &group_id,
                now_ms,
            )
            .map_err(|error| FfiError::Store {
                code: error.code().as_str().to_owned(),
                reason: error.to_string(),
            })?;
            Ok(SharedSyncOnceFfi {
                fetched: report
                    .applied
                    .saturating_add(report.stale)
                    .saturating_add(report.unsupported),
                applied: report.applied,
                stale: report.stale,
                unsupported: report.unsupported,
                needs_catchup: report.needs_catchup,
                unknown_authors: report.unknown_authors,
            })
        })();
        // 无条件把唯一的句柄装回去（失败也不留"无核心"状态）。
        match ConversationCore::open(&self.database_path, &self.master_key_hex) {
            Ok(core) => *guard = Some(core),
            Err(error) => return Err(failure!(error)),
        }
        outcome
    }
}
/// 打开裸正式库（sync_once 释放核心后用唯一句柄跑一轮）。
fn open_formal_store(
    database_path: &str,
    master_key_hex: &str,
) -> Result<chat_storage::formal::Store, FfiError> {
    let key = decode_master_key(master_key_hex)?;
    open_store(
        StoreOptions::new(database_path, KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes(key),
    )
    .map_err(storage_failure)
}

impl ChatCoreFfi {
    fn lock(&self) -> Result<MutexGuard<'_, Option<ConversationCore>>, FfiError> {
        self.core.lock().map_err(|_| FfiError::Store {
            code: "CORE_CLOSED".to_owned(),
            reason: "the shared core lock is poisoned".to_owned(),
        })
    }
}