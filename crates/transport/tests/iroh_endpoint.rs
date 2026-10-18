//! TRANSPORT-002/006 真实 iroh 回环端点的验收用例。
//!
//! 全部用例都有整体 tokio 超时上限（10–30 秒），内部连接/流/读写也用 1–5 秒预算；
//! 任何挂起都必须以超时失败结束，而不是让测试永久阻塞。只绑定 127.0.0.1。

use std::net::SocketAddr;
use std::time::Duration;

use chat_protocol::{
    Capabilities, CapabilityLimits, DeviceId, ProtocolVersion, negotiate_capabilities,
};
use chat_transport::{
    ConnectionEvent, EndpointConfig, EndpointId, FrameLimits, PeerFrameReceiver, ReceiveBudget,
    ReceiveOutcome, RetryPolicy, RetryTracker, SendBudget, SendFrameRequest, SendOutcome,
    SessionConfig, SessionLimits, TransportEndpoint, TransportSession, receive_peer_frame,
    send_peer_frame, start_endpoint,
};

const FRAME_LIMIT: usize = 8 * 1024;
const FRAGMENT: usize = 2 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(5);

fn device(seed: u8) -> DeviceId {
    DeviceId::from_bytes([seed; 32])
}

fn retry_tracker() -> RetryTracker {
    RetryTracker::new(RetryPolicy {
        base_delay_ms: 50,
        max_delay_ms: 1_000,
        max_attempts: 3,
        jitter_max_ms: 0,
        jitter_seed: 0,
    })
    .unwrap()
}

fn session(
    local: EndpointId,
    peer: EndpointId,
    local_device: u8,
    peer_device: u8,
) -> TransportSession {
    let caps = Capabilities::new(
        vec![ProtocolVersion::V1],
        vec!["e2ee.v1".to_owned(), "mls.v1".to_owned()],
        CapabilityLimits::new(FRAME_LIMIT, FRAME_LIMIT, FRAME_LIMIT),
    );
    let negotiated = negotiate_capabilities(caps.clone(), caps).unwrap();
    let mut session = TransportSession::open(SessionConfig {
        local_endpoint: local,
        local_device: device(local_device),
        peer_endpoint: peer,
        peer_device: device(peer_device),
        negotiated: Some(negotiated),
        now_unix_ms: 0,
        limits: SessionLimits {
            max_frame_bytes: FRAME_LIMIT,
            ..SessionLimits::DEFAULT
        },
    })
    .unwrap();
    session.apply(ConnectionEvent::Connect).unwrap();
    session.apply(ConnectionEvent::Connected).unwrap();
    session
}

fn receiver_limits() -> FrameLimits {
    FrameLimits {
        max_frame_bytes: FRAME_LIMIT,
        max_fragment_bytes: FRAGMENT,
        max_fragments_per_frame: 8,
    }
}

fn send_budget() -> SendBudget {
    SendBudget {
        max_fragment_bytes: FRAGMENT,
        ..SendBudget::DEFAULT
    }
}

fn endpoint_config(seed: u8) -> EndpointConfig {
    EndpointConfig::loopback().with_secret_key([seed; 32])
}

async fn connected_pair(
    alice: &TransportEndpoint,
    bob: &TransportEndpoint,
) -> (
    chat_transport::PeerConnection,
    chat_transport::PeerConnection,
) {
    let mut accepts = bob.accept_loop(4).unwrap();
    let alice_conn = alice
        .connect(bob.address().unwrap(), IO_TIMEOUT)
        .await
        .unwrap();
    let bob_conn = accepts.next(IO_TIMEOUT).await.unwrap();
    (alice_conn, bob_conn)
}

async fn timed<F: std::future::Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), future)
        .await
        .expect("test-level timeout: transport operation hung")
}

