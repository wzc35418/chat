//! 用真实节点客户端实现 [`NodeSubmitter`]（NODE-006 append / NODE-007 日志 / NODE-008 head）。
//!
//! 本模块把 `send_orchestrator` 的可注入端口接到真实网络：
//!
//! - `submit`：先读节点当前公开 head（`GET /v1/groups/{id}/head`），再用**节点给出的**
//!   `control_head / policy_version / mls_epoch` 作为候选的 CAS 前置状态，把
//!   `(SubmissionId, 密文)` 追加到群日志（`POST /v1/groups/{id}/entries`）。
//!   客户端从不本地推断这些字段，也不发送任何明文或密钥。
//! - `query`：按 SubmissionId 扫描节点日志（`GET /v1/groups/{id}/log` 分页）；
//!   找到即 `Accepted`（附节点回执的 seq），**找不到返回 `Unknown`（绝不返回 Rejected）**。
//!
//! 三态映射（与 docs/implementation/core.md 的实现记录一致）：
//!
//! |节点结果|映射|理由|
//! |---|---|---|
//! |成功（节点已分配 seq）|`Accepted`|回执绑定本次候选，客户端已复核|
//! |409 冲突（相异密文/群 ID 冲突/stale head/epoch）|`Rejected`|同一次尝试永远不会被接受，必须重建|
//! |鉴权/权限永久失败（签名、凭据、成员窗口）|`Rejected`|换挑战或重试都不会变好|
//! |请求形状/内容上限|`Rejected`|这次密文本身不会被接受|
//! |网络/超时/限流/节点 5xx/无法解释的响应|`Unknown`|节点可能已经写入：只允许按 ID 对账，绝不重发|
//! |本地配置错误（客户端/桥不可用）|`Err`|这是本地故障，状态不变，稍后可重试|
//!
//! 同步/异步边界：`NodeSubmitter` 的方法是同步的，而 `NodeClient` 是异步的。本模块用一条
//! **独立线程 + current-thread 运行时**做桥（[`SubmitBridge`]），因此可以在任何上下文里被
//! 调用（包括已经是异步运行时的调用方），不会出现“在运行时里阻塞”的 panic。
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use chat_node_client::{NodeClient, NodeClientError, NodeClientErrorKind};
use chat_node_services::canonical::hex_encode;
use chat_node_services::group::{
    AppendResult, GroupCandidate, LogPage, SignedLogHead, KIND_APPLICATION,
};
use chat_protocol::{ErrorCode, GroupId, SubmissionId};
use chat_sync::SyncError;

use crate::send_orchestrator::{NodeSubmitter, SendResult, SubmitAttempt, SubmitOutcome};

/// 查询时每页读取的日志条数（节点侧还有更小的策略上限）。
pub const DEFAULT_QUERY_PAGE_LIMIT: u32 = 50;
/// 允许的最大单页条数（与节点 `MAX_GROUP_LOG_PAGE_ENTRIES` 保持一致）。
pub const MAX_QUERY_PAGE_LIMIT: u32 = 500;

/// 桥内部错误：区分“节点回答了一个错误”和“桥本身不可用”。
#[derive(Debug)]
enum BridgeError {
    /// 节点返回了错误（含网络失败）。
    Client(Box<NodeClientError>),
    /// 桥线程已退出/通道关闭：这是本地故障，不是节点的回答。
    Unavailable,
}

impl BridgeError {
    fn into_sync(self) -> SyncError {
        match self {
            Self::Client(error) => sync_error(&error),
            Self::Unavailable => bridge_unavailable(),
        }
    }
}

/// 一次桥调用。
enum BridgeCall {
    Head(Sender<Result<SignedLogHead, NodeClientError>>),
    Append {
        candidate: Box<GroupCandidate>,
        reply: Sender<Result<AppendResult, NodeClientError>>,
    },
    Log {
        after: u64,
        limit: u32,
        reply: Sender<Result<LogPage, NodeClientError>>,
    },
}

