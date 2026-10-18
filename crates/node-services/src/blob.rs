//! NODE-012~015：附件密文 blob 的上传会话、按序分块、发布与读取。
//!
//! 节点只保存**密文块**（不持文件密钥、不看明文摘要），因此附件与群正文一样是“客户端加密、
//! 节点只做公开元数据校验”。事务边界（TRANSACTIONS.md T4）：
//!
//! - begin：上传会话行 + 配额核算同一 IMMEDIATE 事务；
//! - put：分块登记行 + 块内容同一事务（**单库单事务**，没有“文件已写、DB 未登记”的双系统半写）；
//! - complete：整体摘要校验 + 对象发布 + 回执写入同一事务；
//! - read：只读快照，按成员窗口与发布状态鉴权。
//!
//! 已知限制（详见 docs/implementation/node.md 的 NODE-012~015 实现状态）：
//! 密文块存放在节点 SQLite（`blob_chunk_content`）而不是独立文件系统，因此 `storage_ref` 只是
//! 内容寻址引用；下载授权按“群成员窗口 + 发布状态”判定，尚无 API-004 的签名下载能力令牌。
use chat_protocol::{GroupId, ObjectId, OperationId};
use ed25519_dalek::{Signature, Signer};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::canonical::{
    BLOB_CHUNK_RECEIPT_DOMAIN, BLOB_PUBLISH_RECEIPT_DOMAIN, BLOB_SESSION_DOMAIN, bytes32,
    hex_decode, hex_encode, push_field, sha256, validate_bounded_id, validate_timestamp,
    verify_ed25519,
};
use crate::error::{NodeError, NodeResult};
use crate::group::{load_group, load_membership};
use crate::limits::{
    BlobPolicy, MAX_BLOB_ID_BYTES, MAX_BLOB_UPLOAD_ID_BYTES, MAX_DEVICE_ID_BYTES, MAX_GROUP_ID_BYTES,
};
use crate::services::{AuthenticatedDevice, NodeService, to_i64};

/// 公开的密文 blob 描述：只有密文规则，不含文件密钥或明文摘要。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicBlobDescriptor {
    /// 客户端分配的 blob 标识（PROTOCOL-001 object 文本）。
    pub blob_id: String,
    /// 附件所属群。
    pub group_id: String,
    /// 关联的已接受日志条目（作者必须是上传设备本人）。
    pub source_seq: u64,
    /// 声明密文总字节。
    pub ciphertext_bytes: u64,
    /// 声明单块字节（最后一块可以更短）。
    pub chunk_bytes: u32,
    /// 声明块数。
    pub chunk_count: u32,
}

impl PublicBlobDescriptor {
    /// 形状、上限与边界一致性校验；不检查授权与配额（那是 service 的职责）。
    pub fn validate_shape(&self, policy: &BlobPolicy) -> NodeResult<()> {
        validate_bounded_id("blob id", &self.blob_id, MAX_BLOB_ID_BYTES)?;
        ObjectId::parse(&self.blob_id).map_err(|_| NodeError::InvalidRequest("blob id"))?;
        validate_bounded_id("blob group id", &self.group_id, MAX_GROUP_ID_BYTES)?;
        GroupId::parse(&self.group_id).map_err(|_| NodeError::InvalidRequest("blob group id"))?;
        if self.source_seq == 0 {
            return Err(NodeError::InvalidRequest("blob source seq"));
        }
        validate_timestamp("blob source seq", self.source_seq)?;
        if self.ciphertext_bytes == 0 || self.ciphertext_bytes > policy.max_object_bytes {
            return Err(NodeError::ContentTooLarge);
        }
        if self.chunk_bytes == 0 || usize::try_from(self.chunk_bytes).unwrap_or(usize::MAX) > policy.max_chunk_bytes {
            return Err(NodeError::ContentTooLarge);
        }
        if self.chunk_count == 0 || self.chunk_count > policy.max_chunk_count {
            return Err(NodeError::ContentTooLarge);
        }
        // 边界一致性：前 chunk_count-1 块满块，最后一块非空且不超过声明块大小。
        let chunk = u64::from(self.chunk_bytes);
        let last_index = u64::from(self.chunk_count) - 1;
        let minimum = last_index
            .checked_mul(chunk)
            .and_then(|value| value.checked_add(1))
            .ok_or(NodeError::InvalidRequest("blob descriptor boundaries"))?;
        let maximum = u64::from(self.chunk_count)
            .checked_mul(chunk)
            .ok_or(NodeError::InvalidRequest("blob descriptor boundaries"))?;
        if self.ciphertext_bytes < minimum || self.ciphertext_bytes > maximum {
            return Err(NodeError::InvalidRequest("blob descriptor boundaries"));
        }
        Ok(())
    }
}

/// 节点签发的上传会话：受限上传条款（blob/群/关联条目/块参数/期限/节点身份）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadSession {
    pub upload_id: String,
    pub blob_id: String,
    pub group_id: String,
    pub source_seq: u64,
    pub owner_device_id: String,
    pub ciphertext_bytes: u64,
    pub chunk_bytes: u32,
    pub chunk_count: u32,
    pub created_at: u64,
    pub expires_at: u64,
    pub node_id: String,
    pub node_public_key: String,
    pub signature: String,
}

