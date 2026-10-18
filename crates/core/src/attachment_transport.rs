//! 用真实节点客户端实现附件块传输端口 [crate::attachments::BlobTransport]
//! （NODE-012 begin / NODE-013 put / NODE-014 complete / NODE-015 read）。
//!
//! 端口是同步的、NodeClient 是异步的，因此与 node_submitter 用同一条边界：
//! **独立线程 + current-thread 运行时**做同步→异步桥，本模块的方法可以在任何上下文
//! （包括已经在异步运行时里的调用方）安全调用，不会出现“在运行时里阻塞”的 panic。
//!
//! 标识派生（节点侧的 blob 与本地附件 ID 是两套标识，这里做确定性映射）：
//!
//! - 节点 blob 标识 = `obj:1:<sha256("chat/attachment/blob-id/v1" || attachment_id)>`：
//!   同一附件在任意进程/重启后都得到同一 blob 标识，下载端无需额外注册表；
//! - begin 的 operation ID = `op:1:<sha256(域 || attachment_id || ciphertext_digest)>`：
//!   同一次上传的重试复用同一 operation（节点按 (设备, operation) 幂等返回同一会话），
//!   不同附件/不同密文则得到新会话。
//!
//! 声明块大小（`chunk_bytes`）：节点要求“非末块满块、末块不超过声明块大小”，而客户端
//! 分块密文 = 明文块 + 封装开销（版本字节 + nonce + tag）。因此：
//!
//! - `chunk_count > 1` → `chunk_bytes = chunk_size + CIPHERTEXT_CHUNK_OVERHEAD`；
//! - `chunk_count == 1` → `chunk_bytes = ciphertext_bytes`（唯一块就是末块）。
//!
//! 上传前先做本地一致性校验（`ciphertext_bytes == plain_size + chunk_count * 开销`），
//! 不把自相矛盾的清单发到节点，也不篡改尺寸去凑节点策略。
//!
//! 错误映射：节点/网络错误一律变成 `AttachmentError::Transport { code, detail }`，
//! `code` 优先取节点返回的稳定码（`BLOB_UPLOAD_CONFLICT` / `MANIFEST_MISMATCH` /
//! `CHUNK_OUT_OF_ORDER` / `UPLOAD_EXPIRED` / `QUOTA_EXCEEDED` / `RATE_LIMITED` /
//! `PERMISSION_DENIED` …），没有节点码时退回 PROTOCOL-014 码（`NODE_UNAVAILABLE` 等），
//! 绝不吞错、绝不把失败当成功。
//!
//! 已知限制（详见 docs/implementation/attachments.md）：节点 blob 策略的块上限
//! （`BlobPolicy::max_chunk_bytes`，默认 32 KiB、硬上限 60 KiB）小于客户端附件分块
//! （256 KiB），因此**多块附件会被节点显式拒绝**（CONTENT_TOO_LARGE/MANIFEST_MISMATCH），
//! 只有单块附件（密文 ≤ 策略上限）能走通；这是节点策略与客户端分块参数的差异，本层不隐藏。

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use chat_crypto::attachment::{NONCE_BYTES, TAG_BYTES};
use chat_node_client::{NodeClient, NodeClientError};
use chat_node_services::blob::{
    BlobReceipt, ChunkReceipt, CipherManifest, PublicBlobDescriptor,
    UploadSession as NodeUploadSession,
};
use chat_protocol::{GroupId, ObjectId, OperationId};
use sha2::{Digest, Sha256};

use crate::attachments::{
    AttachmentError, AttachmentResult, BlobTransport, UploadRequest, UploadSession,
    validate_attachment_id,
};

/// 单块密文的封装开销（版本字节 + nonce + tag），与 CRYPTO-011 的分块格式一致。
///
/// 用公开常量计算（crypto 内部的 `CHUNK_OVERHEAD` 是私有的）：测试会用真实导入的
/// 分块长度反证这个值与实际格式一致（尺寸不符会被节点以 MANIFEST_MISMATCH 拒绝）。
pub const CIPHERTEXT_CHUNK_OVERHEAD: usize = 1 + NONCE_BYTES + TAG_BYTES;