/// 同步→异步桥：独立线程 + current-thread 运行时。
struct SubmitBridge {
    calls: Sender<BridgeCall>,
    thread: Option<JoinHandle<()>>,
}

impl SubmitBridge {
    fn spawn(client: Arc<NodeClient>, group_id: String) -> SendResult<Self> {
        let (calls, inbox) = channel();
        let thread = thread::Builder::new()
            .name("chat-node-submit".to_owned())
            .spawn(move || run_bridge(client, group_id, inbox))
            .map_err(|_| {
                SyncError::new(
                    ErrorCode::InternalFailure,
                    "node submitter thread could not start",
                )
            })?;
        Ok(Self {
            calls,
            thread: Some(thread),
        })
    }

    fn head(&self) -> Result<SignedLogHead, BridgeError> {
        let (reply, answer) = channel();
        self.dispatch(BridgeCall::Head(reply))?;
        answer
            .recv()
            .map_err(|_| BridgeError::Unavailable)?
            .map_err(|error| BridgeError::Client(Box::new(error)))
    }

    fn append(&self, candidate: GroupCandidate) -> Result<AppendResult, BridgeError> {
        let (reply, answer) = channel();
        self.dispatch(BridgeCall::Append {
            candidate: Box::new(candidate),
            reply,
        })?;
        answer
            .recv()
            .map_err(|_| BridgeError::Unavailable)?
            .map_err(|error| BridgeError::Client(Box::new(error)))
    }

    fn log_page(&self, after: u64, limit: u32) -> Result<LogPage, BridgeError> {
        let (reply, answer) = channel();
        self.dispatch(BridgeCall::Log {
            after,
            limit,
            reply,
        })?;
        answer
            .recv()
            .map_err(|_| BridgeError::Unavailable)?
            .map_err(|error| BridgeError::Client(Box::new(error)))
    }

    fn dispatch(&self, call: BridgeCall) -> Result<(), BridgeError> {
        self.calls.send(call).map_err(|_| BridgeError::Unavailable)
    }
}

impl Drop for SubmitBridge {
    fn drop(&mut self) {
        // 丢掉发送端 → 桥线程的 recv 立刻返回 Err → 循环退出；再 join，避免线程继续跑网络请求。
        let (dead, _) = channel();
        let _ = std::mem::replace(&mut self.calls, dead);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_bridge(client: Arc<NodeClient>, group_id: String, inbox: Receiver<BridgeCall>) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        // 运行时建不起来：直接退出，调用方会看到 BridgeError::Unavailable 并显式失败。
        return;
    };
    while let Ok(call) = inbox.recv() {
        runtime.block_on(async {
            match call {
                BridgeCall::Head(reply) => {
                    let _ = reply.send(client.get_head(&group_id).await);
                }
                BridgeCall::Append { candidate, reply } => {
                    let _ = reply.send(client.append_entry(&candidate).await);
                }
                BridgeCall::Log { after, limit, reply } => {
                    let _ = reply.send(client.fetch_log(&group_id, after, limit).await);
                }
            }
        });
    }
}

/// 三态映射的结论。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mapping {
    /// 明确拒绝：重建新 SubmissionId。
    Rejected,
    /// 结果未知：只允许按原 SubmissionId 对账。
    Unknown,
    /// 本地配置/客户端错误：原样上抛 `Err`，本地状态不变。
    Misconfigured,
}

/// 节点错误 → 三态（映射表与模块文档一致；不含任何正文/密钥）。
fn classify(kind: NodeClientErrorKind, node_code: Option<&'static str>) -> Mapping {
    match kind {
        NodeClientErrorKind::Config => Mapping::Misconfigured,
        NodeClientErrorKind::Authentication
        | NodeClientErrorKind::Permission
        | NodeClientErrorKind::Conflict
        | NodeClientErrorKind::NotFound
        | NodeClientErrorKind::InvalidRequest => Mapping::Rejected,
        NodeClientErrorKind::Limit => match node_code {
            // 限流/容量类：节点只是暂时没空，这次提交没有被接受也没有被否决。
            Some("RATE_LIMITED" | "CHALLENGE_CAPACITY_EXCEEDED") => Mapping::Unknown,
            // 内容相关的上限：这份密文永远不会被接受，必须重建。
            _ => Mapping::Rejected,
        },
        NodeClientErrorKind::Network
        | NodeClientErrorKind::Timeout
        | NodeClientErrorKind::Protocol
        | NodeClientErrorKind::Internal => Mapping::Unknown,
    }
}