impl UploadSession {
    /// 会话签名原文。
    pub fn signing_bytes(&self) -> NodeResult<Vec<u8>> {
        let node_public_key = hex_decode::<32>("node public key", &self.node_public_key)?;
        validate_bounded_id("upload id", &self.upload_id, MAX_BLOB_UPLOAD_ID_BYTES)?;
        validate_bounded_id("blob id", &self.blob_id, MAX_BLOB_ID_BYTES)?;
        validate_bounded_id("group id", &self.group_id, MAX_GROUP_ID_BYTES)?;
        validate_bounded_id("owner device id", &self.owner_device_id, MAX_DEVICE_ID_BYTES)?;
        validate_timestamp("source seq", self.source_seq)?;
        validate_timestamp("created at", self.created_at)?;
        validate_timestamp("expires at", self.expires_at)?;
        let mut bytes = Vec::with_capacity(256);
        bytes.extend_from_slice(BLOB_SESSION_DOMAIN);
        push_field(&mut bytes, self.node_id.as_bytes());
        bytes.extend_from_slice(&node_public_key);
        push_field(&mut bytes, self.upload_id.as_bytes());
        push_field(&mut bytes, self.blob_id.as_bytes());
        push_field(&mut bytes, self.group_id.as_bytes());
        bytes.extend_from_slice(&self.source_seq.to_be_bytes());
        push_field(&mut bytes, self.owner_device_id.as_bytes());
        bytes.extend_from_slice(&self.ciphertext_bytes.to_be_bytes());
        bytes.extend_from_slice(&u64::from(self.chunk_bytes).to_be_bytes());
        bytes.extend_from_slice(&u64::from(self.chunk_count).to_be_bytes());
        bytes.extend_from_slice(&self.created_at.to_be_bytes());
        bytes.extend_from_slice(&self.expires_at.to_be_bytes());
        Ok(bytes)
    }

    /// 用固定节点公钥验证会话。
    pub fn verify(&self, expected_node_public_key: &[u8; 32]) -> NodeResult<()> {
        let node_public_key = hex_decode::<32>("node public key", &self.node_public_key)?;
        if &node_public_key != expected_node_public_key {
            return Err(NodeError::NodeIdentityMismatch);
        }
        let signature = hex_decode::<64>("upload session signature", &self.signature)?;
        verify_ed25519(
            expected_node_public_key,
            &self.signing_bytes()?,
            &signature,
            || NodeError::InvalidSignature,
        )
    }
}

/// 分块接收回执（同 index 同摘要重传返回同一份）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkReceipt {
    pub upload_id: String,
    pub chunk_index: u32,
    pub cipher_hash: String,
    pub byte_size: u64,
    pub received_chunks: u32,
    pub chunk_count: u32,
    pub node_id: String,
    pub node_public_key: String,
    pub signature: String,
}

impl ChunkReceipt {
    /// 回执签名原文。
    pub fn signing_bytes(&self) -> NodeResult<Vec<u8>> {
        let node_public_key = hex_decode::<32>("node public key", &self.node_public_key)?;
        let cipher_hash = hex_decode::<32>("chunk cipher hash", &self.cipher_hash)?;
        validate_bounded_id("upload id", &self.upload_id, MAX_BLOB_UPLOAD_ID_BYTES)?;
        let mut bytes = Vec::with_capacity(192);
        bytes.extend_from_slice(BLOB_CHUNK_RECEIPT_DOMAIN);
        push_field(&mut bytes, self.node_id.as_bytes());
        bytes.extend_from_slice(&node_public_key);
        push_field(&mut bytes, self.upload_id.as_bytes());
        bytes.extend_from_slice(&u64::from(self.chunk_index).to_be_bytes());
        bytes.extend_from_slice(&cipher_hash);
        bytes.extend_from_slice(&self.byte_size.to_be_bytes());
        bytes.extend_from_slice(&u64::from(self.received_chunks).to_be_bytes());
        bytes.extend_from_slice(&u64::from(self.chunk_count).to_be_bytes());
        Ok(bytes)
    }

    /// 用固定节点公钥验证回执。
    pub fn verify(&self, expected_node_public_key: &[u8; 32]) -> NodeResult<()> {
        let node_public_key = hex_decode::<32>("node public key", &self.node_public_key)?;
        if &node_public_key != expected_node_public_key {
            return Err(NodeError::NodeIdentityMismatch);
        }
        let signature = hex_decode::<64>("chunk receipt signature", &self.signature)?;
        verify_ed25519(
            expected_node_public_key,
            &self.signing_bytes()?,
            &signature,
            || NodeError::InvalidSignature,
        )
    }
}

/// 发布清单：块数 / 总密文字节 / 整体密文摘要（按块序拼接后的 SHA-256）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CipherManifest {
    pub chunk_count: u32,
    pub ciphertext_bytes: u64,
    /// 整体密文摘要（小写十六进制 32 字节）。
    pub ciphertext_digest: String,
}