/// 节点 blob 标识派生域。
const BLOB_ID_DOMAIN: &[u8] = b"chat/attachment/blob-id/v1";
/// 上传 operation 派生域。
const UPLOAD_OPERATION_DOMAIN: &[u8] = b"chat/attachment/upload-operation/v1";

fn digest(domain: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(hasher.finalize().as_slice());
    bytes
}

/// 本地附件 ID → 节点 blob 标识（PROTOCOL-001 `obj:1:<hex>` 文本）。
pub fn blob_object_id_for(attachment_id: &str) -> AttachmentResult<String> {
    validate_attachment_id(attachment_id)?;
    let bytes = digest(BLOB_ID_DOMAIN, &[attachment_id.as_bytes()]);
    Ok(ObjectId::from_bytes(bytes).encode())
}

/// 本地附件 ID + 整文件密文摘要 → 上传 operation（PROTOCOL-001 `op:1:<hex>` 文本）。
pub fn upload_operation_id_for(
    attachment_id: &str,
    ciphertext_digest: &str,
) -> AttachmentResult<String> {
    validate_attachment_id(attachment_id)?;
    if ciphertext_digest.len() != 64 {
        return Err(AttachmentError::AttachmentCorrupted(
            "ciphertext digest is not a 32-byte lowercase hex value",
        ));
    }
    let bytes = digest(
        UPLOAD_OPERATION_DOMAIN,
        &[attachment_id.as_bytes(), ciphertext_digest.as_bytes()],
    );
    Ok(OperationId::from_bytes(bytes).encode())
}

/// 声明块大小：节点要求“非末块满块”，因此这里按客户端分块格式换算密文块长度。
fn declared_chunk_bytes(request: &UploadRequest) -> AttachmentResult<u32> {
    if request.chunk_count == 0 {
        return Err(AttachmentError::InvalidChunkContext(
            "upload request declares zero chunks",
        ));
    }
    let overhead = CIPHERTEXT_CHUNK_OVERHEAD as u64;
    let expected_total = request
        .plain_size
        .checked_add(u64::from(request.chunk_count).saturating_mul(overhead))
        .ok_or(AttachmentError::ContentTooLarge)?;
    if expected_total != request.ciphertext_bytes {
        return Err(AttachmentError::AttachmentCorrupted(
            "declared ciphertext size does not match the chunk layout",
        ));
    }
    let chunk_bytes = if request.chunk_count > 1 {
        u64::from(request.chunk_size)
            .checked_add(overhead)
            .ok_or(AttachmentError::ContentTooLarge)?
    } else {
        request.ciphertext_bytes
    };
    if chunk_bytes == 0 {
        return Err(AttachmentError::AttachmentCorrupted(
            "declared chunk size is zero",
        ));
    }
    u32::try_from(chunk_bytes).map_err(|_| AttachmentError::ContentTooLarge)
}

/// 桥内部错误：区分“节点回答了一个错误”和“桥本身不可用”。
#[derive(Debug)]
enum BridgeFailure {
    Client(Box<NodeClientError>),
    Unavailable,
}

/// 一次桥调用。
enum TransportCall {
    Begin {
        descriptor: Box<PublicBlobDescriptor>,
        operation_id: String,
        reply: Sender<Result<NodeUploadSession, NodeClientError>>,
    },
    Put {
        upload_id: String,
        index: u32,
        bytes: Vec<u8>,
        reply: Sender<Result<ChunkReceipt, NodeClientError>>,
    },
    Complete {
        upload_id: String,
        manifest: Box<CipherManifest>,
        reply: Sender<Result<BlobReceipt, NodeClientError>>,
    },
    Read {
        blob_id: String,
        index: u32,
        reply: Sender<Result<Vec<u8>, NodeClientError>>,
    },
}

/// 同步→异步桥：独立线程 + current-thread 运行时。
struct TransportBridge {
    calls: Sender<TransportCall>,
    thread: Option<JoinHandle<()>>,
}

impl TransportBridge {
    fn spawn(client: Arc<NodeClient>) -> AttachmentResult<Self> {
        let (calls, inbox) = channel();
        let thread = thread::Builder::new()
            .name("chat-attachment-transport".to_owned())
            .spawn(move || run_bridge(client, inbox))
            .map_err(|_| {
                AttachmentError::transport(
                    "INTERNAL_FAILURE",
                    "attachment transport thread could not start",
                )
            })?;
        Ok(Self {
            calls,
            thread: Some(thread),
        })
    }

