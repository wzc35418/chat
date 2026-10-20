//! 附件上传/下载接真节点（ATTACHMENTS-003/005 的节点侧装配，FFI 只传字符串）。
//!
//! 本模块把三件已经在别处实现好的东西接起来：
//!
//! 1. **持久身份**（正式库里的身份种子）→ 设备签名钥：与 send 链路同一条派生路径
//!    （`Store::load_identity_seeds` + `signing_key_from_seed`），因此节点看到的
//!    `dev:1:…` 与本地身份一致；FFI 不需要、也不允许传任何种子/口令类入参；
//! 2. [NodeBlobTransport]（NODE-012~015）：真节点客户端 + 会话/回执验签；
//! 3. [PersistedAttachmentStore] 的 `upload_attachment` / `download_attachment`：
//!    状态白名单、分块密文逐块上传/认证解密、失败保留进度、绝不伪造 available。
//!
//! 约定：
//!
//! - 上传必须绑定“作者正是本设备”的已接受日志条目（NODE-012 授权），即 `source_seq`；
//! - 下载只需要凭据（NODE-015：上传设备本人恒可读），`group_id`/`source_seq` 仍按端口
//!   构造契约校验（可传上传时相同的值）；
//! - 解密需要进程内的 FileKey：跨进程/重启后下载会显式 `ATTACHMENT_KEY_UNAVAILABLE`，
//!   在 E2EE SecretDescriptor 落地前不伪造“可下载”。
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_protocol::signing::signing_key_from_seed;
use chat_protocol::GroupId;
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, StoreOptions, open_store};

use crate::attachment_transport::{
    CIPHERTEXT_CHUNK_OVERHEAD, NodeBlobTransport,
};
use crate::attachments::{
    AttachmentError, AttachmentManifest, AttachmentResult, AttachmentStatus, DownloadReceipt,
    FileToken, PersistedAttachmentStore,
};

/// 主密钥字节数（与 storage 的 MASTER_KEY_BYTES 一致）。
const MASTER_KEY_BYTES: usize = 32;
/// 设备密钥种子字节数。
const DEVICE_SEED_BYTES: usize = 32;
/// 节点公钥字节数。
const PUBLIC_KEY_BYTES: usize = 32;

/// 打开“持久附件入口”：正式加密库（含 attachments 表）+ 加密分块缓存目录。
///
/// `database_path` 必须已存在；主密钥是 64 位十六进制（32 字节）；错误密钥/损坏库显式失败。
/// 同一库是**独占单写者**：调用方必须先释放其它句柄（例如先 `ConversationCore::close`）。
pub fn open_persisted_attachments(
    database_path: &str,
    master_key_hex: &str,
    cache_root: &str,
) -> AttachmentResult<PersistedAttachmentStore> {
    if database_path.is_empty() {
        return Err(AttachmentError::transport(
            "INVALID_INPUT",
            "database path must not be empty",
        ));
    }
    if cache_root.is_empty() {
        return Err(AttachmentError::transport(
            "INVALID_INPUT",
            "cache root must not be empty",
        ));
    }
    let master_key = decode_master_key(master_key_hex)?;
    let store = open_store(
        StoreOptions::new(database_path, KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes(master_key),
    )
    .map_err(AttachmentError::from)?;
    PersistedAttachmentStore::open(cache_root, store)
}

fn decode_master_key(text: &str) -> AttachmentResult<[u8; MASTER_KEY_BYTES]> {
    let bytes = text.as_bytes();
    if bytes.len() != MASTER_KEY_BYTES * 2 {
        return Err(AttachmentError::transport(
            "INVALID_INPUT",
            "master key must be exactly 64 hexadecimal characters",
        ));
    }
    let mut key = [0u8; MASTER_KEY_BYTES];
    let (pairs, remainder) = bytes.as_chunks::<2>();
    if !remainder.is_empty() {
        return Err(AttachmentError::transport(
            "INVALID_INPUT",
            "master key must be exactly 64 hexadecimal characters",
        ));
    }
    for (index, pair) in pairs.iter().enumerate() {
        let high = decode_nibble(pair[0])?;
        let low = decode_nibble(pair[1])?;
        key[index] = (high << 4) | low;
    }
    Ok(key)
}

fn decode_nibble(byte: u8) -> AttachmentResult<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(AttachmentError::transport(
            "INVALID_INPUT",
            "master key must be exactly 64 hexadecimal characters",
        )),
    }
}