impl CipherManifest {
    /// 形状与摘要格式校验。
    pub fn validate_shape(&self) -> NodeResult<[u8; 32]> {
        if self.chunk_count == 0 || self.ciphertext_bytes == 0 {
            return Err(NodeError::InvalidRequest("cipher manifest"));
        }
        validate_timestamp("cipher manifest bytes", self.ciphertext_bytes)?;
        hex_decode::<32>("ciphertext digest", &self.ciphertext_digest)
    }
}

/// 发布回执（重复 complete 返回同一份）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobReceipt {
    pub blob_id: String,
    pub upload_id: String,
    pub group_id: String,
    pub source_seq: u64,
    pub owner_device_id: String,
    pub ciphertext_bytes: u64,
    pub chunk_count: u32,
    pub ciphertext_digest: String,
    pub completed_at: u64,
    pub node_id: String,
    pub node_public_key: String,
    pub signature: String,
}

impl BlobReceipt {
    /// 回执签名原文。
    pub fn signing_bytes(&self) -> NodeResult<Vec<u8>> {
        let node_public_key = hex_decode::<32>("node public key", &self.node_public_key)?;
        let digest = hex_decode::<32>("ciphertext digest", &self.ciphertext_digest)?;
        validate_bounded_id("blob id", &self.blob_id, MAX_BLOB_ID_BYTES)?;
        validate_bounded_id("upload id", &self.upload_id, MAX_BLOB_UPLOAD_ID_BYTES)?;
        validate_bounded_id("group id", &self.group_id, MAX_GROUP_ID_BYTES)?;
        validate_bounded_id("owner device id", &self.owner_device_id, MAX_DEVICE_ID_BYTES)?;
        validate_timestamp("source seq", self.source_seq)?;
        validate_timestamp("completed at", self.completed_at)?;
        let mut bytes = Vec::with_capacity(256);
        bytes.extend_from_slice(BLOB_PUBLISH_RECEIPT_DOMAIN);
        push_field(&mut bytes, self.node_id.as_bytes());
        bytes.extend_from_slice(&node_public_key);
        push_field(&mut bytes, self.blob_id.as_bytes());
        push_field(&mut bytes, self.upload_id.as_bytes());
        push_field(&mut bytes, self.group_id.as_bytes());
        bytes.extend_from_slice(&self.source_seq.to_be_bytes());
        push_field(&mut bytes, self.owner_device_id.as_bytes());
        bytes.extend_from_slice(&self.ciphertext_bytes.to_be_bytes());
        bytes.extend_from_slice(&u64::from(self.chunk_count).to_be_bytes());
        bytes.extend_from_slice(&digest);
        bytes.extend_from_slice(&self.completed_at.to_be_bytes());
        Ok(bytes)
    }

    /// 用固定节点公钥验证回执。
    pub fn verify(&self, expected_node_public_key: &[u8; 32]) -> NodeResult<()> {
        let node_public_key = hex_decode::<32>("node public key", &self.node_public_key)?;
        if &node_public_key != expected_node_public_key {
            return Err(NodeError::NodeIdentityMismatch);
        }
        let signature = hex_decode::<64>("publish receipt signature", &self.signature)?;
        verify_ed25519(
            expected_node_public_key,
            &self.signing_bytes()?,
            &signature,
            || NodeError::InvalidSignature,
        )
    }
}

/// 读取返回的密文块（只有密文与公开元数据）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CipherChunk {
    pub blob_id: String,
    pub chunk_index: u32,
    pub chunk_count: u32,
    pub cipher_hash: String,
    pub bytes: Vec<u8>,
}
// ---------------------------------------------------------------- 存储投影

pub(crate) struct UploadRow {
    upload_id: String,
    blob_id: String,
    group_id: String,
    source_seq: u64,
    owner_device_id: String,
    ciphertext_bytes: u64,
    chunk_bytes: u32,
    chunk_count: u32,
    created_at: u64,
    expires_at: u64,
    state: String,
}

struct BlobRow {
    upload_id: String,
    group_id: String,
    source_seq: u64,
    owner_device_id: String,
    cipher_bytes: u64,
    chunk_count: u32,
    state: String,
}

fn load_upload(
    tx: &rusqlite::Transaction<'_>,
    upload_id: &str,
) -> NodeResult<Option<UploadRow>> {
    let row: Option<(String, String, String, i64, String, i64, i64, i64, i64, i64, String)> = tx
        .query_row(
            "SELECT upload_id, blob_id, group_id, source_seq, owner_device_id,
                    expected_cipher_bytes, chunk_bytes, expected_chunk_count, created_at,
                    expires_at, state
               FROM blob_uploads WHERE upload_id = ?1",
            [upload_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                ))
            },
        )
        .optional()?;
    row.map(
        |(
            upload_id,
            blob_id,
            group_id,
            source_seq,
            owner_device_id,
            ciphertext_bytes,
            chunk_bytes,
            chunk_count,
            created_at,
            expires_at,
            state,
        )| {
            Ok(UploadRow {
                upload_id,
                blob_id,
                group_id,
                source_seq: u64::try_from(source_seq).map_err(|_| NodeError::StorageUnavailable)?,
                owner_device_id,
                ciphertext_bytes: u64::try_from(ciphertext_bytes)
                    .map_err(|_| NodeError::StorageUnavailable)?,
                chunk_bytes: u32::try_from(chunk_bytes)
                    .map_err(|_| NodeError::StorageUnavailable)?,
                chunk_count: u32::try_from(chunk_count)
                    .map_err(|_| NodeError::StorageUnavailable)?,
                created_at: u64::try_from(created_at)
                    .map_err(|_| NodeError::StorageUnavailable)?,
                expires_at: u64::try_from(expires_at)
                    .map_err(|_| NodeError::StorageUnavailable)?,
                state,
            })
        },
    )
    .transpose()
}

