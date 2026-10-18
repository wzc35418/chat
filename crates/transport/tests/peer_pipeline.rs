//! TRANSPORT-005~007 可注入 wire 管线的端到端验收。
//!
//! 覆盖：分片-重组往返、丢片/乱序/重复注入、发送预算耗尽、wire 失败到可重试/致命、
//! 跨会话与未认证来源拒绝、Pull 计划映射与响应校验（含分页边界）。全程无网络与 sleep。

use std::collections::VecDeque;

use chat_protocol::{
    Capabilities, CapabilityLimits, DeviceId, ProtocolVersion, negotiate_capabilities,
};
use chat_transport::{
    ConnectionEvent, EndpointId, FrameLimits, MAX_PULL_PAGE_ITEMS, MAX_REQUESTS_PER_PLAN,
    PageResponse, PeerFrameReceiver, PullRange, ReceiveBudget, ReceiveOutcome, ResponseValidator,
    RetryPolicy, RetryTracker, SendBudget, SendOutcome, SessionConfig, SessionLimits,
    TransportSession, WireFailure, WireSink, WireSource, plan_pull_requests, receive_peer_frame,
    send_peer_frame,
};

fn endpoint(seed: u8) -> EndpointId {
    EndpointId::from_bytes([seed; 32])
}

fn device(seed: u8) -> DeviceId {
    DeviceId::from_bytes([seed; 32])
}

fn connected_session(local: u8, remote: u8) -> TransportSession {
    let caps = Capabilities::new(
        vec![ProtocolVersion::V1],
        vec!["e2ee.v1".to_owned(), "mls.v1".to_owned()],
        CapabilityLimits::new(8 * 1024, 8 * 1024, 8 * 1024),
    );
    let negotiated = negotiate_capabilities(caps.clone(), caps).unwrap();
    let mut session = TransportSession::open(SessionConfig {
        local_endpoint: endpoint(local),
        local_device: device(0xa0 + local),
        peer_endpoint: endpoint(remote),
        peer_device: device(0xb0 + remote),
        negotiated: Some(negotiated),
        now_unix_ms: 0,
        limits: SessionLimits {
            max_frame_bytes: 8 * 1024,
            ..SessionLimits::DEFAULT
        },
    })
    .unwrap();
    session.apply(ConnectionEvent::Connect).unwrap();
    session.apply(ConnectionEvent::Connected).unwrap();
    session
}

fn frame_limits() -> FrameLimits {
    FrameLimits {
        max_frame_bytes: 8 * 1024,
        max_fragment_bytes: 1024,
        max_fragments_per_frame: 8,
    }
}

fn fragment_budget() -> SendBudget {
    SendBudget {
        max_fragment_bytes: 1024,
        ..SendBudget::DEFAULT
    }
}

fn retry_tracker(max_attempts: u32) -> RetryTracker {
    RetryTracker::new(RetryPolicy {
        base_delay_ms: 100,
        max_delay_ms: 1_000,
        max_attempts,
        jitter_max_ms: 0,
        jitter_seed: 0,
    })
    .unwrap()
}

struct MemorySink {
    records: Vec<Vec<u8>>,
    queued: usize,
    fail_at: Option<(usize, WireFailure)>,
}

impl MemorySink {
    fn new() -> Self {
        Self {
            records: Vec::new(),
            queued: 0,
            fail_at: None,
        }
    }
}

impl WireSink for MemorySink {
    fn write_record(&mut self, record: &[u8]) -> core::result::Result<(), WireFailure> {
        if let Some((at, failure)) = self.fail_at
            && self.records.len() == at
        {
            return Err(failure);
        }
        self.records.push(record.to_vec());
        Ok(())
    }

    fn queued_records(&self) -> usize {
        self.queued
    }
}