    fn begin(
        &self,
        descriptor: PublicBlobDescriptor,
        operation_id: String,
    ) -> Result<NodeUploadSession, BridgeFailure> {
        let (reply, answer) = channel();
        self.dispatch(TransportCall::Begin {
            descriptor: Box::new(descriptor),
            operation_id,
            reply,
        })?;
        answer
            .recv()
            .map_err(|_| BridgeFailure::Unavailable)?
            .map_err(|error| BridgeFailure::Client(Box::new(error)))
    }

    fn put(
        &self,
        upload_id: &str,
        index: u32,
        bytes: Vec<u8>,
    ) -> Result<ChunkReceipt, BridgeFailure> {
        let (reply, answer) = channel();
        self.dispatch(TransportCall::Put {
            upload_id: upload_id.to_owned(),
            index,
            bytes,
            reply,
        })?;
        answer
            .recv()
            .map_err(|_| BridgeFailure::Unavailable)?
            .map_err(|error| BridgeFailure::Client(Box::new(error)))
    }

    fn complete(
        &self,
        upload_id: &str,
        manifest: CipherManifest,
    ) -> Result<BlobReceipt, BridgeFailure> {
        let (reply, answer) = channel();
        self.dispatch(TransportCall::Complete {
            upload_id: upload_id.to_owned(),
            manifest: Box::new(manifest),
            reply,
        })?;
        answer
            .recv()
            .map_err(|_| BridgeFailure::Unavailable)?
            .map_err(|error| BridgeFailure::Client(Box::new(error)))
    }

    fn read(&self, blob_id: &str, index: u32) -> Result<Vec<u8>, BridgeFailure> {
        let (reply, answer) = channel();
        self.dispatch(TransportCall::Read {
            blob_id: blob_id.to_owned(),
            index,
            reply,
        })?;
        answer
            .recv()
            .map_err(|_| BridgeFailure::Unavailable)?
            .map_err(|error| BridgeFailure::Client(Box::new(error)))
    }

    fn dispatch(&self, call: TransportCall) -> Result<(), BridgeFailure> {
        self.calls.send(call).map_err(|_| BridgeFailure::Unavailable)
    }
}

impl Drop for TransportBridge {
    fn drop(&mut self) {
        // 丢掉发送端 → 桥线程的 recv 立刻返回 Err → 循环退出；再 join 避免线程继续跑网络请求。
        let (dead, _) = channel();
        let _ = std::mem::replace(&mut self.calls, dead);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_bridge(client: Arc<NodeClient>, inbox: Receiver<TransportCall>) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        // 运行时建不起来：直接退出，调用方会看到 Unavailable 并显式失败。
        return;
    };
    while let Ok(call) = inbox.recv() {
        runtime.block_on(async {
            match call {
                TransportCall::Begin {
                    descriptor,
                    operation_id,
                    reply,
                } => {
                    let _ = reply.send(
                        client
                            .begin_blob_upload(&operation_id, &descriptor)
                            .await,
                    );
                }
                TransportCall::Put {
                    upload_id,
                    index,
                    bytes,
                    reply,
                } => {
                    let _ = reply.send(client.put_blob_chunk(&upload_id, index, &bytes).await);
                }
                TransportCall::Complete {
                    upload_id,
                    manifest,
                    reply,
                } => {
                    let _ = reply.send(
                        client
                            .complete_blob_upload(&upload_id, &manifest)
                            .await,
                    );
                }
                TransportCall::Read {
                    blob_id,
                    index,
                    reply,
                } => {
                    let _ = reply.send(client.read_blob_chunk(&blob_id, index).await);
                }
            }
        });
    }
}

/// 节点错误的稳定码：优先节点返回码，其次 PROTOCOL-014 码（两者都是冻结码）。
fn node_code(error: &NodeClientError) -> &'static str {
    error
        .node_code()
        .unwrap_or_else(|| error.code().as_str())
}

