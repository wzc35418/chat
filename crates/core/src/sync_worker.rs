//! SW 包：SYNC 常驻拉取 worker —— 把 SYNC-002/008 的决策核心接到真节点并落地本地状态。
//!
//! 三件事：
//!
//! 1. [`run_pull_once`]：读本地游标与节点签名 head → 用 [`mailbox_pull_loop`] 决策 → `Pull` 时按页拉取、
//!    **逐条验签（节点回执 + 候选绑定 + 序号/前序哈希连续性 + 群绑定）**、落库、再前进游标；
//!    缺口 / 本地越过节点 / 空页但 head 前进等一律返回 `resync_required`（绝不静默跳过）；
//! 2. [`run_catchup_once`]：用 [`catchup_plan`] 生成按序步骤，执行 `PullLog`（按序分页）与 `CommitHead`；
//!    `RebuildEpoch` / `AwaitControl` **只返回给上层**，本模块不伪造 epoch 推进；
//! 3. 失败按 sync 的退避语义**记录**（连续失败数 + 下次允许时间），**不 sleep**；游标只在“验签 + 落库”
//!    成功之后前进 —— 落库与游标是同一次 storage 快照写入，不存在“游标前进但条目没落盘”。
//!
//! 持久化形态：本轮用 storage 的密封 KV（`mls_group_state` + `mls_record`）承载“拉取游标状态 + 已验签条目”，
//! 因为 schema 里的 `group_log_entries` / `group_policy_snapshots` 还没有 storage API（storage 不在本包写作用域）。
//! 状态用 `syncpull/<sha256(group_id)>` 作为 storage group id，**epoch 字段就是游标序号**（storage 自带的
//! epoch 单调守卫因此顺带保证“游标不会倒退”）。条目每条一个 record（键 `log/<8 字节大端 seq>`）。
//!
//! 语义边界（重要）：**作者签名不在本层重复验证**。节点的 NODE-006 在接收候选时已用作者设备公钥验签，
//! 客户端没有 IDENTITY 凭据目录可查远端设备公钥；因此本层验证的是“节点确实接受并签发了这条条目，且链连续、
//! 候选字节与回执绑定”。MLS 内层应用（`process_verified_entry`）与 epoch 重建仍属上层（见 sync.md 限制）。
use std::future::Future;
use std::pin::Pin;

use chat_node_client::{NodeClient, NodeClientError, NodeClientErrorKind};
use chat_node_services::canonical::{decode_hex_bytes, hex_decode, hex_encode, sha256_concat};
use chat_node_services::group::{LogEntry, SignedLogHead};
use chat_protocol::{
    AccountId, Digest, ErrorCode, GroupId, MAX_CIPHERTEXT_BYTES, MessageId, VerifiedHead,
};
use sha2::{Digest as _, Sha256};

use crate::groups::{
    PeerEntryInput, PeerEntryOutcome, group_process_peer_entry, load_group_device,
};
use chat_storage::formal::{MlsGroupStateInput, MlsRecordInput, Store};
use chat_sync::catchup::{AwaitReason, CatchupPolicy, CatchupStep, NodeLogHead, catchup_plan};
use chat_sync::clock::{Clock, SystemClock};
use chat_sync::error::{Result as SyncResult, SyncError};
use chat_sync::pull::{
    LastPull, PullAction, PullInput, PullPolicy, ResyncReason, StopReason, mailbox_pull_loop,
};
use serde::{Deserialize, Serialize};

/// 拉取状态的 storage group id 前缀。
const PULL_STATE_PREFIX: &str = "syncpull/";
/// 已验签条目的 record 键前缀（后接 8 字节大端序号）。
const ENTRY_RECORD_PREFIX: &[u8] = b"log/";
/// 状态记录格式版本（进入密封明文；不兼容旧版本时显式失败）。
const PULL_STATE_VERSION: u8 = 1;
/// 单群最多持久化多少条已验签条目（storage 每群 provider 记录上限是 4096）。
pub const MAX_PERSISTED_ENTRIES: usize = 4096;

/// 上一次拉取的结果（持久化形态，重启后仍能驱动连续性判定）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum LastPullState {
    NotPolled,
    Page { from: u64, to: u64, items: u32 },
    Empty,
    Failed { retry_after_ms: Option<u64> },
}

impl From<LastPullState> for LastPull {
    fn from(state: LastPullState) -> Self {
        match state {
            LastPullState::NotPolled => LastPull::NotPolled,
            LastPullState::Page { from, to, items } => LastPull::Page { from, to, items },
            LastPullState::Empty => LastPull::Empty,
            LastPullState::Failed { retry_after_ms } => LastPull::Failed { retry_after_ms },
        }
    }
}

/// 本地拉取状态（密封存进正式库）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PullState {
    version: u8,
    group_id: String,
    cursor_seq: u64,
    /// 游标处的日志链尾摘要（小写 hex，32 字节）。
    cursor_log_hash: String,
    control_head: String,
    policy_version: u64,
    mls_epoch: u64,
    applied_entries: u64,
    consecutive_failures: u32,
    next_attempt_at_ms: i64,
    last_pull_at_ms: Option<i64>,
    last: LastPullState,
    /// 上次观察到的节点 head 序号（只用于“未到期”这类不需要联网的时机决策）。
    node_head: u64,
}