#[tokio::test]
async fn endpoints_exchange_fragmented_frames_and_reject_oversize() {
    timed(async {
        let alice = start_endpoint(endpoint_config(1)).await.unwrap();
        let bob = start_endpoint(endpoint_config(2)).await.unwrap();
        // 固定密钥 → 确定性节点 ID。
        let alice_expected = iroh::SecretKey::from_bytes(&[1u8; 32]).public();
        assert_eq!(alice.node_id().as_bytes(), alice_expected.as_bytes());
        assert_eq!(alice.endpoint_id(), alice.node_id());
        assert_ne!(alice.node_id(), bob.node_id());
        assert!(alice.local_addr().unwrap().ip().is_loopback());

        let (alice_conn, bob_conn) = connected_pair(&alice, &bob).await;
        assert_eq!(alice_conn.remote_endpoint(), bob.node_id());
        assert_eq!(bob_conn.remote_endpoint(), alice.node_id());

        let mut alice_stream = alice_conn.open_stream(IO_TIMEOUT).await.unwrap();
        let alice_session = session(alice.node_id(), bob.node_id(), 0xa1, 0xb2);
        let bob_session = session(bob.node_id(), alice.node_id(), 0xb2, 0xa1);
        let mut retry = retry_tracker();
        let mut receiver = PeerFrameReceiver::new(&bob_session, receiver_limits()).unwrap();

        // 多分片帧（5 KiB / 2 KiB = 3 片）。
        let payload: Vec<u8> = (0..5 * 1024).map(|index| (index % 251) as u8).collect();
        let outcome = alice_stream
            .send_frame(
                SendFrameRequest {
                    session: &alice_session,
                    frame_id: 1,
                    payload: &payload,
                    budget: send_budget(),
                    retry: &mut retry,
                    now_unix_ms: 0,
                },
                IO_TIMEOUT,
            )
            .await
            .unwrap();
        assert_eq!(
            outcome,
            SendOutcome::Sent {
                frame_id: 1,
                fragments: 3,
                records: 3
            }
        );
        // QUIC 对端在收到首帧数据后才暴露该双向流，因此先发后收。
        let mut bob_stream = bob_conn.accept_stream(IO_TIMEOUT).await.unwrap();
        let received = bob_stream
            .receive_frame(
                &bob_session,
                &mut receiver,
                ReceiveBudget::DEFAULT,
                IO_TIMEOUT,
            )
            .await
            .unwrap();
        assert_eq!(
            received,
            ReceiveOutcome::Frame {
                frame_id: 1,
                payload: payload.clone(),
                records: 3
            }
        );

        // 连续第二帧（单分片）。
        assert!(
            alice_stream
                .send_frame(
                    SendFrameRequest {
                        session: &alice_session,
                        frame_id: 2,
                        payload: b"second",
                        budget: send_budget(),
                        retry: &mut retry,
                        now_unix_ms: 0,
                    },
                    IO_TIMEOUT,
                )
                .await
                .unwrap()
                .is_sent()
        );
        let received = bob_stream
            .receive_frame(
                &bob_session,
                &mut receiver,
                ReceiveBudget::DEFAULT,
                IO_TIMEOUT,
            )
            .await
            .unwrap();
        assert_eq!(
            received,
            ReceiveOutcome::Frame {
                frame_id: 2,
                payload: b"second".to_vec(),
                records: 1
            }
        );
        assert_eq!(receiver.last_completed_frame(), Some(2));

        // 超过会话帧上限：发送前显式拒绝，不写线。
        let oversize = vec![0u8; FRAME_LIMIT + 1];
        assert_eq!(
            alice_stream
                .send_frame(
                    SendFrameRequest {
                        session: &alice_session,
                        frame_id: 3,
                        payload: &oversize,
                        budget: send_budget(),
                        retry: &mut retry,
                        now_unix_ms: 0,
                    },
                    IO_TIMEOUT,
                )
                .await
                .unwrap_err()
                .code(),
            "CONTENT_TOO_LARGE"
        );
        alice_stream.finish();
        bob_conn.close(b"done");
        alice_conn.close(b"done");
        alice.close().await;
        bob.close().await;
    })
    .await;
}