struct MemorySource {
    records: VecDeque<Vec<u8>>,
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

#[test]
fn send_receive_round_trip_across_fragments() {
    let session = connected_session(1, 2);
    let mut sink = MemorySink::new();
    let mut retry = retry_tracker(3);
    // 7.7 KiB：8 KiB 会话上限内的 8 个分片。
    let payload: Vec<u8> = (0..7_700).map(|index| (index % 251) as u8).collect();
    let outcome = send_peer_frame(
        &session,
        1,
        &payload,
        &mut sink,
        fragment_budget(),
        &mut retry,
        1_000,
    )
    .unwrap();
    assert_eq!(
        outcome,
        SendOutcome::Sent {
            frame_id: 1,
            fragments: 8,
            records: 8
        }
    );
    let mut source = MemorySource::new(sink.records, endpoint(2));
    let mut receiver = PeerFrameReceiver::new(&session, frame_limits()).unwrap();
    let outcome =
        receive_peer_frame(&session, &mut receiver, &mut source, ReceiveBudget::DEFAULT).unwrap();
    assert_eq!(
        outcome,
        ReceiveOutcome::Frame {
            frame_id: 1,
            payload,
            records: 8
        }
    );
    assert_eq!(receiver.last_completed_frame(), Some(1));
    // 连续第二帧。
    let mut sink = MemorySink::new();
    assert!(
        send_peer_frame(
            &session,
            2,
            b"next",
            &mut sink,
            fragment_budget(),
            &mut retry,
            1_000
        )
        .unwrap()
        .is_sent()
    );
    let mut source = MemorySource::new(sink.records, endpoint(2));
    let outcome =
        receive_peer_frame(&session, &mut receiver, &mut source, ReceiveBudget::DEFAULT).unwrap();
    assert_eq!(
        outcome,
        ReceiveOutcome::Frame {
            frame_id: 2,
            payload: b"next".to_vec(),
            records: 1
        }
    );
}

#[test]
fn injected_loss_duplication_and_reordering_are_rejected() {
    let session = connected_session(1, 2);
    let payload = vec![5u8; 2_500];
    let mut sink = MemorySink::new();
    let mut retry = retry_tracker(3);
    send_peer_frame(
        &session,
        1,
        &payload,
        &mut sink,
        fragment_budget(),
        &mut retry,
        0,
    )
    .unwrap();
    assert_eq!(sink.records.len(), 3);
    let records = sink.records;
    // 丢片：只剩 0 和 2 → 收到 2 时判乱序。
    let mut source = MemorySource::new(vec![records[0].clone(), records[2].clone()], endpoint(2));
    let mut receiver = PeerFrameReceiver::new(&session, frame_limits()).unwrap();
    assert_eq!(
        receive_peer_frame(&session, &mut receiver, &mut source, ReceiveBudget::DEFAULT)
            .unwrap_err()
            .code(),
        "OUT_OF_ORDER_FRAGMENT"
    );
    // 重复注入。
    let mut source = MemorySource::new(vec![records[0].clone(), records[0].clone()], endpoint(2));
    let mut receiver = PeerFrameReceiver::new(&session, frame_limits()).unwrap();
    assert_eq!(
        receive_peer_frame(&session, &mut receiver, &mut source, ReceiveBudget::DEFAULT)
            .unwrap_err()
            .code(),
        "DUPLICATE_FRAGMENT"
    );
    // 尾片丢失后直接换下一帧 → INCOMPLETE_FRAME。
    let mut next_sink = MemorySink::new();
    send_peer_frame(
        &session,
        2,
        b"other",
        &mut next_sink,
        fragment_budget(),
        &mut retry,
        0,
    )
    .unwrap();
    let mut cross = MemorySource::new(vec![records[0].clone()], endpoint(2));
    cross.records.extend(next_sink.records.iter().cloned());
    let mut receiver = PeerFrameReceiver::new(&session, frame_limits()).unwrap();
    assert_eq!(
        receive_peer_frame(&session, &mut receiver, &mut cross, ReceiveBudget::DEFAULT)
            .unwrap_err()
            .code(),
        "INCOMPLETE_FRAME"
    );
}

#[test]
fn send_budgets_and_retry_budget_are_explicit() {
    let session = connected_session(1, 2);
    let mut retry = retry_tracker(2);
    // 队列深度不足。
    let mut sink = MemorySink::new();
    sink.queued = 6;
    let budget = SendBudget {
        max_queue_depth: 7,
        ..fragment_budget()
    };
    let payload = vec![1u8; 3 * 1024];
    assert_eq!(
        send_peer_frame(&session, 1, &payload, &mut sink, budget, &mut retry, 0)
            .unwrap_err()
            .code(),
        "SEND_BUDGET_EXCEEDED"
    );
    // 记录数预算不足。
    let budget = SendBudget {
        max_records_per_send: 2,
        ..fragment_budget()
    };
    assert_eq!(
        send_peer_frame(&session, 1, b"x", &mut sink, budget, &mut retry, 0)
            .unwrap_err()
            .code(),
        "SEND_BUDGET_EXCEEDED"
    );
    // 可重试失败 → 退避；预算耗尽 → 致命。
    let mut sink = MemorySink::new();
    sink.fail_at = Some((0, WireFailure::Unavailable));
    let first = send_peer_frame(
        &session,
        2,
        &payload,
        &mut sink,
        fragment_budget(),
        &mut retry,
        500,
    )
    .unwrap();
    match first {
        SendOutcome::RetryableFailure {
            delay_ms,
            next_attempt_at_ms,
            ..
        } => {
            assert_eq!(delay_ms, 100);
            assert_eq!(next_attempt_at_ms, 600);
        }
        other => panic!("expected retryable, got {other:?}"),
    }
    let second = send_peer_frame(
        &session,
        2,
        &payload,
        &mut sink,
        fragment_budget(),
        &mut retry,
        600,
    )
    .unwrap();
    assert!(second.is_retryable());
    let third = send_peer_frame(
        &session,
        2,
        &payload,
        &mut sink,
        fragment_budget(),
        &mut retry,
        800,
    )
    .unwrap();
    assert!(third.is_fatal());
    // 连接关闭 → 致命，不消耗预算。
    let mut sink = MemorySink::new();
    sink.fail_at = Some((0, WireFailure::Closed));
    let mut fresh = retry_tracker(3);
    assert!(
        send_peer_frame(
            &session,
            3,
            &payload,
            &mut sink,
            fragment_budget(),
            &mut fresh,
            0
        )
        .unwrap()
        .is_fatal()
    );
    assert_eq!(fresh.attempts(), 0);
}

#[test]
fn cross_session_and_unauthenticated_sources_are_rejected() {
    let session = connected_session(1, 2);
    let other = connected_session(3, 4);
    let mut receiver = PeerFrameReceiver::new(&session, frame_limits()).unwrap();
    // 来源声称是另一个对端。
    let mut source = MemorySource::new(Vec::new(), endpoint(4));
    assert_eq!(
        receive_peer_frame(&session, &mut receiver, &mut source, ReceiveBudget::DEFAULT)
            .unwrap_err()
            .code(),
        "SESSION_MISMATCH"
    );
    // 未认证来源。
    let mut source = MemorySource {
        records: VecDeque::new(),
        peer: None,
        failure: None,
    };
    assert_eq!(
        receive_peer_frame(&session, &mut receiver, &mut source, ReceiveBudget::DEFAULT)
            .unwrap_err()
            .code(),
        "SESSION_MISMATCH"
    );
    // 接收器绑定 A，却用于 B 会话。
    let mut source = MemorySource::new(Vec::new(), endpoint(4));
    assert_eq!(
        receive_peer_frame(&other, &mut receiver, &mut source, ReceiveBudget::DEFAULT)
            .unwrap_err()
            .code(),
        "SESSION_MISMATCH"
    );
    // 未连接会话不能收发。
    let mut disconnected = connected_session(1, 2);
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
    assert_eq!(
        send_peer_frame(
            &disconnected,
            1,
            b"x",
            &mut MemorySink::new(),
            fragment_budget(),
            &mut retry_tracker(2),
            0
        )
        .unwrap_err()
        .code(),
        "SESSION_NOT_USABLE"
    );
}

#[test]
fn pull_plan_maps_paginated_ranges_to_frame_requests() {
    let range = PullRange::new(1, 250).unwrap();
    let plan = plan_pull_requests(range, 100, 7, MAX_REQUESTS_PER_PLAN).unwrap();
    assert_eq!(plan.total_items(), 250);
    assert_eq!(
        plan.requests()
            .iter()
            .map(|request| (
                request.request_index(),
                request.from(),
                request.to(),
                request.frame_id()
            ))
            .collect::<Vec<_>>(),
        vec![(0, 1, 100, 7), (1, 101, 200, 8), (2, 201, 250, 9)]
    );
    // 相同计划摘要稳定（用于幂等去重）。
    let same = plan_pull_requests(range, 100, 7, MAX_REQUESTS_PER_PLAN).unwrap();
    assert_eq!(plan.digest(), same.digest());
    assert_eq!(plan.digest_hex().len(), 64);
    // 响应按序校验；重复/乱序/范围不符拒绝。
    let mut validator = ResponseValidator::new(&plan).unwrap();
    assert!(
        validator
            .accept(PageResponse {
                request_index: 0,
                from: 1,
                to: 100,
                items: 100
            })
            .is_ok()
    );
    assert_eq!(
        validator
            .accept(PageResponse {
                request_index: 0,
                from: 1,
                to: 100,
                items: 100
            })
            .unwrap_err()
            .code(),
        "DUPLICATE_RESPONSE"
    );
    assert_eq!(
        validator
            .accept(PageResponse {
                request_index: 2,
                from: 201,
                to: 250,
                items: 50
            })
            .unwrap_err()
            .code(),
        "RESPONSE_MISMATCH"
    );
    assert_eq!(
        validator
            .accept(PageResponse {
                request_index: 1,
                from: 101,
                to: 201,
                items: 101
            })
            .unwrap_err()
            .code(),
        "RESPONSE_MISMATCH"
    );
    assert!(
        validator
            .accept(PageResponse {
                request_index: 1,
                from: 101,
                to: 200,
                items: 100
            })
            .is_ok()
    );
    assert!(
        validator
            .accept(PageResponse {
                request_index: 2,
                from: 201,
                to: 250,
                items: 50
            })
            .is_ok()
    );
    assert!(validator.is_complete());
    assert!(validator.missing_requests().is_empty());
}

#[test]
fn pull_plan_boundaries_are_explicit() {
    // 单条、整除、每页一条、最大页。
    assert_eq!(
        plan_pull_requests(PullRange::new(9, 9).unwrap(), MAX_PULL_PAGE_ITEMS, 0, 4)
            .unwrap()
            .requests()
            .len(),
        1
    );
    assert_eq!(
        plan_pull_requests(PullRange::new(1, 500).unwrap(), 500, 0, 1)
            .unwrap()
            .requests()
            .len(),
        1
    );
    assert_eq!(
        plan_pull_requests(PullRange::new(1, 501).unwrap(), 500, 0, 2)
            .unwrap()
            .requests()
            .len(),
        2
    );
    // 非法输入全部显式拒绝。
    for (from, to, page, max_requests) in [
        (0, 1, 100, 4),
        (2, 1, 100, 4),
        (1, 1, 0, 4),
        (1, 1, MAX_PULL_PAGE_ITEMS + 1, 4),
        (1, 5, 100, 0),
        (1, 5, 100, MAX_REQUESTS_PER_PLAN + 1),
        (1, 1_000, 10, 4),
    ] {
        assert_eq!(
            plan_pull_requests(PullRange { from, to }, page, 0, max_requests)
                .unwrap_err()
                .code(),
            "PULL_PLAN_INVALID"
        );
    }
    assert_eq!(
        plan_pull_requests(PullRange { from: 1, to: 2 }, 1, u32::MAX, 4)
            .unwrap_err()
            .code(),
        "PULL_PLAN_INVALID"
    );
}