impl PullState {
    /// 由可信起点（建群回执里的已验证 head）构造创世状态。
    fn genesis(group_id: &str, head: VerifiedHead) -> Self {
        Self {
            version: PULL_STATE_VERSION,
            group_id: group_id.to_owned(),
            cursor_seq: head.log_sequence,
            cursor_log_hash: hex_encode(head.log_hash.as_bytes()),
            control_head: hex_encode(head.control_head.as_bytes()),
            policy_version: head.policy_version,
            mls_epoch: head.mls_epoch,
            applied_entries: 0,
            consecutive_failures: 0,
            next_attempt_at_ms: 0,
            last_pull_at_ms: None,
            last: LastPullState::NotPolled,
            node_head: 0,
        }
    }

    /// 还原成协议层的“本机已验证 head”。
    fn verified_head(&self) -> SyncResult<VerifiedHead> {
        Ok(VerifiedHead {
            group_id: GroupId::parse(&self.group_id)
                .map_err(|_| invalid_target("persisted pull state has an invalid group id"))?,
            control_head: chat_protocol::ControlHead::from_bytes(decode_digest(
                "persisted control head",
                &self.control_head,
            )?),
            policy_version: self.policy_version,
            mls_epoch: self.mls_epoch,
            log_sequence: self.cursor_seq,
            log_hash: Digest::from_bytes(decode_digest(
                "persisted log hash",
                &self.cursor_log_hash,
            )?),
        })
    }
}

/// 公开只读视图（诊断 / 测试断言用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullStateView {
    /// 已连续应用到的日志序号。
    pub cursor_seq: u64,
    /// 游标处的链尾摘要（小写 hex）。
    pub cursor_log_hash: String,
    /// 本地已验证控制头（小写 hex）。
    pub control_head: String,
    /// 本地已验证策略版本。
    pub policy_version: u64,
    /// 本地已验证 MLS epoch。
    pub mls_epoch: u64,
    /// 累计应用条目数。
    pub applied_entries: u64,
    /// 连续失败次数（退避语义的持久计数）。
    pub consecutive_failures: u32,
    /// 下一次允许拉取的时间（Unix 毫秒）。
    pub next_attempt_at_ms: i64,
}

/// 一次增量拉取的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullOutcome {
    /// 本次真正验签并落库的条目数。
    pub applied: u64,
    /// 拉取后的游标（未推进时等于调用前的值）。
    pub cursor_after: u64,
    /// 是否要求重新对齐（交给 [`run_catchup_once`]）。
    pub resync_required: bool,
    /// 需要重新对齐的原因。
    pub resync_reason: Option<ResyncReason>,
    /// 决策时看到的节点 head 序号。
    pub node_head: u64,
    /// 本轮建议的等待时长（毫秒）；未到时机/失败退避/已到 head 时为 `Some`。
    pub backoff_ms: Option<u64>,
    /// 拉取循环已终止（失败预算耗尽）时的原因。
    pub stopped: Option<StopReason>,
}

/// 一次追赶计划执行的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatchupOutcome {
    /// 本次真正验签并落库的条目数。
    pub applied: u64,
    /// 执行后的游标。
    pub cursor_after: u64,
    /// 已执行的步骤数（`PullLog` / `CommitHead`）。
    pub executed_steps: usize,
    /// 需要上层动作的步骤（`RebuildEpoch` / `AwaitControl` 及其后续），按序返回，**未执行**。
    pub upper_actions: Vec<CatchupStep>,
    /// 计划要求等待的控制结果原因。
    pub awaiting: Option<AwaitReason>,
    /// 计划本身是否已完全追平（无任何步骤）。
    pub caught_up: bool,
}

/// 初始化（或重置）某群的拉取游标。
///
/// `head` 必须是**可信起点**的已验证 head：正常情况下来自建群/加入回执（GROUPS-001 的节点回执），
/// 而不是“问节点要一个 head 然后相信它”——那样等于把信任根交给节点自我声明。
pub fn initialize_pull_state(
    store: &mut Store,
    group_id: &str,
    head: VerifiedHead,
) -> SyncResult<()> {
    let group = GroupId::parse(group_id)
        .map_err(|_| invalid_target("group id must be PROTOCOL-001 group text"))?;
    if head.group_id != group {
        return Err(invalid_target("initial head is for another group"));
    }
    let state = PullState::genesis(group_id, head);
    persist(store, &state, &[])
}

/// 读取某群的拉取状态（未初始化返回 `None`）。
pub fn load_pull_state(store: &Store, group_id: &str) -> SyncResult<Option<PullStateView>> {
    Ok(load_state(store, group_id)?.map(|state| PullStateView {
        cursor_seq: state.cursor_seq,
        cursor_log_hash: state.cursor_log_hash,
        control_head: state.control_head,
        policy_version: state.policy_version,
        mls_epoch: state.mls_epoch,
        applied_entries: state.applied_entries,
        consecutive_failures: state.consecutive_failures,
        next_attempt_at_ms: state.next_attempt_at_ms,
    }))
}