#[tokio::test]
async fn sync_ports_bridge_over_real_connection_and_enforce_queue_budget() {
    timed(async {
        let alice = start_endpoint(endpoint_config(3)).await.unwrap();
        let bob = start_endpoint(endpoint_config(4)).await.unwrap();
        let (alice_conn, bob_conn) = connected_pair(&alice, &bob).await;
        let alice_stream = alice_conn.open_stream(IO_TIMEOUT).await.unwrap();
        let (mut sink, _alice_source) = alice_stream.split(8).unwrap();
        let alice_session = session(alice.node_id(), bob.node_id(), 0xa1, 0xb2);
        let bob_session = session(bob.node_id(), alice.node_id(), 0xb2, 0xa1);

        // 队列深度预算：帧需要 3 条记录，但队列只有 1。
        let tight = SendBudget {
            max_queue_depth: 1,
            ..send_budget()
        };
        let payload = vec![3u8; 5 * 1024];
        assert_eq!(
            send_peer_frame(
                &alice_session,
                1,
                &payload,
                &mut sink,
                tight,
                &mut retry_tracker(),
                0
            )
            .unwrap_err()
            .code(),
            "SEND_BUDGET_EXCEEDED"
        );

        // 正常同步发送 + 后台写任务 flush。
        let outcome = send_peer_frame(
            &alice_session,
            1,
            &payload,
            &mut sink,
            send_budget(),
            &mut retry_tracker(),
            0,
        )
        .unwrap();
        assert!(outcome.is_sent());
        sink.flush(IO_TIMEOUT).await.unwrap();
        // 收到首帧数据后对端才暴露该双向流。
        let bob_stream = bob_conn.accept_stream(IO_TIMEOUT).await.unwrap();
        let (_bob_sink, mut source) = bob_stream.split(8).unwrap();

        let mut receiver = PeerFrameReceiver::new(&bob_session, receiver_limits()).unwrap();
        let mut received = None;
        for _ in 0..200 {
            match receive_peer_frame(
                &bob_session,
                &mut receiver,
                &mut source,
                ReceiveBudget::DEFAULT,
            )
            .unwrap()
            {
                ReceiveOutcome::Frame { payload, .. } => {
                    received = Some(payload);
                    break;
                }
                ReceiveOutcome::Pending { .. } | ReceiveOutcome::Unavailable { .. } => {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                ReceiveOutcome::Closed { .. } => break,
            }
        }
        assert_eq!(received.as_deref(), Some(payload.as_slice()));
        alice_conn.close(b"done");
        bob_conn.close(b"done");
        alice.close().await;
        bob.close().await;
    })
    .await;
}

#[tokio::test]
async fn disconnect_then_reconnect() {
    timed(async {
        let alice = start_endpoint(endpoint_config(5)).await.unwrap();
        let bob = start_endpoint(endpoint_config(6)).await.unwrap();
        let mut accepts = bob.accept_loop(4).unwrap();
        let alice_session = session(alice.node_id(), bob.node_id(), 0xa1, 0xb2);
        let bob_session = session(bob.node_id(), alice.node_id(), 0xb2, 0xa1);
        let mut retry = retry_tracker();

        // 第一次连接：发一帧后关闭。
        let alice_conn = alice
            .connect(bob.address().unwrap(), IO_TIMEOUT)
            .await
            .unwrap();
        let bob_conn = accepts.next(IO_TIMEOUT).await.unwrap();
        let mut alice_stream = alice_conn.open_stream(IO_TIMEOUT).await.unwrap();
        assert!(
            alice_stream
                .send_frame(
                    SendFrameRequest {
                        session: &alice_session,
                        frame_id: 1,
                        payload: b"first",
                        budget: send_budget(),
                        retry: &mut retry,
                        now_unix_ms: 0,
                    },
                    IO_TIMEOUT,
                )
                .await
                .unwrap()
                .is_sent()
        );
        let mut bob_stream = bob_conn.accept_stream(IO_TIMEOUT).await.unwrap();
        let mut receiver = PeerFrameReceiver::new(&bob_session, receiver_limits()).unwrap();
        let first = bob_stream
            .receive_frame(
                &bob_session,
                &mut receiver,
                ReceiveBudget::DEFAULT,
                IO_TIMEOUT,
            )
            .await
            .unwrap();
        assert!(first.is_frame());
        alice_conn.close(b"disconnect");
        let closed = bob_stream
            .receive_frame(
                &bob_session,
                &mut receiver,
                ReceiveBudget::DEFAULT,
                IO_TIMEOUT,
            )
            .await
            .unwrap();
        assert!(
            matches!(closed, ReceiveOutcome::Closed { .. }),
            "expected closed, got {closed:?}"
        );

        // 重连：新连接新流继续交换。
        let alice_conn = alice
            .connect(bob.address().unwrap(), IO_TIMEOUT)
            .await
            .unwrap();
        let bob_conn = accepts.next(IO_TIMEOUT).await.unwrap();
        let mut alice_stream = alice_conn.open_stream(IO_TIMEOUT).await.unwrap();
        assert!(
            alice_stream
                .send_frame(
                    SendFrameRequest {
                        session: &alice_session,
                        frame_id: 2,
                        payload: b"again",
                        budget: send_budget(),
                        retry: &mut retry,
                        now_unix_ms: 0,
                    },
                    IO_TIMEOUT,
                )
                .await
                .unwrap()
                .is_sent()
        );
        let mut bob_stream = bob_conn.accept_stream(IO_TIMEOUT).await.unwrap();
        let mut receiver = PeerFrameReceiver::new(&bob_session, receiver_limits()).unwrap();
        let received = bob_stream
            .receive_frame(
                &bob_session,
                &mut receiver,
                ReceiveBudget::DEFAULT,
                IO_TIMEOUT,
            )
            .await
            .unwrap();
        assert_eq!(
            received,
            ReceiveOutcome::Frame {
                frame_id: 2,
                payload: b"again".to_vec(),
                records: 1
            }
        );
        alice.close().await;
        bob.close().await;
    })
    .await;
}

#[tokio::test]
async fn unreachable_peer_fails_with_timeout_instead_of_hanging() {
    timed(async {
        let alice = start_endpoint(endpoint_config(7)).await.unwrap();
        // 不可达对端：合法节点 ID + 未监听的回环端口。
        let unreachable = iroh::SecretKey::from_bytes(&[9u8; 32]).public();
        let peer = iroh::EndpointAddr::new(unreachable)
            .with_ip_addr(SocketAddr::from(([127, 0, 0, 1], 1)));
        let started = std::time::Instant::now();
        let error = alice
            .connect(peer, Duration::from_millis(300))
            .await
            .unwrap_err();
        assert_eq!(error.code(), "PEER_UNREACHABLE");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "connect must fail within the timeout"
        );
        // 没有入站连接时 accept 显式超时。
        let error = alice.accept(Duration::from_millis(50)).await.unwrap_err();
        assert_eq!(error.code(), "TRANSPORT_TIMEOUT");
        alice.close().await;
    })
    .await;
}

