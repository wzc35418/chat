//! 传输层：P0 loopback iroh 实现 + 纯逻辑核心 + 可注入 wire 收发管线。
//!
//! - [`network`]：P0 仅 loopback 的真实 iroh/QUIC 传输（公开 relay/发现关闭）；
//! - [`session`]：TRANSPORT-001 的纯逻辑子集（会话/端点/协商结果/初始状态与预算）；
//! - [`state`]：TRANSPORT-002 的连接状态机（冻结迁移表、非法迁移拒绝、重复事件幂等）；
//! - [`frame`]：TRANSPORT-003 的帧编解码与严格有序分片重组；
//! - [`retry`]：TRANSPORT-004 的指数退避 + 有界 jitter + 预算耗尽 Stop；
//! - [`wire`]：可注入 `WireSink`/`WireSource` 端口（无真实 IO）；
//! - [`endpoint`]：TRANSPORT-002 真实 iroh 端点启动/连接/接受（仅回环，显式超时）；
//! - [`wire_io`]：TRANSPORT-006 把帧记录读写适配到 iroh 双向流（异步 API + 同步端口桥接）；
//! - [`send`]：TRANSPORT-005 负载→分片→逐片写出，带预算与可重试/致命判定；
//! - [`receive`]：TRANSPORT-006 逐片喂入重组器，带会话/端点绑定校验；
//! - [`pull_map`]：TRANSPORT-007 把 sync `Pull { from, to }` 映射成帧请求序列与响应校验；
//! - [`error`]：传输层稳定错误码（与 PROTOCOL-014 的关系见模块文档）。
//!
//! 不引入新的网络依赖、不 sleep、无 `unsafe`；真实直连/穿透/中继与节点客户端
//! （TRANSPORT-002、008 起的节点调用）仍未实现。
#![forbid(unsafe_code)]
pub mod network;

pub mod endpoint;
pub mod error;
pub mod frame;
pub mod pull_map;
pub mod receive;
pub mod retry;
pub mod send;
pub mod session;
pub mod state;
pub mod wire;
pub mod wire_io;

pub use endpoint::{
    AcceptLoopHandle, DEFAULT_IO_TIMEOUT_MS, EndpointConfig, MAX_ACCEPT_QUEUE, MAX_IO_TIMEOUT_MS,
    PeerConnection, TRANSPORT_ALPN, TransportEndpoint, start_endpoint, validate_timeout,
};
pub use error::{CODE_TABLE, Result, TransportError};
pub use frame::{
    FRAGMENT_HEADER_BYTES, FRAME_HARD_MAX_BYTES, Fragment, FragmentAssembler, FrameCodec,
    FrameLimits, LENGTH_PREFIX_BYTES, MAX_FRAGMENTS_PER_FRAME_HARD,
};
pub use pull_map::{
    AcceptedPage, DEFAULT_PULL_PAGE_ITEMS, MAX_PULL_PAGE_ITEMS, MAX_REQUESTS_PER_PLAN,
    PULL_PLAN_DOMAIN, PageRequest, PageResponse, PullRange, PullRequestPlan, ResponseValidator,
    plan_pull_requests,
};
pub use receive::{
    PeerFrameReceiver, RECEIVE_HARD_MAX_RECORDS, ReceiveBudget, ReceiveOutcome, receive_peer_frame,
};
pub use retry::{
    RETRY_HARD_MAX_ATTEMPTS, RETRY_HARD_MAX_DELAY_MS, RetryOutcome, RetryPolicy, RetryStopReason,
    RetryTracker,
};
pub use send::{
    DEFAULT_FRAGMENT_BYTES, FatalReason, SEND_HARD_MAX_QUEUE, SEND_HARD_MAX_RECORDS, SendBudget,
    SendOutcome, send_peer_frame,
};
pub use session::{
    ENDPOINT_ID_BYTES, EndpointId, SESSION_HARD_MAX_HANDSHAKE_MS, SESSION_HARD_MAX_QUEUE,
    SESSION_HARD_MAX_RECONNECT_ATTEMPTS, SessionConfig, SessionLimits, TransportSession,
    open_session,
};
pub use state::{
    ConnectionEvent, ConnectionState, ConnectionStateMachine, TRANSITIONS, TransitionTableCheck,
    next_state,
};
pub use wire::{WireFailure, WireSink, WireSource};
pub use wire_io::{
    IrohWireSink, IrohWireSource, MAX_IO_QUEUE, MAX_RECORD_BYTES, SendFrameRequest, WireStream,
    accept_stream, open_stream,
};