/// 一次上传/下载的节点参数（全部来自调用方的字符串，不含任何密钥材料）。
#[derive(Clone, PartialEq, Eq)]
pub struct AttachmentNodeConfig {
    /// 节点 HTTP(S) 基地址。
    pub endpoint_url: String,
    /// 节点标识。
    pub node_id: String,
    /// 可选的固定节点签名公钥（32 字节）。
    pub node_public_key: Option<[u8; PUBLIC_KEY_BYTES]>,
    /// 附件所属的协议群（PROTOCOL-001 `grp:1:<hex>`）。
    pub group_id: String,
    /// 本设备已接受日志条目的序号（NODE-012 的上传授权关联）。
    pub source_seq: u64,
}

impl std::fmt::Debug for AttachmentNodeConfig {
    /// 只暴露地址/群/序号；公钥只报长度。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttachmentNodeConfig")
            .field("endpoint_url", &self.endpoint_url)
            .field("node_id", &self.node_id)
            .field(
                "node_public_key_len",
                &self.node_public_key.map(|_| PUBLIC_KEY_BYTES),
            )
            .field("group_id", &self.group_id)
            .field("source_seq", &self.source_seq)
            .finish()
    }
}

impl AttachmentNodeConfig {
    /// 校验并构造：端点必须可解析、群 ID 必须是 PROTOCOL-001 文本、序号必须为正、
    /// 公钥给出时必须是 64 位十六进制。
    pub fn new(
        endpoint_url: impl Into<String>,
        node_id: impl Into<String>,
        node_public_key_hex: Option<&str>,
        group_id: impl Into<String>,
        source_seq: u64,
    ) -> AttachmentResult<Self> {
        let endpoint_url = endpoint_url.into();
        let node_id = node_id.into();
        let group_id = group_id.into();
        if node_id.trim().is_empty() || node_id.len() > 128 {
            return Err(AttachmentError::transport(
                "INVALID_TARGET",
                "node id must be a non-empty bounded identifier",
            ));
        }
        NodeEndpoint::new(&endpoint_url, &node_id).map_err(|_| {
            AttachmentError::transport(
                "INVALID_TARGET",
                "node endpoint must be a parseable http(s) url",
            )
        })?;
        GroupId::parse(&group_id).map_err(|_| {
            AttachmentError::transport("INVALID_TARGET", "group id must be PROTOCOL-001 group text")
        })?;
        if source_seq == 0 {
            return Err(AttachmentError::transport(
                "INVALID_TARGET",
                "source seq must be a positive log sequence",
            ));
        }
        let node_public_key = match node_public_key_hex {
            Some(text) => Some(decode_public_key(text)?),
            None => None,
        };
        Ok(Self {
            endpoint_url,
            node_id,
            node_public_key,
            group_id,
            source_seq,
        })
    }
}

fn decode_public_key(text: &str) -> AttachmentResult<[u8; PUBLIC_KEY_BYTES]> {
    if text.len() != PUBLIC_KEY_BYTES * 2 {
        return Err(AttachmentError::transport(
            "INVALID_SIGNATURE",
            "node public key must be 64 hexadecimal characters",
        ));
    }
    let mut key = [0u8; PUBLIC_KEY_BYTES];
    for (index, pair) in text.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let high = decode_nibble(pair[0]).map_err(|_| {
            AttachmentError::transport(
                "INVALID_SIGNATURE",
                "node public key must be 64 hexadecimal characters",
            )
        })?;
        let low = decode_nibble(pair[1]).map_err(|_| {
            AttachmentError::transport(
                "INVALID_SIGNATURE",
                "node public key must be 64 hexadecimal characters",
            )
        })?;
        key[index] = (high << 4) | low;
    }
    Ok(key)
}