/// 读回某群已验签并落库的日志条目（按序号升序）。
pub fn load_applied_entries(store: &Store, group_id: &str) -> SyncResult<Vec<LogEntry>> {
    let storage_id = storage_group_id(group_id)?;
    let records = store.load_mls_records(&storage_id).map_err(storage_error)?;
    let mut entries = Vec::new();
    for record in records {
        if !record.record_key.starts_with(ENTRY_RECORD_PREFIX) {
            continue;
        }
        let entry: LogEntry = serde_json::from_slice(&record.value)
            .map_err(|_| corrupted("stored log entry could not be decoded"))?;
        entries.push(entry);
    }
    entries.sort_by_key(|entry| entry.receipt.seq);
    Ok(entries)
}

/// SYNC-002 落地：执行一次增量拉取。
///
/// 决策全部走 [`mailbox_pull_loop`]（不在这里重新实现一遍规则）；本函数只负责
/// “按决策读节点、验签、落库、前进游标”。失败**不 sleep**：把连续失败数与下次允许时间
/// 写进状态（下一次调用自然得到 `Backoff`），然后返回显式错误。
pub fn run_pull_once(
    store: &mut Store,
    client: &NodeClient,
    group_id: &str,
) -> SyncResult<PullOutcome> {
    run_pull_once_at(store, client, group_id, SystemClock.now_unix_ms())
}

/// 与 [`run_pull_once_at`] 相同，但额外返回本页的 MLS 应用报告（跳过/失败分类可见）。
pub fn run_pull_once_reporting_at(
    store: &mut Store,
    client: &NodeClient,
    group_id: &str,
    now_ms: i64,
) -> SyncResult<(PullOutcome, ApplyReport)> {
    run_pull_once_inner(store, client, group_id, now_ms, true)
}

/// 与 [`run_pull_once`] 相同，但显式给定时钟（测试/诊断入口，避免读系统时间）。
pub fn run_pull_once_at(
    store: &mut Store,
    client: &NodeClient,
    group_id: &str,
    now_ms: i64,
) -> SyncResult<PullOutcome> {
    run_pull_once_inner(store, client, group_id, now_ms, false).map(|(outcome, _)| outcome)
}

