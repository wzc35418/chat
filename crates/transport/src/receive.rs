//! TRANSPORT-006 的纯逻辑子集：从注入的 [`WireSource`] 逐片喂入重组器并输出完整帧。
//!
//! 绑定与拒绝：
//!
//! - 会话必须可用（`Connected`/`Degraded`），否则 `SESSION_NOT_USABLE`；
//! - 接收器在创建时绑定会话（本机/对端端点与设备），复用旧接收器读另一个会话
//!   `SESSION_MISMATCH`；
//! - `WireSource::peer_endpoint()` 必须等于会话对端端点；返回 `None`（无法认证来源）
//!   同样拒绝，绝不把未认证连接当作会话；
//! - 分片错误沿用 [`crate::frame`] 的码：乱序 `OUT_OF_ORDER_FRAGMENT`、重复
//!   `DUPLICATE_FRAGMENT`、旧帧 `STALE_FRAME`、未收齐换帧 `INCOMPLETE_FRAME`、
//!   超限 `CONTENT_TOO_LARGE`、结构非法 `MALFORMED_FRAME`。
//!
//! 每次调用最多消费 `max_records_per_call` 条记录，未收齐时返回
//! `Pending { budget_exhausted }`，由调用方继续驱动。

use crate::error::{Result, TransportError};
use crate::frame::{FRAGMENT_HEADER_BYTES, FragmentAssembler, FrameCodec, FrameLimits};
use crate::session::{EndpointId, TransportSession};
use crate::wire::WireSource;
use chat_protocol::DeviceId;

/// 单次接收记录的硬上限。
pub const RECEIVE_HARD_MAX_RECORDS: usize = 1024;

/// 接收预算。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiveBudget {
    /// 单次调用最多消费的记录数。
    pub max_records_per_call: usize,
}

impl ReceiveBudget {
    /// 默认每次最多消费 64 条记录。
    pub const DEFAULT: ReceiveBudget = ReceiveBudget {
        max_records_per_call: 64,
    };

    /// 有界校验。
    pub fn validate(&self) -> Result<()> {
        if self.max_records_per_call == 0 || self.max_records_per_call > RECEIVE_HARD_MAX_RECORDS {
            return Err(TransportError::InvalidConfig {
                detail: "receive maxRecordsPerCall must be within 1..=1024",
            });
        }
        Ok(())
    }
}

impl Default for ReceiveBudget {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SessionBinding {
    local_endpoint: EndpointId,
    local_device: DeviceId,
    peer_endpoint: EndpointId,
    peer_device: DeviceId,
}

impl SessionBinding {
    fn of(session: &TransportSession) -> Self {
        Self {
            local_endpoint: session.local_endpoint(),
            local_device: session.local_device(),
            peer_endpoint: session.peer_endpoint(),
            peer_device: session.peer_device(),
        }
    }
}

/// 通过校验的完整帧（wire 适配器与纯逻辑管线共用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceivedFrame {
    /// 帧编号。
    pub frame_id: u32,
    /// 完整帧字节。
    pub payload: Vec<u8>,
}

/// 绑定到单个会话的接收器。
#[derive(Clone, Debug)]
pub struct PeerFrameReceiver {
    binding: SessionBinding,
    codec: FrameCodec,
    assembler: FragmentAssembler,
}

impl PeerFrameReceiver {
    /// 创建接收器；`limits` 必须与发送方协商的分片参数一致，且不得大于会话帧上限。
    pub fn new(session: &TransportSession, limits: FrameLimits) -> Result<Self> {
        limits.validate()?;
        if limits.max_frame_bytes > session.limits().max_frame_bytes {
            return Err(TransportError::InvalidConfig {
                detail: "receiver frame limit exceeds the session frame limit",
            });
        }
        Ok(Self {
            binding: SessionBinding::of(session),
            codec: FrameCodec::new(limits)?,
            assembler: FragmentAssembler::new(limits)?,
        })
    }

    /// 帧预算。
    pub const fn frame_limits(&self) -> FrameLimits {
        self.codec.limits()
    }

    /// 最近一次完整帧编号。
    pub const fn last_completed_frame(&self) -> Option<u32> {
        self.assembler.last_completed_frame()
    }