/// 节点错误的静态说明（不含 URL、正文或密钥）。
fn node_detail(error: &NodeClientError) -> String {
    match (error.node_code(), error.http_status()) {
        (Some(code), _) => format!("node answered {code}"),
        (None, Some(status)) => format!("node answered http {status}"),
        (None, None) => "node request failed".to_owned(),
    }
}

fn transport_failure(failure: BridgeFailure) -> AttachmentError {
    match failure {
        BridgeFailure::Client(error) => {
            AttachmentError::transport(node_code(&error), node_detail(&error))
        }
        BridgeFailure::Unavailable => AttachmentError::transport(
            "NODE_UNAVAILABLE",
            "attachment transport bridge is unavailable",
        ),
    }
}

/// 一次上传尝试的只读绑定（complete 需要清单里的块数/总字节）。
struct SessionBinding {
    attachment_id: String,
    chunk_count: u32,
    ciphertext_bytes: u64,
}

#[derive(Default)]
struct TransportStatsInner {
    begins: AtomicU64,
    puts: AtomicU64,
    completes: AtomicU64,
    reads: AtomicU64,
}

/// 传输端口的调用计数（克隆共享；用于断言“重试没有产生第二次会话/重复块”）。
#[derive(Clone, Default)]
pub struct TransportStats {
    inner: Arc<TransportStatsInner>,
}

impl TransportStats {
    /// begin_blob_upload 调用次数。
    pub fn begin_calls(&self) -> u64 {
        self.inner.begins.load(Ordering::SeqCst)
    }

    /// put_blob_chunk 调用次数。
    pub fn put_calls(&self) -> u64 {
        self.inner.puts.load(Ordering::SeqCst)
    }

    /// complete_blob_upload 调用次数。
    pub fn complete_calls(&self) -> u64 {
        self.inner.completes.load(Ordering::SeqCst)
    }

    /// read_blob_chunk 调用次数。
    pub fn read_calls(&self) -> u64 {
        self.inner.reads.load(Ordering::SeqCst)
    }
}

impl fmt::Debug for TransportStats {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportStats")
            .field("begins", &self.begin_calls())
            .field("puts", &self.put_calls())
            .field("completes", &self.complete_calls())
            .field("reads", &self.read_calls())
            .finish()
    }
}

/// 真实节点上的附件块传输端口。
///
/// 一个实例绑定一台设备（签名钥在 [NodeClient] 里）与一个群 + 关联日志序号
/// （NODE-012 要求上传必须挂到“作者正是本设备”的已接受条目上）。
pub struct NodeBlobTransport {
    bridge: TransportBridge,
    group_id: String,
    source_seq: u64,
    device_id: String,
    sessions: Mutex<HashMap<String, SessionBinding>>,
    stats: TransportStats,
}

impl fmt::Debug for NodeBlobTransport {
    /// 只暴露设备/群/关联序号与调用计数；客户端与签名钥不进日志。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NodeBlobTransport")
            .field("device_id", &self.device_id)
            .field("group_id", &self.group_id)
            .field("source_seq", &self.source_seq)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl NodeBlobTransport {
    /// 用已构造的节点客户端（承载 endpoint + 设备凭据）与上传绑定构造。
    ///
    /// group_id 必须是 PROTOCOL-001 群文本；source_seq 是本设备已接受日志条目的序号
    /// （附件消息的 seq），节点用它做上传授权关联。
    pub fn new(
        client: NodeClient,
        group_id: impl Into<String>,
        source_seq: u64,
    ) -> AttachmentResult<Self> {
        let group_id = group_id.into();
        GroupId::parse(&group_id).map_err(|_| {
            AttachmentError::transport(
                "INVALID_TARGET",
                "group id must be PROTOCOL-001 group text",
            )
        })?;
        if source_seq == 0 {
            return Err(AttachmentError::transport(
                "INVALID_TARGET",
                "upload source seq must be a positive log sequence",
            ));
        }
        let device_id = client.device_id().to_owned();
        let bridge = TransportBridge::spawn(Arc::new(client))?;
        Ok(Self {
            bridge,
            group_id,
            source_seq,
            device_id,
            sessions: Mutex::new(HashMap::new()),
            stats: TransportStats::default(),
        })
    }

    /// 绑定的设备标识。
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// 绑定的群标识。
    pub fn group_id(&self) -> &str {
        &self.group_id
    }

    /// 绑定的关联日志序号。
    pub fn source_seq(&self) -> u64 {
        self.source_seq
    }

    /// 可克隆的调用计数句柄（在交给上传/下载编排之前取一份即可在外部断言）。
    pub fn stats(&self) -> TransportStats {
        self.stats.clone()
    }

    fn lock_sessions(
        &self,
    ) -> AttachmentResult<std::sync::MutexGuard<'_, HashMap<String, SessionBinding>>> {
        self.sessions.lock().map_err(|_| {
            AttachmentError::InternalFailure("attachment transport session table is poisoned")
        })
    }
}

