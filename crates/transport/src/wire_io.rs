//! TRANSPORT-006 的真实 iroh 实现：把帧记录读写适配到 iroh 双向流。
//!
//! 帧线格式与纯逻辑核心完全一致：每条记录是 `[u32 BE wire_len][分片元数据][payload]`。
//! 两种用法：
//!
//! - **异步直连 API**：[`WireStream::write_record`]/[`WireStream::read_record`] 以及
//!   [`WireStream::send_frame`]/[`WireStream::receive_frame`]（复用同一 FrameCodec、
//!   发送预算与接收重组器），每次等待都有显式超时；
//! - **同步端口适配**：[`WireStream::split`] 把 iroh 流拆成后台读写任务 + 有界通道，
//!   得到实现 `WireSink`/`WireSource` 的 [`IrohWireSink`]/[`IrohWireSource`]，
//!   既有 `send_peer_frame`/`receive_peer_frame` 纯逻辑管线可直接跑在真实连接上。
//!
//! 所有长度先检查后分配（单记录 ≤ 分片头上限 + 64 KiB 帧上限），超时/关闭映射到
//! `TRANSPORT_TIMEOUT`/`CONNECTION_CLOSED`。

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

use iroh::endpoint::{RecvStream, SendStream};
use tokio::sync::mpsc;

use crate::endpoint::{PeerConnection, validate_timeout};
use crate::error::{Result, TransportError};
use crate::frame::{FRAGMENT_HEADER_BYTES, FRAME_HARD_MAX_BYTES};
use crate::receive::{PeerFrameReceiver, ReceiveBudget, ReceiveOutcome};
use crate::retry::{RetryOutcome, RetryTracker};
use crate::send::{FatalReason, SendBudget, SendOutcome, encode_frame_for_send};
use crate::session::{EndpointId, TransportSession};
use crate::wire::{WireFailure, WireSink, WireSource};

/// 单条记录字节上限（分片头 + 最大帧）。
pub const MAX_RECORD_BYTES: usize = FRAGMENT_HEADER_BYTES + FRAME_HARD_MAX_BYTES;
/// 同步端口桥接队列上限。
pub const MAX_IO_QUEUE: usize = 256;

const ERROR_NONE: u8 = 0;
const ERROR_UNAVAILABLE: u8 = 1;
const ERROR_CLOSED: u8 = 2;

type ErrorSlot = Arc<AtomicU8>;

fn set_error(slot: &ErrorSlot, failure: WireFailure) {
    let code = match failure {
        WireFailure::Unavailable => ERROR_UNAVAILABLE,
        WireFailure::Closed => ERROR_CLOSED,
    };
    slot.store(code, Ordering::SeqCst);
}

fn current_error(slot: &ErrorSlot) -> Option<WireFailure> {
    match slot.load(Ordering::SeqCst) {
        ERROR_UNAVAILABLE => Some(WireFailure::Unavailable),
        ERROR_CLOSED => Some(WireFailure::Closed),
        _ => None,
    }
}

async fn read_one_record(recv: &mut RecvStream) -> Result<Option<Vec<u8>>> {
    let mut length = [0u8; crate::frame::LENGTH_PREFIX_BYTES];
    // 先读 1 字节以区分「流正常结束」与「记录被截断」。
    match recv.read(&mut length[..1]).await {
        Ok(None) => return Ok(None),
        Ok(Some(1)) => {}
        Ok(Some(_)) => {
            return Err(TransportError::MalformedFrame {
                detail: "truncated record length prefix",
            });
        }
        Err(_) => {
            return Err(TransportError::ConnectionClosed {
                detail: "read failed",
            });
        }
    }
    recv.read_exact(&mut length[1..])
        .await
        .map_err(|_| TransportError::ConnectionClosed {
            detail: "truncated record length prefix",
        })?;
    let declared = u32::from_be_bytes(length) as usize;
    let min_payload = FRAGMENT_HEADER_BYTES - crate::frame::LENGTH_PREFIX_BYTES;
    let max_payload = MAX_RECORD_BYTES - crate::frame::LENGTH_PREFIX_BYTES;
    if !(min_payload..=max_payload).contains(&declared) {
        return Err(TransportError::MalformedFrame {
            detail: "record length out of range",
        });
    }
    let mut record = Vec::with_capacity(crate::frame::LENGTH_PREFIX_BYTES + declared);
    record.extend_from_slice(&length);
    record.resize(crate::frame::LENGTH_PREFIX_BYTES + declared, 0);
    recv.read_exact(&mut record[crate::frame::LENGTH_PREFIX_BYTES..])
        .await
        .map_err(|_| TransportError::ConnectionClosed {
            detail: "truncated record payload",
        })?;
    Ok(Some(record))
}