/// 节点的固定短语（不含 URL/正文/密钥）。
fn client_detail(kind: NodeClientErrorKind) -> &'static str {
    match kind {
        NodeClientErrorKind::Network => "node connection failed",
        NodeClientErrorKind::Timeout => "node request timed out",
        NodeClientErrorKind::Protocol => "node response did not match the protocol",
        NodeClientErrorKind::Authentication => "node rejected the authentication",
        NodeClientErrorKind::Permission => "node denied the operation",
        NodeClientErrorKind::Conflict => "node reported a conflict",
        NodeClientErrorKind::NotFound => "node does not know the target",
        NodeClientErrorKind::InvalidRequest => "node rejected the request",
        NodeClientErrorKind::Limit => "node limit reached",
        NodeClientErrorKind::Internal => "node internal failure",
        NodeClientErrorKind::Config => "node client is misconfigured",
    }
}

/// 节点错误 → sync 错误（保留 PROTOCOL-014 码，静态 detail）。
fn sync_error(error: &NodeClientError) -> SyncError {
    SyncError::new(error.code(), client_detail(error.kind()))
}

fn bridge_unavailable() -> SyncError {
    SyncError::new(
        ErrorCode::InternalFailure,
        "node submitter bridge is unavailable",
    )
}

/// 节点错误 → 三态结果（`Config` 是本地故障，原样上抛）。
fn outcome_for(error: &NodeClientError) -> SendResult<SubmitOutcome> {
    match classify(error.kind(), error.node_code()) {
        Mapping::Rejected => Ok(SubmitOutcome::Rejected {
            detail: rejected_detail(error),
        }),
        Mapping::Unknown => Ok(SubmitOutcome::Unknown {
            detail: unknown_detail(error),
        }),
        Mapping::Misconfigured => Err(sync_error(error)),
    }
}

fn rejected_detail(error: &NodeClientError) -> String {
    match error.node_code() {
        Some(code) => format!("node rejected the attempt ({code})"),
        None => format!(
            "node rejected the attempt (http {})",
            error.http_status().unwrap_or(0)
        ),
    }
}

fn unknown_detail(error: &NodeClientError) -> String {
    match error.node_code() {
        Some(code) => format!("node result is unknown ({code})"),
        None => format!("node result is unknown (http {})", error.http_status().unwrap_or(0)),
    }
}

#[derive(Default)]
struct SubmitterStats {
    submits: AtomicU64,
    queries: AtomicU64,
    last_receipt: Mutex<Option<AppendResult>>,
}

/// 提交/对账计数与最近回执（克隆共享；可在编排器之外观测“没有重发”）。
#[derive(Clone, Default)]
pub struct SubmitterMetrics {
    inner: Arc<SubmitterStats>,
}

impl SubmitterMetrics {
    /// 已发生的节点提交次数。
    pub fn submit_calls(&self) -> u64 {
        self.inner.submits.load(Ordering::SeqCst)
    }

    /// 已发生的按 ID 对账次数。
    pub fn query_calls(&self) -> u64 {
        self.inner.queries.load(Ordering::SeqCst)
    }

    /// 最近一次被节点接受的回执（诊断用；含 seq/哈希，不含正文）。
    pub fn last_receipt(&self) -> Option<AppendResult> {
        self.inner
            .last_receipt
            .lock()
            .ok()
            .and_then(|receipt| receipt.clone())
    }
}