impl BlobTransport for NodeBlobTransport {
    fn begin_upload(&mut self, request: UploadRequest) -> AttachmentResult<UploadSession> {
        validate_attachment_id(&request.attachment_id)?;
        let chunk_bytes = declared_chunk_bytes(&request)?;
        let blob_id = blob_object_id_for(&request.attachment_id)?;
        let operation_id =
            upload_operation_id_for(&request.attachment_id, &request.ciphertext_digest)?;
        let descriptor = PublicBlobDescriptor {
            blob_id: blob_id.clone(),
            group_id: self.group_id.clone(),
            source_seq: self.source_seq,
            ciphertext_bytes: request.ciphertext_bytes,
            chunk_bytes,
            chunk_count: request.chunk_count,
        };
        self.stats.inner.begins.fetch_add(1, Ordering::SeqCst);
        let session = self
            .bridge
            .begin(descriptor, operation_id)
            .map_err(transport_failure)?;
        self.lock_sessions()?.insert(
            session.upload_id.clone(),
            SessionBinding {
                attachment_id: request.attachment_id,
                chunk_count: request.chunk_count,
                ciphertext_bytes: request.ciphertext_bytes,
            },
        );
        Ok(UploadSession::new(session.upload_id))
    }

    fn put_chunk(&mut self, session: &str, index: u32, bytes: &[u8]) -> AttachmentResult<()> {
        if bytes.is_empty() {
            return Err(AttachmentError::InvalidChunkContext(
                "ciphertext chunk must not be empty",
            ));
        }
        self.stats.inner.puts.fetch_add(1, Ordering::SeqCst);
        self.bridge
            .put(session, index, bytes.to_vec())
            .map_err(transport_failure)?;
        Ok(())
    }

    fn complete(&mut self, session: &str, total_ciphertext_digest: &str) -> AttachmentResult<()> {
        let binding = self.lock_sessions()?.remove(session).ok_or_else(|| {
            AttachmentError::transport(
                "UPLOAD_NOT_FOUND",
                "upload session is not known to this transport instance",
            )
        })?;
        // 摘要必须对应本次绑定：节点会再用声明块数/总字节复核一次。
        let manifest = CipherManifest {
            chunk_count: binding.chunk_count,
            ciphertext_bytes: binding.ciphertext_bytes,
            ciphertext_digest: total_ciphertext_digest.to_owned(),
        };
        self.stats.inner.completes.fetch_add(1, Ordering::SeqCst);
        let receipt = self
            .bridge
            .complete(session, manifest)
            .map_err(transport_failure)?;
        if receipt.chunk_count != binding.chunk_count
            || receipt.ciphertext_bytes != binding.ciphertext_bytes
        {
            // 回执必须绑定本次清单；不匹配属于协议违规，显式失败而不是当作成功。
            return Err(AttachmentError::transport(
                "INVALID_RECEIPT_SCOPE",
                "node publish receipt does not bind the declared manifest",
            ));
        }
        if receipt.blob_id != blob_object_id_for(&binding.attachment_id)? {
            return Err(AttachmentError::transport(
                "INVALID_RECEIPT_SCOPE",
                "node publish receipt does not bind this attachment blob",
            ));
        }
        Ok(())
    }

    fn read_chunk(&mut self, blob_id: &str, index: u32) -> AttachmentResult<Vec<u8>> {
        let object_id = blob_object_id_for(blob_id)?;
        self.stats.inner.reads.fetch_add(1, Ordering::SeqCst);
        self.bridge.read(&object_id, index).map_err(transport_failure)
    }
}