/// 异步发送一帧所需的上下文（避免过多位置参数）。
pub struct SendFrameRequest<'a> {
    /// 会话（可用性/帧上限）。
    pub session: &'a TransportSession,
    /// 帧编号。
    pub frame_id: u32,
    /// 应用负载。
    pub payload: &'a [u8],
    /// 发送预算。
    pub budget: SendBudget,
    /// 重试跟踪器（失败时更新预算，成功时重置）。
    pub retry: &'a mut RetryTracker,
    /// 当前时间（Unix 毫秒）。
    pub now_unix_ms: u64,
}

/// 一条 iroh 双向流上的帧记录读写。
pub struct WireStream {
    send: SendStream,
    recv: RecvStream,
    peer: EndpointId,
}

impl core::fmt::Debug for WireStream {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "WireStream(peer={})", self.peer)
    }
}

impl WireStream {
    /// 由底层流构造（端点模块内部使用）。
    pub(crate) fn new(send: SendStream, recv: RecvStream, peer: EndpointId) -> Self {
        Self { send, recv, peer }
    }

    /// 对端节点 ID（握手认证）。
    pub fn peer_endpoint(&self) -> EndpointId {
        self.peer
    }

    /// 写一条记录；超时 `TRANSPORT_TIMEOUT`，连接/流关闭 `CONNECTION_CLOSED`。
    pub async fn write_record(&mut self, record: &[u8], timeout: Duration) -> Result<()> {
        validate_timeout(timeout)?;
        if record.len() > MAX_RECORD_BYTES {
            return Err(TransportError::ContentTooLarge {
                size: record.len(),
                limit: MAX_RECORD_BYTES,
            });
        }
        match tokio::time::timeout(timeout, self.send.write_all(record)).await {
            Err(_) => Err(TransportError::TransportTimeout {
                detail: "write timed out",
            }),
            Ok(Err(_)) => Err(TransportError::ConnectionClosed {
                detail: "write failed",
            }),
            Ok(Ok(())) => Ok(()),
        }
    }