impl std::fmt::Debug for SubmitterMetrics {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SubmitterMetrics")
            .field("submit_calls", &self.submit_calls())
            .field("query_calls", &self.query_calls())
            .finish()
    }
}

/// 真实节点实现：一个实例绑定一个群与一台设备（设备的签名钥在 [`NodeClient`] 里）。
pub struct NodeClientSubmitter {
    bridge: SubmitBridge,
    group_id: String,
    device_id: String,
    page_limit: u32,
    metrics: SubmitterMetrics,
}

impl std::fmt::Debug for NodeClientSubmitter {
    /// 只暴露群、分页与调用计数；客户端与签名钥都不进日志。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NodeClientSubmitter")
            .field("group_id", &self.group_id)
            .field("device_id", &self.device_id)
            .field("page_limit", &self.page_limit)
            .field("metrics", &self.metrics)
            .finish()
    }
}

impl NodeClientSubmitter {
    /// 用真实节点客户端与固定群 ID 构造（默认每页 50 条）。
    ///
    /// `group_id` 必须是 PROTOCOL-001 群文本；会话→群的映射由上层在构造时确定
    /// （当前切片：一个会话对应一个群）。
    pub fn new(client: NodeClient, group_id: impl Into<String>) -> SendResult<Self> {
        Self::with_page_limit(client, group_id, DEFAULT_QUERY_PAGE_LIMIT)
    }

    /// 显式指定查询分页大小。
    pub fn with_page_limit(
        client: NodeClient,
        group_id: impl Into<String>,
        page_limit: u32,
    ) -> SendResult<Self> {
        let group_id = group_id.into();
        GroupId::parse(&group_id).map_err(|_| {
            SyncError::new(
                ErrorCode::InvalidId,
                "group id must be PROTOCOL-001 group text",
            )
        })?;
        if page_limit == 0 || page_limit > MAX_QUERY_PAGE_LIMIT {
            return Err(SyncError::new(
                ErrorCode::InvalidTarget,
                "query page limit is out of range",
            ));
        }
        let device_id = client.device_id().to_owned();
        let bridge = SubmitBridge::spawn(Arc::new(client), group_id.clone())?;
        Ok(Self {
            bridge,
            group_id,
            device_id,
            page_limit,
            metrics: SubmitterMetrics::default(),
        })
    }

    /// 绑定的群 ID。
    pub fn group_id(&self) -> &str {
        &self.group_id
    }

    /// 可克隆的计数句柄：在把 submitter 交给编排器之前取一份，就能在外部断言调用次数。
    pub fn metrics(&self) -> SubmitterMetrics {
        self.metrics.clone()
    }

    /// 已发生的节点提交次数（诊断/断言用；证明“没有重发”）。
    pub fn submit_calls(&self) -> u64 {
        self.metrics.submit_calls()
    }

    /// 已发生的按 ID 对账次数。
    pub fn query_calls(&self) -> u64 {
        self.metrics.query_calls()
    }

    /// 最近一次被节点接受的回执（诊断用；含 seq/哈希，不含正文）。
    pub fn last_receipt(&self) -> Option<AppendResult> {
        self.metrics.last_receipt()
    }