#[tokio::test]
async fn start_endpoint_validates_configuration() {
    timed(async {
        let mut config = EndpointConfig::loopback();
        config.bind_addr = SocketAddr::from(([192, 168, 0, 5], 0));
        assert_eq!(
            start_endpoint(config).await.unwrap_err().code(),
            "INVALID_TRANSPORT_CONFIG"
        );
        let mut config = EndpointConfig::loopback();
        config.alpn.clear();
        assert!(start_endpoint(config).await.is_err());
        // 合法端点可重复关闭。
        let endpoint = start_endpoint(EndpointConfig::loopback()).await.unwrap();
        let mut accepts_err = endpoint.accept_loop(0).unwrap_err();
        assert_eq!(accepts_err.code(), "INVALID_TRANSPORT_CONFIG");
        accepts_err = endpoint
            .connect(
                iroh::EndpointAddr::new(iroh::SecretKey::from_bytes(&[8u8; 32]).public()),
                Duration::ZERO,
            )
            .await
            .unwrap_err();
        assert_eq!(accepts_err.code(), "INVALID_TRANSPORT_CONFIG");
        let queued = endpoint.accept_loop(4).unwrap();
        drop(queued);
        endpoint.close().await;
        endpoint.close().await;
    })
    .await;
}
#[tokio::test]
async fn snapshot_matches_effective_configuration() {
    timed(async {
        let mut config = EndpointConfig::loopback()
            .with_secret_key([11u8; 32])
            .with_relay_urls(vec![
                "https://relay.example.org".to_owned(),
                "http://127.0.0.1:8080".to_owned(),
            ]);
        config.alpn = b"chat/p1/transport/test-alpn".to_vec();
        let requested_bind = config.bind_addr;
        let endpoint = start_endpoint(config).await.unwrap();
        let snapshot = endpoint.snapshot();
        let expected = iroh::SecretKey::from_bytes(&[11u8; 32]).public();
        assert_eq!(snapshot.node_id.as_bytes(), expected.as_bytes());
        assert_eq!(snapshot.bind_addr, requested_bind);
        assert!(snapshot.local_addr.ip().is_loopback());
        assert_ne!(snapshot.local_addr.port(), 0);
        assert!(!snapshot.allow_lan);
        assert_eq!(snapshot.alpn, b"chat/p1/transport/test-alpn".to_vec());
        assert_eq!(
            snapshot.relay_urls,
            vec![
                "https://relay.example.org/".to_owned(),
                "http://127.0.0.1:8080/".to_owned(),
            ]
        );
        assert!(snapshot.relay_enabled);
        assert!(!snapshot.discovery_enabled);
        assert!(snapshot.secret_key_configured);
        // 默认回环配置不启用中继。
        let plain = start_endpoint(EndpointConfig::loopback()).await.unwrap();
        assert!(!plain.snapshot().relay_enabled);
        assert!(plain.snapshot().relay_urls.is_empty());
        assert!(!plain.snapshot().secret_key_configured);
        assert!(!plain.snapshot().allow_lan);
        plain.close().await;
        endpoint.close().await;
    })
    .await;
}