    /// 读一条记录；`Ok(None)` 表示流正常结束（记录边界处 EOF）。
    pub async fn read_record(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>> {
        validate_timeout(timeout)?;
        match tokio::time::timeout(timeout, read_one_record(&mut self.recv)).await {
            Err(_) => Err(TransportError::TransportTimeout {
                detail: "read timed out",
            }),
            Ok(result) => result,
        }
    }

    /// 结束发送方向（接收方向由 drop 关闭）。
    pub fn finish(self) {
        let mut send = self.send;
        let _ = send.finish();
    }

    /// 异步发送一帧：复用 [`encode_frame_for_send`] 的预算与分片规则。
    pub async fn send_frame(
        &mut self,
        request: SendFrameRequest<'_>,
        timeout: Duration,
    ) -> Result<SendOutcome> {
        let SendFrameRequest {
            session,
            frame_id,
            payload,
            budget,
            retry,
            now_unix_ms,
        } = request;
        let fragments = encode_frame_for_send(session, frame_id, payload, budget)?;
        let mut sent_records = 0usize;
        for (index, fragment) in fragments.iter().enumerate() {
            match self.write_record(&fragment.wire_bytes(), timeout).await {
                Ok(()) => sent_records += 1,
                Err(TransportError::TransportTimeout { .. }) => {
                    return Ok(match retry.record_failure(now_unix_ms) {
                        RetryOutcome::Retry {
                            delay_ms,
                            next_attempt_at_ms,
                            ..
                        } => SendOutcome::RetryableFailure {
                            frame_id,
                            fragment_index: index as u16,
                            sent_records,
                            delay_ms,
                            next_attempt_at_ms,
                        },
                        RetryOutcome::Stop { .. } => SendOutcome::FatalFailure {
                            frame_id,
                            fragment_index: index as u16,
                            sent_records,
                            reason: FatalReason::RetryBudgetExhausted,
                        },
                    });
                }
                Err(TransportError::ConnectionClosed { .. }) => {
                    return Ok(SendOutcome::FatalFailure {
                        frame_id,
                        fragment_index: index as u16,
                        sent_records,
                        reason: FatalReason::Wire(WireFailure::Closed),
                    });
                }
                Err(other) => return Err(other),
            }
        }
        retry.reset();
        Ok(SendOutcome::Sent {
            frame_id,
            fragments: fragments.len() as u16,
            records: sent_records,
        })
    }

    /// 异步接收一帧：复用 [`PeerFrameReceiver`] 的有界解码与严格有序重组。
    pub async fn receive_frame(
        &mut self,
        session: &TransportSession,
        receiver: &mut PeerFrameReceiver,
        budget: ReceiveBudget,
        timeout: Duration,
    ) -> Result<ReceiveOutcome> {
        receiver.validate_binding(session)?;
        budget.validate()?;
        if self.peer != session.peer_endpoint() {
            return Err(TransportError::SessionMismatch {
                detail: "stream peer endpoint does not match the session",
            });
        }
        let mut records = 0usize;
        loop {
            if records >= budget.max_records_per_call {
                return Ok(ReceiveOutcome::Pending {
                    records,
                    budget_exhausted: true,
                });
            }
            match self.read_record(timeout).await {
                Err(TransportError::TransportTimeout { .. }) => {
                    return Ok(ReceiveOutcome::Unavailable { records });
                }
                Err(TransportError::ConnectionClosed { .. }) => {
                    return Ok(ReceiveOutcome::Closed { records });
                }
                Err(other) => return Err(other),
                Ok(None) => {
                    return Ok(ReceiveOutcome::Pending {
                        records,
                        budget_exhausted: false,
                    });
                }
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

    /// 拆成同步 `WireSink`/`WireSource` 适配器（后台任务 + 有界通道）。
    pub fn split(self, queue: usize) -> Result<(IrohWireSink, IrohWireSource)> {
        if queue == 0 || queue > MAX_IO_QUEUE {
            return Err(TransportError::InvalidConfig {
                detail: "io queue must be within 1..=256",
            });
        }
        let Self {
            mut send,
            mut recv,
            peer,
        } = self;
        let (write_tx, mut write_rx) = mpsc::channel::<Vec<u8>>(queue);
        let (read_tx, read_rx) = mpsc::channel::<Vec<u8>>(queue);
        let queued = Arc::new(AtomicUsize::new(0));
        let write_error: ErrorSlot = Arc::new(AtomicU8::new(ERROR_NONE));
        let read_error: ErrorSlot = Arc::new(AtomicU8::new(ERROR_NONE));

        let queued_for_writer = Arc::clone(&queued);
        let write_error_for_writer = Arc::clone(&write_error);
        tokio::spawn(async move {
            while let Some(record) = write_rx.recv().await {
                if send.write_all(&record).await.is_err() {
                    set_error(&write_error_for_writer, WireFailure::Closed);
                    break;
                }
                let _ = queued_for_writer.try_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                    Some(value.saturating_sub(1))
                });
            }
            let _ = send.finish();
        });

        let read_error_for_reader = Arc::clone(&read_error);
        tokio::spawn(async move {
            loop {
                match read_one_record(&mut recv).await {
                    Ok(Some(record)) => {
                        if read_tx.send(record).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        set_error(&read_error_for_reader, WireFailure::Closed);
                        break;
                    }
                    Err(_) => {
                        set_error(&read_error_for_reader, WireFailure::Closed);
                        break;
                    }
                }
            }
        });

        Ok((
            IrohWireSink {
                tx: write_tx,
                queued,
                error: write_error,
            },
            IrohWireSource {
                rx: read_rx,
                error: read_error,
                peer,
            },
        ))
    }
}

/// 同步 `WireSink` 适配器：非阻塞入队，队列满返回可重试失败。
pub struct IrohWireSink {
    tx: mpsc::Sender<Vec<u8>>,
    queued: Arc<AtomicUsize>,
    error: ErrorSlot,
}

impl IrohWireSink {
    /// 等待后台任务写完当前队列；超时 `TRANSPORT_TIMEOUT`，出错 `CONNECTION_CLOSED`。
    pub async fn flush(&self, timeout: Duration) -> Result<()> {
        validate_timeout(timeout)?;
        let waiting = async {
            loop {
                if let Some(failure) = current_error(&self.error) {
                    return Err(TransportError::ConnectionClosed {
                        detail: match failure {
                            WireFailure::Unavailable => "wire became unavailable",
                            WireFailure::Closed => "wire closed",
                        },
                    });
                }
                if self.queued.load(Ordering::SeqCst) == 0 {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        };
        match tokio::time::timeout(timeout, waiting).await {
            Err(_) => Err(TransportError::TransportTimeout {
                detail: "flush timed out",
            }),
            Ok(result) => result,
        }
    }

    /// 后台写任务报告的失败（若有）。
    pub fn failure(&self) -> Option<WireFailure> {
        current_error(&self.error)
    }
}

impl WireSink for IrohWireSink {
    fn write_record(&mut self, record: &[u8]) -> core::result::Result<(), WireFailure> {
        if let Some(failure) = current_error(&self.error) {
            return Err(failure);
        }
        if record.len() > MAX_RECORD_BYTES {
            return Err(WireFailure::Closed);
        }
        match self.tx.try_send(record.to_vec()) {
            Ok(()) => {
                self.queued.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => Err(WireFailure::Unavailable),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(WireFailure::Closed),
        }
    }

    fn queued_records(&self) -> usize {
        self.queued.load(Ordering::SeqCst)
    }
}

/// 同步 `WireSource` 适配器：从后台读任务的有界队列取记录。
pub struct IrohWireSource {
    rx: mpsc::Receiver<Vec<u8>>,
    error: ErrorSlot,
    peer: EndpointId,
}

impl IrohWireSource {
    /// 后台读任务报告的失败（若有）。
    pub fn failure(&self) -> Option<WireFailure> {
        current_error(&self.error)
    }
}

impl WireSource for IrohWireSource {
    fn next_record(&mut self) -> core::result::Result<Option<Vec<u8>>, WireFailure> {
        match self.rx.try_recv() {
            Ok(record) => Ok(Some(record)),
            Err(mpsc::error::TryRecvError::Empty) => match current_error(&self.error) {
                Some(failure) => Err(failure),
                None => Ok(None),
            },
            Err(mpsc::error::TryRecvError::Disconnected) => Err(WireFailure::Closed),
        }
    }

    fn peer_endpoint(&self) -> Option<EndpointId> {
        Some(self.peer)
    }
}

/// 打开一条双向流（便捷函数）。
pub async fn open_stream(connection: &PeerConnection, timeout: Duration) -> Result<WireStream> {
    connection.open_stream(timeout).await
}

/// 接受一条双向流（便捷函数）。
pub async fn accept_stream(connection: &PeerConnection, timeout: Duration) -> Result<WireStream> {
    connection.accept_stream(timeout).await
}