    /// 用节点当前 head 构造候选：CAS 字段只来自节点，不从本地推断。
    fn candidate_for(&self, attempt: &SubmitAttempt, head: &SignedLogHead) -> GroupCandidate {
        GroupCandidate {
            protocol_major: 1,
            protocol_minor: 0,
            submission_id: attempt.submission_id.clone(),
            group_id: self.group_id.clone(),
            author_device_id: self.device_id.clone(),
            kind: KIND_APPLICATION.to_owned(),
            expected_control_head: head.control_head.clone(),
            policy_version: head.policy_version,
            mls_epoch: head.mls_epoch,
            encrypted_payload: hex_encode(&attempt.ciphertext),
            control_metadata: None,
        }
    }
}
impl NodeSubmitter for NodeClientSubmitter {
    /// 提交一次不可变尝试：读节点 head → 追加日志 → 三态映射。
    ///
    /// 同 SubmissionId 同字节重复提交是幂等的：节点按 `(group, submission)` 去重并返回原回执，
    /// 因此本方法不会产生第二条日志（除非上游换用了相异字节，那会被映射为 `Rejected`）。
    fn submit(&self, attempt: &SubmitAttempt) -> SendResult<SubmitOutcome> {
        self.metrics.inner.submits.fetch_add(1, Ordering::SeqCst);
        SubmissionId::parse(&attempt.submission_id).map_err(|_| {
            SyncError::new(
                ErrorCode::InvalidId,
                "submission id must be PROTOCOL-001 submission text",
            )
        })?;
        if attempt.ciphertext.is_empty() {
            return Err(SyncError::new(
                ErrorCode::InvalidTarget,
                "ciphertext must not be empty",
            ));
        }
        // 先用只读 head 取节点当前公开前置状态；失败按三态映射（绝不做本地推断）。
        let head = match self.bridge.head() {
            Ok(head) => head,
            Err(BridgeError::Client(error)) => return outcome_for(&error),
            Err(BridgeError::Unavailable) => return Err(bridge_unavailable()),
        };
        if head.group_id != self.group_id {
            return Err(SyncError::new(
                ErrorCode::InvalidTarget,
                "node head is for another group",
            ));
        }
        let candidate = self.candidate_for(attempt, &head);
        match self.bridge.append(candidate) {
            Ok(receipt) => {
                // 回执必须绑定本次提交；不匹配属于协议违规，显式失败而不是当成接受。
                if receipt.group_id != self.group_id
                    || receipt.submission_id != attempt.submission_id
                {
                    return Err(SyncError::new(
                        ErrorCode::InvalidReceiptScope,
                        "node receipt does not bind this submission",
                    ));
                }
                if let Ok(mut slot) = self.metrics.inner.last_receipt.lock() {
                    *slot = Some(receipt.clone());
                }
                Ok(SubmitOutcome::Accepted {
                    detail: format!("node accepted seq={}", receipt.seq),
                })
            }
            Err(BridgeError::Client(error)) => outcome_for(&error),
            Err(BridgeError::Unavailable) => Err(bridge_unavailable()),
        }
    }