fn run_pull_once_inner(
    store: &mut Store,
    client: &NodeClient,
    group_id: &str,
    now_ms: i64,
    want_report: bool,
) -> SyncResult<(PullOutcome, ApplyReport)> {
    let policy = PullPolicy::DEFAULT;
    let mut state = load_state(store, group_id)?
        .ok_or_else(|| SyncError::new(ErrorCode::InvalidCursor, "pull cursor is not initialized"))?;
    // 先用“上次观察到的节点 head”做一次时机决策：未到期时不调用数据源（SYNC-002 的明确要求）。
    match mailbox_pull_loop(policy, pull_input(&state, now_ms))? {
        PullAction::Backoff {
            delay_ms,
            next_attempt_at_ms,
            reason: chat_sync::pull::BackoffReason::NotDue,
        } => {
            state.next_attempt_at_ms = next_attempt_at_ms;
            persist(store, &state, &[])?;
            return Ok((
                PullOutcome {
                    applied: 0,
                    cursor_after: state.cursor_seq,
                    resync_required: false,
                    resync_reason: None,
                    node_head: state.node_head,
                    backoff_ms: Some(delay_ms),
                    stopped: None,
                },
                ApplyReport::default(),
            ));
        }
        PullAction::Stop { reason } => {
            return Ok((
                PullOutcome {
                    applied: 0,
                    cursor_after: state.cursor_seq,
                    resync_required: false,
                    resync_reason: None,
                    node_head: state.node_head,
                    backoff_ms: None,
                    stopped: Some(reason),
                },
                ApplyReport::default(),
            ));
        }
        _ => {}
    }
    // 取节点 head 失败同样是“这一轮拉取失败”：计入连续失败与下次允许时间（不 sleep、不推进游标）。
    let head = match fetch_head(client, group_id) {
        Ok(head) => head,
        Err(error) => {
            record_failure(store, &mut state, policy, now_ms)?;
            return Err(error);
        }
    };
    let node = node_log_head(&head)?;
    state.node_head = node.log_sequence;
    let action = mailbox_pull_loop(policy, pull_input(&state, now_ms))?;
    match action {
        PullAction::Pull { from, to } => {
            let pinned = pinned_node_key(client)?;
            match fetch_and_verify(client, group_id, &state, pinned, from, to) {
                Ok(entries) => {
                    // 先 apply 进 MLS：落后（MISSING_LOG_ENTRIES）时**不推进游标**，交给追赶流程。
                    let report = apply_verified_page(store, group_id, &entries, now_ms)?;
                    if report.needs_catchup {
                        state.consecutive_failures = 0;
                        state.last_pull_at_ms = Some(now_ms);
                        persist(store, &state, &[])?;
                        return Ok((
                            PullOutcome {
                                applied: report.applied,
                                cursor_after: state.cursor_seq,
                                resync_required: true,
                                // 本机 MLS 落后于条目：走追赶流程（RebuildEpoch 由上层执行）。
                            resync_reason: Some(ResyncReason::GapTooLarge),
                                node_head: node.log_sequence,
                                backoff_ms: None,
                                stopped: None,
                            },
                            report,
                        ));
                    }
                    let applied = u64::try_from(entries.len()).unwrap_or(0);
                    let last = entries
                        .last()
                        .ok_or_else(|| corrupted("verified page is empty"))?;
                    state.cursor_seq = to;
                    state.cursor_log_hash = last.receipt.log_hash.clone();
                    state.applied_entries = state.applied_entries.saturating_add(applied);
                    state.consecutive_failures = 0;
                    state.last = LastPullState::Page {
                        from,
                        to,
                        items: u32::try_from(applied).unwrap_or(u32::MAX),
                    };
                    state.last_pull_at_ms = Some(now_ms);
                    state.next_attempt_at_ms = now_ms.saturating_add(policy.page_interval_ms);
                    // 条目与游标是同一次快照写入：不存在“游标前进但条目没落盘”。
                    persist(store, &state, &entries)?;
                    let _ = want_report;
                    Ok((
                        PullOutcome {
                            applied,
                            cursor_after: state.cursor_seq,
                            resync_required: false,
                            resync_reason: None,
                            node_head: node.log_sequence,
                            backoff_ms: None,
                            stopped: None,
                        },
                        report,
                    ))
                }
                Err(error) => {
                    record_failure(store, &mut state, policy, now_ms)?;
                    Err(error)
                }
            }
        }
        PullAction::Resync { reason } => Ok((
            PullOutcome {
                applied: 0,
                cursor_after: state.cursor_seq,
                resync_required: true,
                resync_reason: Some(reason),
                node_head: node.log_sequence,
                backoff_ms: None,
                stopped: None,
            },
            ApplyReport::default(),
        )),
        PullAction::Backoff {
            delay_ms,
            next_attempt_at_ms,
            ..
        } => {
            // 未到时机/已到 head/页间隔：不调用数据源，只把下次时间写回（幂等）。
            state.next_attempt_at_ms = next_attempt_at_ms;
            persist(store, &state, &[])?;
            Ok((
                PullOutcome {
                    applied: 0,
                    cursor_after: state.cursor_seq,
                    resync_required: false,
                    resync_reason: None,
                    node_head: node.log_sequence,
                    backoff_ms: Some(delay_ms),
                    stopped: None,
                },
                ApplyReport::default(),
            ))
        }
        PullAction::Stop { reason } => Ok((
            PullOutcome {
                applied: 0,
                cursor_after: state.cursor_seq,
                resync_required: false,
                resync_reason: None,
                node_head: node.log_sequence,
                backoff_ms: None,
                stopped: Some(reason),
            },
            ApplyReport::default(),
        )),
    }
}

/// SYNC-008 落地：执行一次追赶计划。
///
/// `PullLog` 按序分页执行（每页验签后落库才继续）；`CommitHead` 只在**所有**前置步骤都已执行
/// （也就是真的验证到计划目标）后落库；`RebuildEpoch` / `AwaitControl` 停下来交给上层，
/// 本函数绝不猜测 epoch、绝不把未执行完的计划说成“已追平”。
pub fn run_catchup_once(
    store: &mut Store,
    client: &NodeClient,
    group_id: &str,
) -> SyncResult<CatchupOutcome> {
    run_catchup_once_at(store, client, group_id, SystemClock.now_unix_ms())
}

/// 与 [`run_catchup_once`] 相同，但显式给定时钟。
pub fn run_catchup_once_at(
    store: &mut Store,
    client: &NodeClient,
    group_id: &str,
    now_ms: i64,
) -> SyncResult<CatchupOutcome> {
    let policy = CatchupPolicy::DEFAULT;
    let mut state = load_state(store, group_id)?
        .ok_or_else(|| SyncError::new(ErrorCode::InvalidCursor, "pull cursor is not initialized"))?;
    let head = fetch_head(client, group_id)?;
    let node = node_log_head(&head)?;
    let pinned = pinned_node_key(client)?;
    let plan = catchup_plan(state.verified_head()?, node, policy)?;
    let mut applied = 0_u64;
    let mut executed = 0_usize;
    let mut upper_actions: Vec<CatchupStep> = Vec::new();
    let mut steps = plan.steps.iter().copied();
    while let Some(step) = steps.next() {
        match step {
            CatchupStep::PullLog { from, to } => {
                let entries = fetch_and_verify(client, group_id, &state, pinned, from, to)?;
                let count = u64::try_from(entries.len()).unwrap_or(0);
                let last = entries
                    .last()
                    .ok_or_else(|| corrupted("verified page is empty"))?;
                state.cursor_seq = to;
                state.cursor_log_hash = last.receipt.log_hash.clone();
                state.applied_entries = state.applied_entries.saturating_add(count);
                state.last = LastPullState::Page {
                    from,
                    to,
                    items: u32::try_from(count).unwrap_or(u32::MAX),
                };
                state.last_pull_at_ms = Some(now_ms);
                state.consecutive_failures = 0;
                applied = applied.saturating_add(count);
                executed += 1;
                persist(store, &state, &entries)?;
            }
            CatchupStep::CommitHead {
                log_sequence,
                mls_epoch,
            } => {
                // 只有日志真的验到这里才允许把 head 的控制状态采纳为已验证。
                if state.cursor_seq != log_sequence {
                    upper_actions.push(step);
                    upper_actions.extend(steps);
                    break;
                }
                state.control_head = head.control_head.clone();
                state.policy_version = head.policy_version;
                state.mls_epoch = mls_epoch;
                executed += 1;
                persist(store, &state, &[])?;
            }
            // 需要上层动作：不执行、不伪造推进，交给调用方。
            CatchupStep::RebuildEpoch { .. } | CatchupStep::AwaitControl { .. } => {
                upper_actions.push(step);
                upper_actions.extend(steps);
                break;
            }
        }
    }
    Ok(CatchupOutcome {
        applied,
        cursor_after: state.cursor_seq,
        executed_steps: executed,
        upper_actions,
        awaiting: plan.awaiting(),
        caught_up: plan.is_caught_up(),
    })
}