#[tokio::test]
async fn lan_binding_requires_allow_lan() {
    timed(async {
        // 未开 allow_lan：非回环绑定被显式拒绝（启动前）。
        let mut config = EndpointConfig::loopback();
        config.bind_addr = SocketAddr::from(([0, 0, 0, 0], 0));
        assert_eq!(
            config.validate().unwrap_err().code(),
            "INVALID_TRANSPORT_CONFIG"
        );
        assert_eq!(
            start_endpoint(config).await.unwrap_err().code(),
            "INVALID_TRANSPORT_CONFIG"
        );
        // 开启 allow_lan 后允许绑定（离线可复现的通配地址）。
        let mut config = EndpointConfig::loopback().with_allow_lan(true);
        config.bind_addr = SocketAddr::from(([0, 0, 0, 0], 0));
        let endpoint = start_endpoint(config).await.unwrap();
        let snapshot = endpoint.snapshot();
        assert!(snapshot.allow_lan);
        assert!(snapshot.local_addr.ip().is_unspecified());
        assert!(!snapshot.relay_enabled);
        // 通配地址不可直接 dial。
        assert_eq!(
            endpoint.address().unwrap_err().code(),
            "ENDPOINT_START_FAILED"
        );
        endpoint.close().await;
    })
    .await;
}

#[tokio::test]
async fn relay_configuration_is_validated_at_startup() {
    timed(async {
        for urls in [
            vec!["ftp://relay.example.org/".to_owned()],
            vec!["http://relay.example.org/".to_owned()],
            vec!["https://user:pass@relay.example.org/".to_owned()],
            vec!["not a url".to_owned()],
        ] {
            let config = EndpointConfig::loopback().with_relay_urls(urls);
            assert_eq!(
                start_endpoint(config).await.unwrap_err().code(),
                "INVALID_TRANSPORT_CONFIG"
            );
        }
        // 合法但不可达的中继可以启动端点（中继是可选路径）。
        let config =
            EndpointConfig::loopback().with_relay_urls(vec!["https://127.0.0.1:9/".to_owned()]);
        let endpoint = start_endpoint(config).await.unwrap();
        assert!(endpoint.snapshot().relay_enabled);
        assert_eq!(
            endpoint.snapshot().relay_urls,
            vec!["https://127.0.0.1:9/".to_owned()]
        );
        // 回环 http 中继被接受。
        let local =
            EndpointConfig::loopback().with_relay_urls(vec!["http://127.0.0.1:8080".to_owned()]);
        start_endpoint(local).await.unwrap().close().await;
        endpoint.close().await;
    })
    .await;
}

#[tokio::test]
async fn unreachable_relay_does_not_break_loopback_connection() {
    timed(async {
        let relay = vec!["https://127.0.0.1:9/".to_owned()];
        let alice = start_endpoint(
            EndpointConfig::loopback()
                .with_secret_key([21u8; 32])
                .with_relay_urls(relay.clone()),
        )
        .await
        .unwrap();
        let bob = start_endpoint(
            EndpointConfig::loopback()
                .with_secret_key([22u8; 32])
                .with_relay_urls(relay),
        )
        .await
        .unwrap();
        assert!(alice.snapshot().relay_enabled);
        assert!(bob.snapshot().relay_enabled);
        let (alice_conn, bob_conn) = connected_pair(&alice, &bob).await;
        let mut alice_stream = alice_conn.open_stream(IO_TIMEOUT).await.unwrap();
        let alice_session = session(alice.node_id(), bob.node_id(), 0xa1, 0xb2);
        let bob_session = session(bob.node_id(), alice.node_id(), 0xb2, 0xa1);
        let mut retry = retry_tracker();
        let payload = b"relay-independent".to_vec();
        assert!(
            alice_stream
                .send_frame(
                    SendFrameRequest {
                        session: &alice_session,
                        frame_id: 1,
                        payload: &payload,
                        budget: send_budget(),
                        retry: &mut retry,
                        now_unix_ms: 0,
                    },
                    IO_TIMEOUT,
                )
                .await
                .unwrap()
                .is_sent()
        );
        let mut bob_stream = bob_conn.accept_stream(IO_TIMEOUT).await.unwrap();
        let mut receiver = PeerFrameReceiver::new(&bob_session, receiver_limits()).unwrap();
        let received = bob_stream
            .receive_frame(
                &bob_session,
                &mut receiver,
                ReceiveBudget::DEFAULT,
                IO_TIMEOUT,
            )
            .await
            .unwrap();
        assert_eq!(
            received,
            ReceiveOutcome::Frame {
                frame_id: 1,
                payload,
                records: 1
            }
        );
        alice_conn.close(b"done");
        bob_conn.close(b"done");
        alice.close().await;
        bob.close().await;
    })
    .await;
}