fn load_blob(tx: &rusqlite::Transaction<'_>, blob_id: &str) -> NodeResult<Option<BlobRow>> {
    let row: Option<(String, String, i64, String, i64, i64, String)> = tx
        .query_row(
            "SELECT upload_id, group_id, source_seq, owner_device_id, cipher_bytes, chunk_count,
                    state
               FROM blob_objects WHERE blob_id = ?1",
            [blob_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;
    row.map(
        |(upload_id, group_id, source_seq, owner_device_id, cipher_bytes, chunk_count, state)| {
            Ok(BlobRow {
                upload_id,
                group_id,
                source_seq: u64::try_from(source_seq).map_err(|_| NodeError::StorageUnavailable)?,
                owner_device_id,
                cipher_bytes: u64::try_from(cipher_bytes)
                    .map_err(|_| NodeError::StorageUnavailable)?,
                chunk_count: u32::try_from(chunk_count)
                    .map_err(|_| NodeError::StorageUnavailable)?,
                state,
            })
        },
    )
    .transpose()
}

fn committed_chunks(tx: &rusqlite::Transaction<'_>, upload_id: &str) -> NodeResult<u32> {
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM blob_chunks WHERE upload_id = ?1",
        [upload_id],
        |row| row.get(0),
    )?;
    u32::try_from(count).map_err(|_| NodeError::StorageUnavailable)
}

/// 该序号的块“应当”有多少字节（最后一块可以更短；begin 已校验边界关系）。
fn expected_chunk_bytes(upload: &UploadRow, index: u32) -> NodeResult<usize> {
    if index >= upload.chunk_count {
        return Err(NodeError::InvalidRequest("chunk index is out of range"));
    }
    let last_index = u64::from(upload.chunk_count) - 1;
    let chunk = u64::from(upload.chunk_bytes);
    let size = if u64::from(index) == last_index {
        upload
            .ciphertext_bytes
            .saturating_sub(last_index.saturating_mul(chunk))
    } else {
        chunk
    };
    usize::try_from(size).map_err(|_| NodeError::StorageUnavailable)
}

/// 该设备当前的（在传预留, 已发布占用）密文字节。
fn device_usage(
    tx: &rusqlite::Transaction<'_>,
    device_id: &str,
    now: i64,
) -> NodeResult<(u64, u64)> {
    let reserved: i64 = tx.query_row(
        "SELECT COALESCE(SUM(expected_cipher_bytes), 0) FROM blob_uploads
          WHERE owner_device_id = ?1 AND state = 'uploading' AND expires_at > ?2",
        params![device_id, now],
        |row| row.get(0),
    )?;
    let used: i64 = tx.query_row(
        "SELECT COALESCE(SUM(cipher_bytes), 0) FROM blob_objects
          WHERE owner_device_id = ?1 AND state = 'published'",
        params![device_id],
        |row| row.get(0),
    )?;
    Ok((
        u64::try_from(reserved).map_err(|_| NodeError::StorageUnavailable)?,
        u64::try_from(used).map_err(|_| NodeError::StorageUnavailable)?,
    ))
}

/// 节点身份快照：事务外先取出签名材料，避免与事务借用冲突。
struct NodeSigner {
    node_id: String,
    node_public_key: [u8; 32],
    signing_key: ed25519_dalek::SigningKey,
}

impl NodeSigner {
    fn take(service: &NodeService) -> Self {
        Self {
            node_id: service.node_id().to_owned(),
            node_public_key: service.node_public_key(),
            signing_key: service.signing_key().clone(),
        }
    }

    fn session(&self, upload: &UploadRow) -> NodeResult<UploadSession> {
        let mut session = UploadSession {
            upload_id: upload.upload_id.clone(),
            blob_id: upload.blob_id.clone(),
            group_id: upload.group_id.clone(),
            source_seq: upload.source_seq,
            owner_device_id: upload.owner_device_id.clone(),
            ciphertext_bytes: upload.ciphertext_bytes,
            chunk_bytes: upload.chunk_bytes,
            chunk_count: upload.chunk_count,
            created_at: upload.created_at,
            expires_at: upload.expires_at,
            node_id: self.node_id.clone(),
            node_public_key: hex_encode(&self.node_public_key),
            signature: String::new(),
        };
        let signature: Signature = self.signing_key.sign(&session.signing_bytes()?);
        session.signature = hex_encode(&signature.to_bytes());
        Ok(session)
    }

    fn chunk_receipt(
        &self,
        upload: &UploadRow,
        index: u32,
        cipher_hash: &[u8; 32],
        byte_size: u64,
        received_chunks: u32,
    ) -> NodeResult<ChunkReceipt> {
        let mut receipt = ChunkReceipt {
            upload_id: upload.upload_id.clone(),
            chunk_index: index,
            cipher_hash: hex_encode(cipher_hash),
            byte_size,
            received_chunks,
            chunk_count: upload.chunk_count,
            node_id: self.node_id.clone(),
            node_public_key: hex_encode(&self.node_public_key),
            signature: String::new(),
        };
        let signature: Signature = self.signing_key.sign(&receipt.signing_bytes()?);
        receipt.signature = hex_encode(&signature.to_bytes());
        Ok(receipt)
    }

    #[allow(clippy::too_many_arguments)]
    fn publish_receipt(
        &self,
        upload: &UploadRow,
        digest: &[u8; 32],
        completed_at: u64,
    ) -> NodeResult<BlobReceipt> {
        let mut receipt = BlobReceipt {
            blob_id: upload.blob_id.clone(),
            upload_id: upload.upload_id.clone(),
            group_id: upload.group_id.clone(),
            source_seq: upload.source_seq,
            owner_device_id: upload.owner_device_id.clone(),
            ciphertext_bytes: upload.ciphertext_bytes,
            chunk_count: upload.chunk_count,
            ciphertext_digest: hex_encode(digest),
            completed_at,
            node_id: self.node_id.clone(),
            node_public_key: hex_encode(&self.node_public_key),
            signature: String::new(),
        };
        let signature: Signature = self.signing_key.sign(&receipt.signing_bytes()?);
        receipt.signature = hex_encode(&signature.to_bytes());
        Ok(receipt)
    }
}

fn session_matches_descriptor(upload: &UploadRow, descriptor: &PublicBlobDescriptor) -> bool {
    upload.blob_id == descriptor.blob_id
        && upload.group_id == descriptor.group_id
        && upload.source_seq == descriptor.source_seq
        && upload.ciphertext_bytes == descriptor.ciphertext_bytes
        && upload.chunk_bytes == descriptor.chunk_bytes
        && upload.chunk_count == descriptor.chunk_count
}

/// 随机 upload id（节点分配，客户端不能自选）。
fn allocate_upload_id() -> String {
    let mut identifier = [0_u8; 16];
    rand::fill(&mut identifier);
    format!("upl_{}", hex_encode(&identifier))
}
impl NodeService {
    /// NODE-012：创建只含密文规则的上传会话并预留配额（同 operation 幂等）。
    ///
    /// 授权关联：上传设备必须是该群当前有效成员，且 `source_seq` 指向一条作者正是
    /// 上传设备本人的已接受日志条目；匿名或无关联上传一律显式拒绝。
    pub fn begin_blob_upload(
        &mut self,
        descriptor: &PublicBlobDescriptor,
        operation_id: &str,
        actor: &AuthenticatedDevice,
        now: u64,
    ) -> NodeResult<UploadSession> {
        validate_timestamp("now", now)?;
        let policy = self.policy().blob.clone();
        descriptor.validate_shape(&policy)?;
        validate_bounded_id("operation id", operation_id, MAX_BLOB_ID_BYTES)?;
        OperationId::parse(operation_id).map_err(|_| NodeError::InvalidRequest("operation id"))?;
        self.require_active_device(actor)?;
        let signer = NodeSigner::take(self);
        let now_sql = to_i64("now", now)?;
        let expires_at = now
            .checked_add(policy.upload_ttl_seconds)
            .ok_or(NodeError::InvalidRequest("blob upload expiry"))?;
        validate_timestamp("blob upload expiry", expires_at)?;
        let source_seq_sql = to_i64("source seq", descriptor.source_seq)?;
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let group_row = load_group(&tx, &descriptor.group_id)?.ok_or(NodeError::GroupNotFound)?;
        if group_row.state != "active" {
            return Err(NodeError::PermissionDenied);
        }
        let membership = load_membership(
            &tx,
            &descriptor.group_id,
            &actor.device_id,
            group_row.last_seq,
        )?
        .ok_or(NodeError::PermissionDenied)?;
        if membership.1.is_some() {
            return Err(NodeError::PermissionDenied);
        }
        let source_author: Option<String> = tx
            .query_row(
                "SELECT author_device_id FROM group_log WHERE group_id = ?1 AND group_seq = ?2",
                params![descriptor.group_id, source_seq_sql],
                |row| row.get(0),
            )
            .optional()?;
        match source_author {
            Some(author) if author == actor.device_id => {}
            _ => return Err(NodeError::PermissionDenied),
        }
        // 幂等：同 (owner, operation) 同描述返回同一会话；相异描述显式冲突。
        let existing_id: Option<String> = tx
            .query_row(
                "SELECT upload_id FROM blob_uploads WHERE owner_device_id = ?1 AND operation_id = ?2",
                params![actor.device_id, operation_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing_id) = existing_id {
            let upload = load_upload(&tx, &existing_id)?.ok_or(NodeError::StorageUnavailable)?;
            if !session_matches_descriptor(&upload, descriptor) {
                return Err(NodeError::BlobUploadConflict);
            }
            let session = signer.session(&upload)?;
            tx.commit()?;
            return Ok(session);
        }
        // blob 标识不能被已发布对象或别的上传会话占用。
        let taken: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM blob_objects WHERE blob_id = ?1
                 UNION ALL SELECT 1 FROM blob_uploads WHERE blob_id = ?1
                 LIMIT 1",
                [&descriptor.blob_id],
                |row| row.get(0),
            )
            .optional()?;
        if taken.is_some() {
            return Err(NodeError::BlobUploadConflict);
        }
        // 配额：在传预留 + 已发布占用 + 本次声明字节必须落在设备上限内。
        let (reserved, used) = device_usage(&tx, &actor.device_id, now_sql)?;
        if reserved
            .saturating_add(used)
            .saturating_add(descriptor.ciphertext_bytes)
            > policy.max_bytes_per_device
        {
            return Err(NodeError::QuotaExceeded);
        }
        let upload_id = allocate_upload_id();
        tx.execute(
            "INSERT INTO blob_uploads
                 (upload_id, owner_device_id, operation_id, expected_cipher_bytes,
                  expected_chunk_count, expires_at, state, blob_id, group_id, source_seq,
                  chunk_bytes, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'uploading', ?7, ?8, ?9, ?10, ?11)",
            params![
                upload_id,
                actor.device_id,
                operation_id,
                to_i64("ciphertext bytes", descriptor.ciphertext_bytes)?,
                i64::from(descriptor.chunk_count),
                to_i64("expires at", expires_at)?,
                descriptor.blob_id,
                descriptor.group_id,
                source_seq_sql,
                i64::from(descriptor.chunk_bytes),
                now_sql
            ],
        )?;
        let upload = load_upload(&tx, &upload_id)?.ok_or(NodeError::StorageUnavailable)?;
        let session = signer.session(&upload)?;
        tx.commit()?;
        Ok(session)
    }

    /// NODE-013：按序写入一块密文（同 index 同摘要幂等，异摘要/乱序/越界显式拒绝）。
    pub fn put_blob_chunk(
        &mut self,
        upload_id: &str,
        index: u32,
        bytes: &[u8],
        actor: &AuthenticatedDevice,
        now: u64,
    ) -> NodeResult<ChunkReceipt> {
        validate_timestamp("now", now)?;
        validate_bounded_id("upload id", upload_id, MAX_BLOB_UPLOAD_ID_BYTES)?;
        self.require_active_device(actor)?;
        if bytes.is_empty() {
            return Err(NodeError::InvalidRequest("cipher chunk is empty"));
        }
        let policy = self.policy().blob.clone();
        let cipher_hash = sha256(bytes);
        let byte_size = u64::try_from(bytes.len()).map_err(|_| NodeError::PayloadTooLarge)?;
        let signer = NodeSigner::take(self);
        let index_sql = to_i64("chunk index", u64::from(index))?;
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let upload = load_upload(&tx, upload_id)?.ok_or(NodeError::UploadNotFound)?;
        if upload.owner_device_id != actor.device_id {
            return Err(NodeError::PermissionDenied);
        }
        if index >= upload.chunk_count {
            return Err(NodeError::InvalidRequest("chunk index is out of range"));
        }
        // 已落库的同一块：同字节（同摘要同长度）幂等返回，相异字节冲突。
        let stored: Option<(Vec<u8>, i64)> = tx
            .query_row(
                "SELECT cipher_hash, byte_size FROM blob_chunks
                  WHERE upload_id = ?1 AND chunk_index = ?2",
                params![upload_id, index_sql],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((stored_hash, stored_size)) = stored {
            let stored_hash = bytes32("chunk hash", stored_hash)?;
            let stored_size =
                u64::try_from(stored_size).map_err(|_| NodeError::StorageUnavailable)?;
            if stored_hash != cipher_hash || stored_size != byte_size {
                return Err(NodeError::ChunkConflict);
            }
            let committed = committed_chunks(&tx, upload_id)?;
            let receipt = signer.chunk_receipt(&upload, index, &cipher_hash, stored_size, committed)?;
            tx.commit()?;
            return Ok(receipt);
        }
        // 会话必须仍在有效期内且未结束。
        if upload.state != "uploading" || now >= upload.expires_at {
            return Err(NodeError::UploadExpired);
        }
        // 尺寸：超过策略/声明块大小是内容越界；与声明边界不符是清单不一致。
        let declared_chunk = usize::try_from(upload.chunk_bytes).unwrap_or(usize::MAX);
        if bytes.len() > policy.max_chunk_bytes || bytes.len() > declared_chunk {
            return Err(NodeError::ContentTooLarge);
        }
        let expected = expected_chunk_bytes(&upload, index)?;
        if bytes.len() != expected {
            return Err(NodeError::ManifestMismatch);
        }
        // 必须按序：下一个待写序号 = 已提交块数。
        let committed = committed_chunks(&tx, upload_id)?;
        if index != committed {
            return Err(NodeError::ChunkOutOfOrder);
        }
        let storage_ref = format!("sha256:{}", hex_encode(&cipher_hash));
        tx.execute(
            "INSERT INTO blob_chunks
                 (upload_id, chunk_index, cipher_hash, byte_size, storage_ref, state)
             VALUES (?1, ?2, ?3, ?4, ?5, 'committed')",
            params![
                upload_id,
                index_sql,
                cipher_hash.as_slice(),
                to_i64("byte size", byte_size)?,
                storage_ref
            ],
        )?;
        tx.execute(
            "INSERT INTO blob_chunk_content (upload_id, chunk_index, ciphertext)
             VALUES (?1, ?2, ?3)",
            params![upload_id, index_sql, bytes],
        )?;
        let receipt = signer.chunk_receipt(&upload, index, &cipher_hash, byte_size, index + 1)?;
        tx.commit()?;
        Ok(receipt)
    }
    /// NODE-014：校验块数/总长/整体摘要后原子发布（重复 complete 返回同一回执）。
    pub fn complete_blob_upload(
        &mut self,
        upload_id: &str,
        manifest: &CipherManifest,
        actor: &AuthenticatedDevice,
        now: u64,
    ) -> NodeResult<BlobReceipt> {
        validate_timestamp("now", now)?;
        validate_bounded_id("upload id", upload_id, MAX_BLOB_UPLOAD_ID_BYTES)?;
        let expected_digest = manifest.validate_shape()?;
        self.require_active_device(actor)?;
        let policy = self.policy().blob.clone();
        let signer = NodeSigner::take(self);
        let now_sql = to_i64("now", now)?;
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let upload = load_upload(&tx, upload_id)?.ok_or(NodeError::UploadNotFound)?;
        if upload.owner_device_id != actor.device_id {
            return Err(NodeError::PermissionDenied);
        }
        // 幂等：已发布时只返回持久回执；清单必须仍然一致。
        if upload.state == "complete" {
            let stored: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT receipt FROM blob_receipts WHERE blob_id = ?1",
                    [&upload.blob_id],
                    |row| row.get(0),
                )
                .optional()?;
            let stored = stored.ok_or(NodeError::StorageUnavailable)?;
            let receipt: BlobReceipt = serde_json::from_slice(&stored)?;
            receipt.verify(&signer.node_public_key)?;
            if receipt.chunk_count != manifest.chunk_count
                || receipt.ciphertext_bytes != manifest.ciphertext_bytes
                || receipt.ciphertext_digest != manifest.ciphertext_digest
            {
                return Err(NodeError::ManifestMismatch);
            }
            tx.commit()?;
            return Ok(receipt);
        }
        if upload.state != "uploading" || now >= upload.expires_at {
            return Err(NodeError::UploadExpired);
        }
        if manifest.chunk_count != upload.chunk_count
            || manifest.ciphertext_bytes != upload.ciphertext_bytes
        {
            return Err(NodeError::ManifestMismatch);
        }
        // 缺块（含缺末块）：已提交块数必须等于声明块数。
        let committed = committed_chunks(&tx, upload_id)?;
        if committed != upload.chunk_count {
            return Err(NodeError::MissingChunks);
        }
        // 逐块复核（序号连续、摘要与长度自洽）并累计整体摘要。
        let mut hasher = Sha256::new();
        let mut total: u64 = 0;
        {
            let mut statement = tx.prepare(
                "SELECT c.chunk_index, c.cipher_hash, c.byte_size, k.ciphertext
                   FROM blob_chunks c
                   JOIN blob_chunk_content k
                     ON k.upload_id = c.upload_id AND k.chunk_index = c.chunk_index
                  WHERE c.upload_id = ?1
                  ORDER BY c.chunk_index",
            )?;
            let mut rows = statement.query([upload_id])?;
            let mut expected_index: u32 = 0;
            while let Some(row) = rows.next()? {
                let index =
                    u32::try_from(row.get::<_, i64>(0)?).map_err(|_| NodeError::StorageUnavailable)?;
                let stored_hash = bytes32("chunk hash", row.get(1)?)?;
                let byte_size =
                    u64::try_from(row.get::<_, i64>(2)?).map_err(|_| NodeError::StorageUnavailable)?;
                let ciphertext: Vec<u8> = row.get(3)?;
                if index != expected_index {
                    return Err(NodeError::StorageUnavailable);
                }
                if sha256(&ciphertext) != stored_hash || byte_size != ciphertext.len() as u64 {
                    return Err(NodeError::StorageUnavailable);
                }
                hasher.update(&ciphertext);
                total = total
                    .checked_add(byte_size)
                    .ok_or(NodeError::StorageUnavailable)?;
                expected_index += 1;
            }
        }
        if total != upload.ciphertext_bytes {
            return Err(NodeError::ManifestMismatch);
        }
        let digest: [u8; 32] = hasher.finalize().into();
        if digest != expected_digest {
            return Err(NodeError::ManifestMismatch);
        }
        // 配额：把本次上传的在传预留转成已发布占用，仍不得超过设备上限。
        let (reserved, used) = device_usage(&tx, &actor.device_id, now_sql)?;
        let reserved_after = reserved.saturating_sub(upload.ciphertext_bytes);
        if reserved_after
            .saturating_add(used)
            .saturating_add(upload.ciphertext_bytes)
            > policy.max_bytes_per_device
        {
            return Err(NodeError::QuotaExceeded);
        }
        tx.execute(
            "INSERT INTO blob_objects
                 (blob_id, upload_id, cipher_manifest, owner_device_id, state, group_id,
                  source_seq, chunk_count, cipher_bytes)
             VALUES (?1, ?2, ?3, ?4, 'published', ?5, ?6, ?7, ?8)",
            params![
                upload.blob_id,
                upload_id,
                serde_json::to_vec(manifest)?,
                actor.device_id,
                upload.group_id,
                to_i64("source seq", upload.source_seq)?,
                i64::from(upload.chunk_count),
                to_i64("ciphertext bytes", upload.ciphertext_bytes)?
            ],
        )?;
        tx.execute(
            "UPDATE blob_uploads SET state = 'complete' WHERE upload_id = ?1",
            [upload_id],
        )?;
        let receipt = signer.publish_receipt(&upload, &digest, now)?;
        tx.execute(
            "INSERT INTO blob_receipts (blob_id, upload_id, completed_at, receipt)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                receipt.blob_id,
                upload_id,
                now_sql,
                serde_json::to_vec(&receipt)?
            ],
        )?;
        tx.commit()?;
        Ok(receipt)
    }

    /// NODE-015：只向受限持有人返回密文块（未发布/越权/越界都显式拒绝）。
    pub fn read_blob_chunk(
        &mut self,
        blob_id: &str,
        index: u32,
        actor: &AuthenticatedDevice,
        now: u64,
    ) -> NodeResult<CipherChunk> {
        validate_timestamp("now", now)?;
        validate_bounded_id("blob id", blob_id, MAX_BLOB_ID_BYTES)?;
        ObjectId::parse(blob_id).map_err(|_| NodeError::InvalidRequest("blob id"))?;
        self.require_active_device(actor)?;
        let tx = self.connection_mut().transaction()?;
        let blob = load_blob(&tx, blob_id)?.ok_or(NodeError::BlobNotFound)?;
        if blob.state != "published" {
            return Err(NodeError::BlobNotFound);
        }
        // 已发布对象必须带非空密文与至少一块；否则是节点侧损坏，显式失败。
        if blob.cipher_bytes == 0 || blob.chunk_count == 0 {
            return Err(NodeError::StorageUnavailable);
        }
        // 下载授权：上传设备本人恒可读；其余设备必须是“附件存在时已在群内”的当前成员。
        if blob.owner_device_id != actor.device_id {
            let group_row = load_group(&tx, &blob.group_id)?.ok_or(NodeError::BlobNotFound)?;
            let membership = load_membership(
                &tx,
                &blob.group_id,
                &actor.device_id,
                group_row.last_seq,
            )?
            .ok_or(NodeError::HistoryNotAuthorized)?;
            if membership.1.is_some() {
                return Err(NodeError::HistoryNotAuthorized);
            }
            if membership.0 > blob.source_seq {
                // 新成员不能因为“现在在群里”就拿到附件之前的旧密文。
                return Err(NodeError::HistoryNotAuthorized);
            }
        }
        if index >= blob.chunk_count {
            return Err(NodeError::InvalidRequest("chunk index is out of range"));
        }
        let index_sql = to_i64("chunk index", u64::from(index))?;
        let stored: Option<(Vec<u8>, i64)> = tx
            .query_row(
                "SELECT cipher_hash, byte_size FROM blob_chunks
                  WHERE upload_id = ?1 AND chunk_index = ?2",
                params![blob.upload_id, index_sql],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (stored_hash, stored_size) = stored.ok_or(NodeError::BlobNotFound)?;
        let stored_hash = bytes32("chunk hash", stored_hash)?;
        let stored_size = u64::try_from(stored_size).map_err(|_| NodeError::StorageUnavailable)?;
        let ciphertext: Option<Vec<u8>> = tx
            .query_row(
                "SELECT ciphertext FROM blob_chunk_content
                  WHERE upload_id = ?1 AND chunk_index = ?2",
                params![blob.upload_id, index_sql],
                |row| row.get(0),
            )
            .optional()?;
        let bytes = ciphertext.ok_or(NodeError::BlobNotFound)?;
        if sha256(&bytes) != stored_hash || stored_size != bytes.len() as u64 {
            return Err(NodeError::StorageUnavailable);
        }
        tx.commit()?;
        Ok(CipherChunk {
            blob_id: blob_id.to_owned(),
            chunk_index: index,
            chunk_count: blob.chunk_count,
            cipher_hash: hex_encode(&stored_hash),
            bytes,
        })
    }
}