/// 一页条目的 MLS 应用报告（SYNC-002 补全：拉到的条目真正 apply 进 MLS）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApplyReport {
    /// 真正应用（消息/Commit/提案）的条目数。
    pub applied: u64,
    /// epoch 已过期（STALE_EPOCH）被跳过的条目数。
    pub stale: u64,
    /// 无法应用（UNSUPPORTED_ENTRY：载荷不是本群的 MLS 消息、本机不在该群、非 UTF-8 正文等）的条目数。
    pub unsupported: u64,
    /// 本机落后于条目 epoch（MISSING_LOG_ENTRIES）：需要先追赶，本页**不推进游标**。
    pub needs_catchup: bool,
    /// 作者凭据**不在凭据目录**里、只能用确定性占位的条目数（显式计数，绝不静默伪造）。
    pub unknown_authors: u64,
}

impl ApplyReport {
    /// 跳过总数（stale + unsupported）。
    pub const fn skipped(&self) -> u64 {
        self.stale.saturating_add(self.unsupported)
    }

    /// 全部条目都被跳过（没有真正应用）。
    pub const fn all_skipped(&self) -> bool {
        self.applied == 0 && self.skipped() > 0
    }
}

/// 应用消息的确定性 message id（同一条目重放得到同一个 id → 落库幂等）。
pub fn applied_message_id(group_id: &str, seq: u64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"chat/sync/applied-message/v1");
    hasher.update((group_id.len() as u32).to_be_bytes());
    hasher.update(group_id.as_bytes());
    hasher.update(seq.to_be_bytes());
    let digest: [u8; 32] = hasher.finalize().into();
    MessageId::from_bytes(digest).encode()
}

/// 远端作者的**占位**账户 ID：**只**在凭据目录里查不到该凭据时使用（并显式计入
/// `ApplyReport::unknown_authors`）。真实映射见 `crates/storage` 的 0011 凭据目录。
fn placeholder_author_account(credential: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"chat/sync/applied-author/v1");
    hasher.update((credential.len() as u32).to_be_bytes());
    hasher.update(credential.as_bytes());
    let digest: [u8; 32] = hasher.finalize().into();
    AccountId::from_bytes(digest).encode()
}


/// 用**凭据目录**把作者凭据解析成真实账户：`(账户文本, 是否已知)`。
///
/// 目录命中 → 返回目录里的 `account_id` 文本（`true`）；未命中 → 返回确定性占位（`false`），
/// 调用方必须把 `false` 计入 `ApplyReport::unknown_authors`（**绝不静默伪造**）。
pub fn resolve_author_account(
    store: &Store,
    credential: &str,
) -> (String, bool) {
    match store.load_credential_directory(credential) {
        Ok(Some(record)) => (AccountId::from_bytes(record.account_id).encode(), true),
        Ok(None) | Err(_) => (placeholder_author_account(credential), false),
    }
}