    /// 是否存在未收齐的帧。
    pub const fn has_in_flight(&self) -> bool {
        self.assembler.has_in_flight()
    }

    /// 丢弃在途状态（重连后必须调用）。
    pub fn reset(&mut self) {
        self.assembler.reset();
    }

    /// 单条记录字节上限（分片头 + 最大分片负载）。
    pub fn max_record_bytes(&self) -> usize {
        self.codec
            .limits()
            .max_fragment_bytes
            .saturating_add(FRAGMENT_HEADER_BYTES)
    }

    /// 校验会话可用且接收器绑定同一会话（wire 适配器复用同一规则）。
    pub fn validate_binding(&self, session: &TransportSession) -> Result<()> {
        if !session.is_usable() {
            return Err(TransportError::SessionNotUsable);
        }
        if self.binding != SessionBinding::of(session) {
            return Err(TransportError::SessionMismatch {
                detail: "receiver is bound to another session",
            });
        }
        Ok(())
    }

    /// 推入一条线格式记录；收齐时返回完整帧。
    ///
    /// 与 [`receive_peer_frame`] 走同一有界校验与严格有序重组路径，真实 wire 适配器
    /// （iroh 连接）直接复用本方法。
    pub fn push_record(&mut self, record: &[u8]) -> Result<Option<ReceivedFrame>> {
        let max_record_bytes = self.max_record_bytes();
        if record.len() > max_record_bytes {
            return Err(TransportError::ContentTooLarge {
                size: record.len(),
                limit: max_record_bytes,
            });
        }
        let fragment = self.codec.decode_wire(record)?;
        let fragment_frame = fragment.frame_id();
        match self.assembler.push(fragment)? {
            Some(payload) => {
                let frame_id = self
                    .assembler
                    .last_completed_frame()
                    .unwrap_or(fragment_frame);
                Ok(Some(ReceivedFrame { frame_id, payload }))
            }
            None => Ok(None),
        }
    }
}

/// 一次接收调用的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiveOutcome {
    /// 收到一个完整帧。
    Frame {
        /// 帧编号。
        frame_id: u32,
        /// 完整帧字节。
        payload: Vec<u8>,
        /// 本次调用消费的记录数。
        records: usize,
    },
    /// 还没有完整帧（部分分片或暂时无数据）。
    Pending {
        /// 本次调用消费的记录数。
        records: usize,
        /// 是否因为单次记录预算而停止（数据可能还有）。
        budget_exhausted: bool,
    },
    /// 来源暂时不可用（可重试）。
    Unavailable {
        /// 本次调用消费的记录数。
        records: usize,
    },
    /// 来源已关闭（致命）。
    Closed {
        /// 本次调用消费的记录数。
        records: usize,
    },
}

impl ReceiveOutcome {
    /// 是否收到完整帧。
    pub const fn is_frame(&self) -> bool {
        matches!(self, ReceiveOutcome::Frame { .. })
    }

    /// 本次调用消费的记录数。
    pub const fn records(&self) -> usize {
        match self {
            ReceiveOutcome::Frame { records, .. }
            | ReceiveOutcome::Pending { records, .. }
            | ReceiveOutcome::Unavailable { records }
            | ReceiveOutcome::Closed { records } => *records,
        }
    }
}