/// 用**持久身份**派生设备凭据并构造块传输端口。
///
/// 身份未入库时显式 `INVALID_STATE_TRANSITION`；节点客户端构造失败显式
/// `INVALID_TARGET`（不重试、不猜测）。
pub fn node_blob_transport(
    store: &PersistedAttachmentStore,
    config: &AttachmentNodeConfig,
) -> AttachmentResult<NodeBlobTransport> {
    let (device_id, signing_key) = store.with_formal_store(|formal| {
        let seeds = formal
            .load_identity_seeds()
            .map_err(AttachmentError::from)?
            .ok_or_else(|| {
                AttachmentError::transport(
                    "INVALID_STATE_TRANSITION",
                    "the local identity is not enrolled in this database",
                )
            })?;
        let device_seed: [u8; DEVICE_SEED_BYTES] =
            seeds.device_seed.as_slice().try_into().map_err(|_| {
                AttachmentError::AttachmentCorrupted(
                    "persisted device seed has an unexpected length",
                )
            })?;
        Ok((seeds.device_id.clone(), signing_key_from_seed(&device_seed)))
    })?;
    let mut endpoint = NodeEndpoint::new(&config.endpoint_url, &config.node_id).map_err(|_| {
        AttachmentError::transport(
            "INVALID_TARGET",
            "node endpoint must be a parseable http(s) url",
        )
    })?;
    if let Some(public_key) = config.node_public_key {
        endpoint = endpoint.with_node_public_key(public_key);
    }
    let client = NodeClient::new(endpoint, device_id, signing_key).map_err(|error| {
        let detail = format!(
            "node client could not be created from the persisted device identity: {}",
            error.detail()
        );
        AttachmentError::transport("INVALID_TARGET", detail)
    })?;
    NodeBlobTransport::new(client, config.group_id.clone(), config.source_seq)
}

/// ATTACHMENTS-003：把本地缓存的分块密文按序上传到真节点，成功后返回真实状态（available）。
///
/// 失败保留本地缓存与进度、状态停在 uploading（可重试）；网络失败绝不标 available。
pub fn upload_attachment_to_node(
    store: &PersistedAttachmentStore,
    attachment_id: &str,
    config: &AttachmentNodeConfig,
) -> AttachmentResult<AttachmentStatus> {
    let mut transport = node_blob_transport(store, config)?;
    store.upload_attachment(attachment_id, &mut transport)
}

/// ATTACHMENTS-005：从真节点取回分块密文、认证解密并写出目标文件。
///
/// 清单来自**本地持久记录**（块数/块大小/明文大小/密文摘要），与记录不一致的远端数据
/// 会在任何写盘之前被拒绝（`ATTACHMENT_CORRUPTED`）；没有进程内 FileKey 时显式
/// `ATTACHMENT_KEY_UNAVAILABLE`。
pub fn download_attachment_from_node(
    store: &PersistedAttachmentStore,
    attachment_id: &str,
    destination: &FileToken,
    config: &AttachmentNodeConfig,
) -> AttachmentResult<DownloadReceipt> {
    let record = store
        .attachment_record(attachment_id)?
        .ok_or(AttachmentError::AttachmentNotFound)?;
    let manifest = manifest_for_record(&record)?;
    let mut transport = node_blob_transport(store, config)?;
    store.download_attachment(attachment_id, &manifest, destination, &mut transport)
}

/// 由持久记录推导公开清单（`ciphertext_bytes` 按分块格式推导：明文 + 块数 × 封装开销）。
pub fn manifest_for_record(
    record: &crate::attachments::StoredAttachmentRecord,
) -> AttachmentResult<AttachmentManifest> {
    if record.chunk_count == 0 {
        return Err(AttachmentError::AttachmentCorrupted(
            "persisted attachment declares zero chunks",
        ));
    }
    let ciphertext_bytes = record
        .plain_size
        .checked_add(u64::from(record.chunk_count) * CIPHERTEXT_CHUNK_OVERHEAD as u64)
        .ok_or(AttachmentError::ContentTooLarge)?;
    Ok(AttachmentManifest {
        attachment_id: record.attachment_id.clone(),
        plain_size: record.plain_size,
        chunk_size: record.chunk_size,
        chunk_count: record.chunk_count,
        ciphertext_bytes,
        ciphertext_digest: record.ciphertext_digest.clone(),
    })
}