    /// 按 SubmissionId 对账：扫描节点日志，找到即 Accepted，找不到返回 Unknown。
    ///
    /// 节点只保存“已接受”的条目，因此“找不到”永远不等于“被拒绝”，只能是 Unknown。
    fn query(&self, submission_id: &str) -> SendResult<SubmitOutcome> {
        self.metrics.inner.queries.fetch_add(1, Ordering::SeqCst);
        SubmissionId::parse(submission_id).map_err(|_| {
            SyncError::new(
                ErrorCode::InvalidId,
                "submission id must be PROTOCOL-001 submission text",
            )
        })?;
        let head = self.bridge.head().map_err(BridgeError::into_sync)?;
        if head.group_id != self.group_id {
            return Err(SyncError::new(
                ErrorCode::InvalidTarget,
                "node head is for another group",
            ));
        }
        if head.seq == 0 {
            return Ok(SubmitOutcome::Unknown {
                detail: "node log is empty".to_owned(),
            });
        }
        let mut after = 0_u64;
        loop {
            let page = self
                .bridge
                .log_page(after, self.page_limit)
                .map_err(BridgeError::into_sync)?;
            if page.group_id != self.group_id {
                return Err(SyncError::new(
                    ErrorCode::InvalidTarget,
                    "node log page is for another group",
                ));
            }
            for entry in &page.entries {
                if entry.receipt.submission_id == submission_id {
                    return Ok(SubmitOutcome::Accepted {
                        detail: format!("node log has seq={}", entry.receipt.seq),
                    });
                }
            }
            if !page.has_more {
                break;
            }
            let next = page.next_after;
            if next <= after {
                // 分页没有前进：宁可显式失败，也不无限循环或误报“已找到/未找到”。
                return Err(SyncError::new(
                    ErrorCode::StateCorrupted,
                    "node log pagination did not advance",
                ));
            }
            after = next;
        }
        Ok(SubmitOutcome::Unknown {
            detail: "submission is not in the node log".to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> NodeClient {
        let endpoint = chat_node_client::NodeEndpoint::new("http://127.0.0.1:1", "node-o")
            .expect("endpoint");
        NodeClient::new(
            endpoint,
            chat_protocol::DeviceId::from_bytes([3_u8; 32]).encode(),
            ed25519_dalek::SigningKey::from_bytes(&[3_u8; 32]),
        )
        .expect("client")
    }

    #[test]
    fn conflicting_and_unauthorized_answers_are_rejected() {
        for kind in [
            NodeClientErrorKind::Conflict,
            NodeClientErrorKind::Authentication,
            NodeClientErrorKind::Permission,
            NodeClientErrorKind::NotFound,
            NodeClientErrorKind::InvalidRequest,
        ] {
            assert_eq!(
                classify(kind, None),
                Mapping::Rejected,
                "{kind:?} must never be retried"
            );
        }
        // 内容超限同样不会被接受，必须重建。
        assert_eq!(
            classify(NodeClientErrorKind::Limit, Some("PAYLOAD_TOO_LARGE")),
            Mapping::Rejected
        );
    }

    #[test]
    fn transport_rate_limit_and_5xx_answers_are_unknown() {
        for kind in [
            NodeClientErrorKind::Network,
            NodeClientErrorKind::Timeout,
            NodeClientErrorKind::Protocol,
            NodeClientErrorKind::Internal,
        ] {
            assert_eq!(
                classify(kind, None),
                Mapping::Unknown,
                "{kind:?} must stay unknown (never resend)"
            );
        }
        assert_eq!(
            classify(NodeClientErrorKind::Limit, Some("RATE_LIMITED")),
            Mapping::Unknown
        );
        assert_eq!(
            classify(NodeClientErrorKind::Limit, Some("CHALLENGE_CAPACITY_EXCEEDED")),
            Mapping::Unknown
        );
    }

    #[test]
    fn local_configuration_failures_are_errors_not_outcomes() {
        assert_eq!(
            classify(NodeClientErrorKind::Config, None),
            Mapping::Misconfigured
        );
    }

    #[test]
    fn constructor_rejects_non_protocol_ids_and_bad_limits() {
        let bad_group = NodeClientSubmitter::new(client(), "grp-1")
            .expect_err("group id must be PROTOCOL-001 text");
        assert_eq!(bad_group.code(), ErrorCode::InvalidId);
        let group = GroupId::from_bytes([5_u8; 32]).encode();
        let bad_limit = NodeClientSubmitter::with_page_limit(client(), group.clone(), 0)
            .expect_err("page limit must be positive");
        assert_eq!(bad_limit.code(), ErrorCode::InvalidTarget);
        let too_large =
            NodeClientSubmitter::with_page_limit(client(), group.clone(), MAX_QUERY_PAGE_LIMIT + 1)
                .expect_err("page limit must be bounded");
        assert_eq!(too_large.code(), ErrorCode::InvalidTarget);
        // 合法配置会真正起桥线程；Drop 时 join 掉，不留后台线程。
        let submitter = NodeClientSubmitter::with_page_limit(client(), group, 10).expect("submitter");
        assert_eq!(submitter.submit_calls(), 0);
        assert_eq!(submitter.query_calls(), 0);
        assert!(submitter.last_receipt().is_none());
    }

    #[test]
    fn debug_output_omits_keys_and_client_internals() {
        let group = GroupId::from_bytes([5_u8; 32]).encode();
        let submitter = NodeClientSubmitter::new(client(), group.clone()).expect("submitter");
        let text = format!("{submitter:?}");
        assert!(text.contains("NodeClientSubmitter"));
        assert!(text.contains(&group));
        assert!(!text.contains("signing_key"));
        assert!(!text.contains("NodeClient {"));
    }
}