/// 把一条已通过外层验证的条目应用进 MLS 并落库。
///
/// 分类（与 SYNC-002 一致，绝不静默丢数据）：
///
/// - 应用成功：应用消息 → 写正式消息行（确定性 message id，重放幂等）；Commit 合并 epoch+1、
///   提案暂存 —— 三者都由 {@link crate::groups::group_process_peer_entry} 负责持久化；
/// - `STALE_EPOCH`：跳过并计数（游标照常前进，否则会被一条旧条目永久卡住）；
/// - `MISSING_LOG_ENTRIES`：本机落后 → 置 `needs_catchup`，**不推进游标**，交给追赶流程；
/// - `UNSUPPORTED_ENTRY` / 载荷不是 MLS 消息 / 本机不在该群 / 正文非 UTF-8：跳过并计数；
/// - 其它错误（例如 STATE_CORRUPTED）**向上抛出**：游标不前进。
pub fn apply_verified_page(
    store: &mut Store,
    group_id: &str,
    entries: &[LogEntry],
    now_ms: i64,
) -> SyncResult<ApplyReport> {
    let mut report = ApplyReport::default();
    // 本机不是该群成员（没有群快照）→ 整页按 UNSUPPORTED 跳过，与“只落库不 apply”的旧行为一致。
    let mut device = match load_group_device(store, group_id) {
        Ok(device) => device,
        Err(_) => {
            report.unsupported = u64::try_from(entries.len()).unwrap_or(u64::MAX);
            return Ok(report);
        }
    };
    for entry in entries {
        let candidate = &entry.candidate.candidate;
        let payload = match decode_hex_bytes(
            "entry payload",
            &candidate.encrypted_payload,
            MAX_CIPHERTEXT_BYTES,
        ) {
            Ok(bytes) => bytes,
            Err(_) => {
                report.unsupported = report.unsupported.saturating_add(1);
                continue;
            }
        };
        let input = PeerEntryInput::new(
            group_id,
            candidate.mls_epoch,
            candidate.author_device_id.clone(),
            payload,
        );
        match group_process_peer_entry(&mut device, store, &input) {
            Ok(PeerEntryOutcome::Application { plaintext, .. }) => {
                let Ok(body) = String::from_utf8(plaintext) else {
                    report.unsupported = report.unsupported.saturating_add(1);
                    continue;
                };
                // 凭据目录：把作者凭据映射成真实 AccountId；查不到才退回占位并显式计数。
                let (author_account, known) =
                    resolve_author_account(store, &candidate.author_device_id);
                if !known {
                    report.unknown_authors = report.unknown_authors.saturating_add(1);
                }
                let input = chat_storage::formal::VerifiedEntryInput {
                    conversation_id: group_id.to_owned(),
                    title: String::new(),
                    message_id: applied_message_id(group_id, entry.receipt.seq),
                    author_account,
                    author_device: candidate.author_device_id.clone(),
                    body,
                    mine: false,
                    created_at_ms: now_ms,
                };
                store
                    .apply_verified_entry(input)
                    .map_err(|error| {
                        // StorageError::code() 是冻结码；正文/时间戳不合法属数据问题，不静默跳过。
                        let _ = error.code();
                        SyncError::new(
                            ErrorCode::StorageFailure,
                            "applied message could not be persisted",
                        )
                    })?;
                report.applied = report.applied.saturating_add(1);
            }
            Ok(PeerEntryOutcome::Commit { .. } | PeerEntryOutcome::Proposal { .. }) => {
                report.applied = report.applied.saturating_add(1);
            }
            Ok(PeerEntryOutcome::OwnPendingCommit { .. }) => {
                report.unsupported = report.unsupported.saturating_add(1);
            }
            Err(error) => match error.code() {
                "STALE_EPOCH" => report.stale = report.stale.saturating_add(1),
                "MISSING_LOG_ENTRIES" => {
                    report.needs_catchup = true;
                    break;
                }
                "UNSUPPORTED_ENTRY"
                | "INVALID_CIPHERTEXT"
                | "INVALID_CANDIDATE"
                | "INVALID_TARGET"
                | "INVALID_CONTROL_RESULT"
                | "INVALID_INPUT"
                | "PERMISSION_DENIED" => {
                    report.unsupported = report.unsupported.saturating_add(1);
                }
                _ => {
                    // 其它错误（例如 STATE_CORRUPTED）不静默跳过：游标不前进。
                    return Err(SyncError::new(
                        ErrorCode::StateCorrupted,
                        "a verified entry could not be applied to the MLS group",
                    ));
                }
            },
        }
    }
    Ok(report)
}
/// 验证一页节点条目：节点回执、候选绑定、群绑定、序号与前序哈希连续性。
///
/// 与 [`run_pull_once`] 内部的验签是**同一个函数**：公开出来是为了能用“手工构造的坏页”验证拒绝路径
/// （不依赖恶意节点实现），也便于上层在别的取数通道上复用同一套校验。
pub fn verify_pull_page(
    pinned_node_key: &[u8; 32],
    group_id: &str,
    previous_log_hash: Digest,
    from: u64,
    to: u64,
    entries: &[LogEntry],
) -> SyncResult<Vec<LogEntry>> {
    if from == 0 || to < from {
        return Err(invalid_target("pull range is empty"));
    }
    let expected_items = to - from + 1;
    if u64::try_from(entries.len()).unwrap_or(u64::MAX) != expected_items {
        return Err(SyncError::new(
            ErrorCode::MissingLogEntries,
            "node page does not cover the requested range",
        ));
    }
    let mut previous = *previous_log_hash.as_bytes();
    let mut verified = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let expected_seq = from + u64::try_from(index).unwrap_or(u64::MAX);
        let receipt = &entry.receipt;
        if receipt.seq != expected_seq {
            return Err(SyncError::new(
                ErrorCode::MissingLogEntries,
                "log entry sequence is not contiguous",
            ));
        }
        if receipt.group_id != group_id || entry.candidate.candidate.group_id != group_id {
            return Err(invalid_target("log entry belongs to another group"));
        }
        let entry_previous = decode_digest("log entry previous hash", &receipt.previous_hash)?;
        if entry_previous != previous {
            return Err(SyncError::new(
                ErrorCode::LogConflict,
                "log entry does not continue the verified chain",
            ));
        }
        receipt.verify(pinned_node_key).map_err(|_| {
            SyncError::new(ErrorCode::InvalidSignature, "node receipt signature is invalid")
        })?;
        receipt
            .verify_candidate(&entry.candidate, MAX_CIPHERTEXT_BYTES)
            .map_err(|_| {
                SyncError::new(
                    ErrorCode::InvalidSignature,
                    "node receipt does not bind the submitted candidate",
                )
            })?;
        previous = decode_digest("log entry hash", &receipt.log_hash)?;
        verified.push(entry.clone());
    }
    Ok(verified)
}