/// TRANSPORT-006：驱动一次有界接收。
pub fn receive_peer_frame<S: WireSource + ?Sized>(
    session: &TransportSession,
    receiver: &mut PeerFrameReceiver,
    source: &mut S,
    budget: ReceiveBudget,
) -> Result<ReceiveOutcome> {
    receiver.validate_binding(session)?;
    budget.validate()?;
    match source.peer_endpoint() {
        Some(endpoint) if endpoint == session.peer_endpoint() => {}
        _ => {
            return Err(TransportError::SessionMismatch {
                detail: "source peer endpoint does not match the session",
            });
        }
    }

    let mut records = 0usize;
    loop {
        if records >= budget.max_records_per_call {
            return Ok(ReceiveOutcome::Pending {
                records,
                budget_exhausted: true,
            });
        }
        match source.next_record() {
            Ok(None) => {
                return Ok(ReceiveOutcome::Pending {
                    records,
                    budget_exhausted: false,
                });
            }
            Err(failure) if failure.is_fatal() => return Ok(ReceiveOutcome::Closed { records }),
            Err(_) => return Ok(ReceiveOutcome::Unavailable { records }),
            Ok(Some(record)) => {
                records += 1;
                if let Some(frame) = receiver.push_record(&record)? {
                    return Ok(ReceiveOutcome::Frame {
                        frame_id: frame.frame_id,
                        payload: frame.payload,
                        records,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::FrameLimits;
    use crate::retry::{RetryPolicy, RetryTracker};
    use crate::send::{SendBudget, send_peer_frame};
    use crate::state::ConnectionEvent;
    use crate::wire::WireSource;
    use crate::wire::{WireFailure, WireSink};
    use chat_protocol::{
        Capabilities, CapabilityLimits, DeviceId, ProtocolVersion, negotiate_capabilities,
    };

    fn endpoint(seed: u8) -> EndpointId {
        EndpointId::from_bytes([seed; 32])
    }

    fn device(seed: u8) -> DeviceId {
        DeviceId::from_bytes([seed; 32])
    }

    fn connected_session(local: u8, remote: u8, limits: FrameLimits) -> TransportSession {
        let caps = Capabilities::new(
            vec![ProtocolVersion::V1],
            vec!["e2ee.v1".to_owned(), "mls.v1".to_owned()],
            CapabilityLimits::new(
                limits.max_frame_bytes,
                limits.max_frame_bytes,
                limits.max_frame_bytes,
            ),
        );
        let negotiated = negotiate_capabilities(caps.clone(), caps).unwrap();
        let mut session = TransportSession::open(crate::session::SessionConfig {
            local_endpoint: endpoint(local),
            local_device: device(0xa0 + local),
            peer_endpoint: endpoint(remote),
            peer_device: device(0xb0 + remote),
            negotiated: Some(negotiated),
            now_unix_ms: 0,
            limits: crate::session::SessionLimits {
                max_frame_bytes: limits.max_frame_bytes,
                ..crate::session::SessionLimits::DEFAULT
            },
        })
        .unwrap();
        session.apply(ConnectionEvent::Connect).unwrap();
        session.apply(ConnectionEvent::Connected).unwrap();
        session
    }

    struct MemorySink {
        records: Vec<Vec<u8>>,
    }

    impl WireSink for MemorySink {
        fn write_record(&mut self, record: &[u8]) -> core::result::Result<(), WireFailure> {
            self.records.push(record.to_vec());
            Ok(())
        }
    }

    struct MemorySource {
        records: std::collections::VecDeque<Vec<u8>>,
        peer: Option<EndpointId>,
        failure: Option<WireFailure>,
    }

    impl MemorySource {
        fn new(records: Vec<Vec<u8>>, peer: EndpointId) -> Self {
            Self {
                records: records.into(),
                peer: Some(peer),
                failure: None,
            }
        }
    }

    impl WireSource for MemorySource {
        fn next_record(&mut self) -> core::result::Result<Option<Vec<u8>>, WireFailure> {
            if let Some(failure) = self.failure.take() {
                return Err(failure);
            }
            Ok(self.records.pop_front())
        }

        fn peer_endpoint(&self) -> Option<EndpointId> {
            self.peer
        }
    }

    fn send_all(session: &TransportSession, frame_id: u32, payload: &[u8]) -> Vec<Vec<u8>> {
        let mut sink = MemorySink {
            records: Vec::new(),
        };
        let mut retry = RetryTracker::new(RetryPolicy {
            jitter_max_ms: 0,
            ..RetryPolicy::DEFAULT
        })
        .unwrap();
        let outcome = send_peer_frame(
            session,
            frame_id,
            payload,
            &mut sink,
            SendBudget {
                max_fragment_bytes: 1024,
                ..SendBudget::DEFAULT
            },
            &mut retry,
            0,
        )
        .unwrap();
        assert!(outcome.is_sent(), "send must succeed: {outcome:?}");
        sink.records
    }

    fn limits() -> FrameLimits {
        FrameLimits {
            max_frame_bytes: 8 * 1024,
            max_fragment_bytes: 1024,
            max_fragments_per_frame: 16,
        }
    }

    #[test]
    fn reassembles_frames_from_sent_records() {
        let session = connected_session(1, 2, limits());
        let payload: Vec<u8> = (0..5_000).map(|index| (index % 251) as u8).collect();
        let records = send_all(&session, 1, &payload);
        assert_eq!(records.len(), 5);
        let mut source = MemorySource::new(records, endpoint(2));
        let mut receiver = PeerFrameReceiver::new(&session, limits()).unwrap();
        let outcome =
            receive_peer_frame(&session, &mut receiver, &mut source, ReceiveBudget::DEFAULT)
                .unwrap();
        assert_eq!(
            outcome,
            ReceiveOutcome::Frame {
                frame_id: 1,
                payload: payload.clone(),
                records: 5
            }
        );
        assert_eq!(receiver.last_completed_frame(), Some(1));
        assert!(!receiver.has_in_flight());
    }

    #[test]
    fn partial_data_returns_pending_and_budget_flag() {
        let session = connected_session(1, 2, limits());
        let payload = vec![7u8; 3_000];
        let records = send_all(&session, 1, &payload);
        let mut source = MemorySource::new(records, endpoint(2));
        let mut receiver = PeerFrameReceiver::new(&session, limits()).unwrap();
        // 每次只允许 1 条记录：前两次返回 budget_exhausted。
        let budget = ReceiveBudget {
            max_records_per_call: 1,
        };
        let first = receive_peer_frame(&session, &mut receiver, &mut source, budget).unwrap();
        assert_eq!(
            first,
            ReceiveOutcome::Pending {
                records: 1,
                budget_exhausted: true
            }
        );
        assert!(receiver.has_in_flight());
        let second = receive_peer_frame(&session, &mut receiver, &mut source, budget).unwrap();
        assert_eq!(second.records(), 1);
        // 换用默认预算收完剩余分片。
        let mut outcome = second;
        while !outcome.is_frame() {
            outcome =
                receive_peer_frame(&session, &mut receiver, &mut source, ReceiveBudget::DEFAULT)
                    .unwrap();
        }
        assert_eq!(
            outcome,
            ReceiveOutcome::Frame {
                frame_id: 1,
                payload,
                records: 1
            }
        );
        // 无数据。
        assert_eq!(
            receive_peer_frame(&session, &mut receiver, &mut source, ReceiveBudget::DEFAULT)
                .unwrap(),
            ReceiveOutcome::Pending {
                records: 0,
                budget_exhausted: false
            }
        );
    }

    #[test]
    fn rejects_out_of_order_duplicate_and_stale_records() {
        let session = connected_session(1, 2, limits());
        let payload = vec![9u8; 2_500];
        let records = send_all(&session, 1, &payload);
        assert_eq!(records.len(), 3);
        // 乱序：先给第 2 片。
        let mut source = MemorySource::new(vec![records[1].clone()], endpoint(2));
        let mut receiver = PeerFrameReceiver::new(&session, limits()).unwrap();
        assert_eq!(
            receive_peer_frame(&session, &mut receiver, &mut source, ReceiveBudget::DEFAULT)
                .unwrap_err()
                .code(),
            "OUT_OF_ORDER_FRAGMENT"
        );
        // 重复：第 1 片两次。
        let mut source = MemorySource::new(
            vec![
                records[0].clone(),
                records[0].clone(),
                records[1].clone(),
                records[2].clone(),
            ],
            endpoint(2),
        );
        let mut receiver = PeerFrameReceiver::new(&session, limits()).unwrap();
        assert_eq!(
            receive_peer_frame(&session, &mut receiver, &mut source, ReceiveBudget::DEFAULT)
                .unwrap_err()
                .code(),
            "DUPLICATE_FRAGMENT"
        );
        // 旧帧迟到：先完成帧 1，再收到帧 1 的旧分片必须拒绝。
        let mut source = MemorySource::new(
            vec![
                records[0].clone(),
                records[1].clone(),
                records[2].clone(),
                records[0].clone(),
            ],
            endpoint(2),
        );
        let mut receiver = PeerFrameReceiver::new(&session, limits()).unwrap();
        let first =
            receive_peer_frame(&session, &mut receiver, &mut source, ReceiveBudget::DEFAULT)
                .unwrap();
        assert!(first.is_frame());
        assert_eq!(
            receive_peer_frame(&session, &mut receiver, &mut source, ReceiveBudget::DEFAULT)
                .unwrap_err()
                .code(),
            "STALE_FRAME"
        );
    }

    #[test]
    fn rejects_cross_session_and_unauthenticated_sources() {
        let session = connected_session(1, 2, limits());
        let mut receiver = PeerFrameReceiver::new(&session, limits()).unwrap();
        // 端点不匹配。
        let mut source = MemorySource::new(Vec::new(), endpoint(9));
        assert_eq!(
            receive_peer_frame(&session, &mut receiver, &mut source, ReceiveBudget::DEFAULT)
                .unwrap_err()
                .code(),
            "SESSION_MISMATCH"
        );
        // 未认证来源（None）同样拒绝。
        let mut source = MemorySource {
            records: std::collections::VecDeque::new(),
            peer: None,
            failure: None,
        };
        assert_eq!(
            receive_peer_frame(&session, &mut receiver, &mut source, ReceiveBudget::DEFAULT)
                .unwrap_err()
                .code(),
            "SESSION_MISMATCH"
        );
        // 接收器绑定其他会话。
        let other = connected_session(3, 4, limits());
        let mut source = MemorySource::new(Vec::new(), endpoint(4));
        assert_eq!(
            receive_peer_frame(&other, &mut receiver, &mut source, ReceiveBudget::DEFAULT)
                .unwrap_err()
                .code(),
            "SESSION_MISMATCH"
        );
        // 未连接会话。
        let mut disconnected = connected_session(1, 2, limits());
        disconnected.apply(ConnectionEvent::Disconnect).unwrap();
        assert_eq!(
            receive_peer_frame(
                &disconnected,
                &mut receiver,
                &mut source,
                ReceiveBudget::DEFAULT
            )
            .unwrap_err()
            .code(),
            "SESSION_NOT_USABLE"
        );
    }

    #[test]
    fn source_failures_map_to_unavailable_or_closed() {
        let session = connected_session(1, 2, limits());
        let mut receiver = PeerFrameReceiver::new(&session, limits()).unwrap();
        let mut source = MemorySource {
            records: std::collections::VecDeque::new(),
            peer: Some(endpoint(2)),
            failure: Some(WireFailure::Unavailable),
        };
        assert_eq!(
            receive_peer_frame(&session, &mut receiver, &mut source, ReceiveBudget::DEFAULT)
                .unwrap(),
            ReceiveOutcome::Unavailable { records: 0 }
        );
        let mut source = MemorySource {
            records: std::collections::VecDeque::new(),
            peer: Some(endpoint(2)),
            failure: Some(WireFailure::Closed),
        };
        assert_eq!(
            receive_peer_frame(&session, &mut receiver, &mut source, ReceiveBudget::DEFAULT)
                .unwrap(),
            ReceiveOutcome::Closed { records: 0 }
        );
    }

    #[test]
    fn oversize_records_are_rejected_before_parsing() {
        let session = connected_session(1, 2, limits());
        let mut receiver = PeerFrameReceiver::new(&session, limits()).unwrap();
        let mut source = MemorySource::new(vec![vec![0u8; 2 * 1024]], endpoint(2));
        assert_eq!(
            receive_peer_frame(&session, &mut receiver, &mut source, ReceiveBudget::DEFAULT)
                .unwrap_err()
                .code(),
            "CONTENT_TOO_LARGE"
        );
        // 非法接收预算。
        let budget = ReceiveBudget {
            max_records_per_call: 0,
        };
        assert_eq!(
            receive_peer_frame(&session, &mut receiver, &mut source, budget)
                .unwrap_err()
                .code(),
            "INVALID_TRANSPORT_CONFIG"
        );
        // 接收器帧上限大于会话上限。
        let too_large = FrameLimits {
            max_frame_bytes: 16 * 1024,
            ..limits()
        };
        assert_eq!(
            PeerFrameReceiver::new(&session, too_large)
                .unwrap_err()
                .code(),
            "INVALID_TRANSPORT_CONFIG"
        );
        // 接收后重置。
        receiver.reset();
        assert!(!receiver.has_in_flight());
        assert_eq!(receiver.last_completed_frame(), None);
        let _ = RetryPolicy::DEFAULT;
    }
}
