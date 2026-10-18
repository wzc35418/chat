//! 传输层：P0 loopback iroh 实现 + 本轮新增的纯逻辑核心。
//!
//! - [`network`]：P0 仅 loopback 的真实 iroh/QUIC 传输（公开 relay/发现关闭）；
//! - [`session`]：TRANSPORT-001 的纯逻辑子集（会话/端点/协商结果/初始状态与预算）；
//! - [`state`]：TRANSPORT-002 的连接状态机（冻结迁移表、非法迁移拒绝、重复事件幂等）；
//! - [`frame`]：TRANSPORT-003 的帧编解码与严格有序分片重组；
//! - [`retry`]：TRANSPORT-004 的指数退避 + 有界 jitter + 预算耗尽 Stop；
//! - [`error`]：传输层稳定错误码（与 PROTOCOL-014 的关系见模块文档）。
//!
//! 本轮不引入新的网络依赖；真正的直连/穿透/中继与节点客户端（TRANSPORT-005~012）
//! 仍未实现。
#![forbid(unsafe_code)]
pub mod network;

pub mod error;
pub mod frame;
pub mod retry;
pub mod session;
pub mod state;

pub use error::{CODE_TABLE, Result, TransportError};
pub use frame::{
    FRAGMENT_HEADER_BYTES, FRAME_HARD_MAX_BYTES, Fragment, FragmentAssembler, FrameCodec,
    FrameLimits, LENGTH_PREFIX_BYTES, MAX_FRAGMENTS_PER_FRAME_HARD,
};
pub use retry::{
    RETRY_HARD_MAX_ATTEMPTS, RETRY_HARD_MAX_DELAY_MS, RetryOutcome, RetryPolicy, RetryStopReason,
    RetryTracker,
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