// ---------------------------------------------------------------- 内部：取数与验签

fn fetch_and_verify(
    client: &NodeClient,
    group_id: &str,
    state: &PullState,
    pinned: &[u8; 32],
    from: u64,
    to: u64,
) -> SyncResult<Vec<LogEntry>> {
    let limit = u32::try_from(to - from + 1)
        .map_err(|_| invalid_target("pull range exceeds the page limit"))?;
    let after = from.saturating_sub(1);
    let page = block_on_client(client, |client| {
        Box::pin(async move {
            client
                .fetch_log(group_id, after, limit)
                .await
                .map_err(map_client_error)
        })
    })?;
    if page.group_id != group_id {
        return Err(invalid_target("node log page is for another group"));
    }
    // 节点 head 里的身份必须就是固定的那个（客户端已验签，这里再显式绑定一次）。
    let head_key = decode_digest("node head key", &page.head.node_public_key)?;
    if &head_key != pinned {
        return Err(SyncError::new(
            ErrorCode::InvalidSignature,
            "node head is signed by an unexpected key",
        ));
    }
    if page.head.seq < to {
        return Err(SyncError::new(
            ErrorCode::LogConflict,
            "node head is behind the requested range",
        ));
    }
    let previous = Digest::from_bytes(decode_digest(
        "cursor log hash",
        &state.cursor_log_hash,
    )?);
    verify_pull_page(pinned, group_id, previous, from, to, &page.entries)
}

fn fetch_head(client: &NodeClient, group_id: &str) -> SyncResult<SignedLogHead> {
    block_on_client(client, |client| {
        Box::pin(async move {
            client
                .get_head(group_id)
                .await
                .map_err(map_client_error)
        })
    })
}

fn pinned_node_key(client: &NodeClient) -> SyncResult<&[u8; 32]> {
    client.endpoint().node_public_key().ok_or_else(|| {
        SyncError::new(
            ErrorCode::InvalidTarget,
            "pull requires a pinned node public key",
        )
    })
}

fn node_log_head(head: &SignedLogHead) -> SyncResult<NodeLogHead> {
    Ok(NodeLogHead {
        log_sequence: head.seq,
        log_hash: Digest::from_bytes(decode_digest("node head log hash", &head.log_hash)?),
        control_head: chat_protocol::ControlHead::from_bytes(decode_digest(
            "node head control head",
            &head.control_head,
        )?),
        policy_version: head.policy_version,
        mls_epoch: head.mls_epoch,
    })
}

// ---------------------------------------------------------------- 内部：状态读写

fn storage_group_id(group_id: &str) -> SyncResult<String> {
    let digest = sha256_concat(&[b"chat/core/sync-pull-state/v1", group_id.as_bytes()]);
    Ok(format!("{PULL_STATE_PREFIX}{}", hex_encode(&digest)))
}

fn entry_record_key(sequence: u64) -> Vec<u8> {
    let mut key = ENTRY_RECORD_PREFIX.to_vec();
    key.extend_from_slice(&sequence.to_be_bytes());
    key
}

fn load_state(store: &Store, group_id: &str) -> SyncResult<Option<PullState>> {
    GroupId::parse(group_id)
        .map_err(|_| invalid_target("group id must be PROTOCOL-001 group text"))?;
    let storage_id = storage_group_id(group_id)?;
    let Some(record) = store.load_mls_group_state(&storage_id).map_err(storage_error)? else {
        return Ok(None);
    };
    let state: PullState = serde_json::from_slice(&record.state)
        .map_err(|_| corrupted("persisted pull state could not be decoded"))?;
    if state.version != PULL_STATE_VERSION {
        return Err(corrupted("persisted pull state has an unsupported version"));
    }
    if state.group_id != group_id {
        return Err(corrupted("persisted pull state belongs to another group"));
    }
    // storage 的 epoch 就是游标：两边不一致说明状态被改过，直接拒绝。
    if record.epoch != state.cursor_seq {
        return Err(corrupted(
            "persisted pull state cursor does not match its storage epoch",
        ));
    }
    Ok(Some(state))
}

fn persist(store: &mut Store, state: &PullState, new_entries: &[LogEntry]) -> SyncResult<()> {
    let storage_id = storage_group_id(&state.group_id)?;
    // storage 的快照写入会整组替换记录，因此必须把已有条目一起带上（旧 epoch 的记录会被删除）。
    let mut records: Vec<MlsRecordInput> = store
        .load_mls_records(&storage_id)
        .map_err(storage_error)?
        .into_iter()
        .filter(|record| record.record_key.starts_with(ENTRY_RECORD_PREFIX))
        .map(|record| MlsRecordInput {
            record_key: record.record_key,
            value: record.value,
        })
        .collect();
    for entry in new_entries {
        let key = entry_record_key(entry.receipt.seq);
        let value = serde_json::to_vec(entry)
            .map_err(|_| corrupted("verified log entry could not be encoded"))?;
        match records.iter_mut().find(|record| record.record_key == key) {
            Some(existing) => existing.value = value,
            None => records.push(MlsRecordInput {
                record_key: key,
                value,
            }),
        }
    }
    if records.len() > MAX_PERSISTED_ENTRIES {
        return Err(SyncError::new(
            ErrorCode::ContentTooLarge,
            "too many persisted log entries for one group",
        ));
    }
    records.sort_by(|left, right| left.record_key.cmp(&right.record_key));
    let state_bytes = serde_json::to_vec(state)
        .map_err(|_| corrupted("pull state could not be encoded"))?;
    store
        .save_mls_group_state(MlsGroupStateInput {
            group_id: storage_id,
            epoch: state.cursor_seq,
            state: state_bytes,
            records,
        })
        .map_err(storage_error)?;
    Ok(())
}

/// 组装一次决策输入（同一个函数给“时机预检”和“拿到新 head 后的决策”复用）。
fn pull_input(state: &PullState, now_ms: i64) -> PullInput {
    PullInput {
        local_tail: state.cursor_seq,
        node_head: state.node_head,
        last: state.last.into(),
        consecutive_failures: state.consecutive_failures,
        now_ms,
        last_pull_at_ms: state.last_pull_at_ms,
        next_attempt_at_ms: state.next_attempt_at_ms,
    }
}

fn record_failure(
    store: &mut Store,
    state: &mut PullState,
    policy: PullPolicy,
    now_ms: i64,
) -> SyncResult<()> {
    state.consecutive_failures = state.consecutive_failures.saturating_add(1);
    let delay_ms = policy.failure_delay_for(state.consecutive_failures);
    state.next_attempt_at_ms = now_ms.saturating_add(delay_ms);
    // 关键：不要把 `LastPull::Failed` 存进状态。决策核心遇到 Failed 会**再算一次退避**，
    // 若把 Failed 持久化，到期后每轮都只算出新的等待而永不重试。这里改写为
    // “连续失败数 + 下次允许时间”（等待时长同样来自 `failure_delay_for`），到期后正常重试。
    state.last = LastPullState::NotPolled;
    // 只写退避状态：游标与已落库条目一个字节都不动。
    persist(store, state, &[])
}

// ---------------------------------------------------------------- 内部：错误与桥

fn invalid_target(detail: &'static str) -> SyncError {
    SyncError::new(ErrorCode::InvalidTarget, detail)
}

fn corrupted(detail: &'static str) -> SyncError {
    SyncError::new(ErrorCode::StateCorrupted, detail)
}

fn storage_error(_error: chat_storage::formal::StorageError) -> SyncError {
    // 只保留冻结码，不回显 SQL/路径等内部细节。
    SyncError::new(ErrorCode::StorageFailure, "pull state storage failed")
}

fn decode_digest(field: &'static str, value: &str) -> SyncResult<[u8; 32]> {
    hex_decode::<32>(field, value)
        .map_err(|_| corrupted("stored digest is not canonical hex"))
}

fn map_client_error(error: NodeClientError) -> SyncError {
    let code = match error.kind() {
        NodeClientErrorKind::Network | NodeClientErrorKind::Timeout => ErrorCode::NodeUnavailable,
        _ => error.code(),
    };
    SyncError::new(code, "the node could not serve this pull")
}

/// 桥用 future（显式带上客户端借用生命周期）。
type ClientFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 同步桥：独立线程 + 独立 current-thread 运行时跑异步客户端调用。
///
/// 与 `group_node` 的同名桥是同一套做法（该文件不在本包写作用域，故此处独立实现）；
/// 用 `thread::scope` 借出 `&NodeClient`，不需要 `Clone`/`'static`，且在 tokio 运行时内也不会嵌套 panic。
fn block_on_client<'a, T>(
    client: &'a NodeClient,
    operation: impl FnOnce(&'a NodeClient) -> ClientFuture<'a, SyncResult<T>> + Send,
) -> SyncResult<T>
where
    T: Send + 'a,
{
    std::thread::scope(|scope| {
        let handle = scope.spawn(move || -> SyncResult<T> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| {
                    SyncError::new(ErrorCode::NodeUnavailable, "pull runtime could not be built")
                })?;
            runtime.block_on(operation(client))
        });
        handle.join().map_err(|_| {
            SyncError::new(ErrorCode::NodeUnavailable, "pull worker thread panicked")
        })?
    })
}
