//! 附件竖直链路的 UniFFI 对象（ATTACHMENTS-001/002/003/005/006/007）。
//!
//! 只做薄封装：分块加密、摘要、状态白名单与节点编排都在 chat_core::attachments /
//! chat_core::attachment_node；本层不加密、不复制密钥、不重写状态机。
//!
//! 两种后端（构造方式决定）：
//!
//! - `AttachmentFfi::new(cache_root)`：**仅内存**的附件缓存对象（旧契约）。导入/状态/导出/
//!   取消可用；`upload`/`download` 显式 `INVALID_STATE_TRANSITION`，因为没有持久记录与
//!   身份库（绝不伪造上传成功）；
//! - `AttachmentFfi::open(database_path, master_key_hex, cache_root)`：**持久**后端
//!   （正式加密库 + 缓存目录）。附件状态真实落入 attachments 表（imported → uploading →
//!   available），上传/下载都走真节点；设备签名钥由库里的**持久身份**派生，本层不接受
//!   任何种子/口令入参。
//!
//! 已知限制：FileKey 只在进程内（E2EE SecretDescriptor 未实现），因此**另一个进程/实例**
//! 下载会显式 `ATTACHMENT_KEY_UNAVAILABLE`；本层不会返回未经验证的明文。
use std::sync::{Arc, Mutex};

use chat_core::attachment_node::{
    AttachmentNodeConfig, download_attachment_from_node, open_persisted_attachments,
    upload_attachment_to_node,
};
use chat_core::attachments::{
    AttachmentDescriptor, AttachmentError, AttachmentStatus, AttachmentStore, FileToken,
    PersistedAttachmentStore, StoredAttachmentRecord,
};

use crate::local_store::FfiError;

fn failure(error: AttachmentError) -> FfiError {
    FfiError::Store {
        code: error.code().to_owned(),
        reason: error.to_string(),
    }
}

fn poisoned(what: &str) -> FfiError {
    FfiError::Store {
        code: "INTERNAL_FAILURE".to_owned(),
        reason: format!("{what} is poisoned"),
    }
}

fn not_persisted() -> FfiError {
    FfiError::Store {
        code: "INVALID_STATE_TRANSITION".to_owned(),
        reason: "upload/download need the persisted attachment store (use AttachmentFfi.open)"
            .to_owned(),
    }
}

/// 附件投影（不含 FileKey）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AttachmentView {
    /// 本地附件 ID（att- 前缀）。
    pub attachment_id: String,
    /// 明文总字节数。
    pub plain_size: u64,
    /// 分块大小（字节）。
    pub chunk_size: u32,
    /// 分块总数。
    pub chunk_count: u32,
    /// 明文 SHA-256（hex）；只用于本地导出校验与 E2EE 内层。
    pub content_digest: String,
    /// 文件名展示值（只作展示，不参与路径拼接）。
    pub display_name: String,
    /// 状态码：imported / uploading / available / downloading / failed / cancelled。
    pub state: String,
    /// 本地缓存中实际存在的块数。
    pub available_chunks: u32,
    /// 已传给节点的块数（失败后保留；不是协议确认）。
    pub transferred_chunks: u32,
    /// 是否已请求取消。
    pub cancel_requested: bool,
}

/// 下载回执（不含正文/密钥）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AttachmentDownloadView {
    /// 本地附件 ID。
    pub attachment_id: String,
    /// 写出的明文字节数。
    pub written_bytes: u64,
    /// 明文 SHA-256（hex，已由内核按本地记录复核）。
    pub content_digest: String,
}

fn view_from_descriptor(
    descriptor: &AttachmentDescriptor,
    status: &AttachmentStatus,
) -> AttachmentView {
    AttachmentView {
        attachment_id: descriptor.attachment_id.clone(),
        plain_size: descriptor.plain_size,
        chunk_size: descriptor.chunk_size,
        chunk_count: descriptor.chunk_count,
        content_digest: descriptor.content_digest.clone(),
        display_name: descriptor.display_name.clone(),
        state: status.state.code().to_owned(),
        available_chunks: status.available_chunks,
        transferred_chunks: status.transferred_chunks,
        cancel_requested: status.cancel_requested,
    }
}

fn view_from_record(record: &StoredAttachmentRecord, status: &AttachmentStatus) -> AttachmentView {
    AttachmentView {
        attachment_id: record.attachment_id.clone(),
        plain_size: record.plain_size,
        chunk_size: record.chunk_size,
        chunk_count: record.chunk_count,
        content_digest: record.content_digest.clone(),
        display_name: record.display_name.clone(),
        state: status.state.code().to_owned(),
        available_chunks: status.available_chunks,
        transferred_chunks: status.transferred_chunks,
        cancel_requested: status.cancel_requested,
    }
}

/// 后端：内存缓存（旧契约）或持久入口（真实上传/下载）。
enum Backend {
    Memory(AttachmentStore),
    Persisted(PersistedAttachmentStore),
}

/// 附件 FFI 门面。
#[derive(uniffi::Object)]
pub struct AttachmentFfi {
    backend: Mutex<Backend>,
    /// 仅内存后端使用：本进程导入过的描述符（持久后端直接读记录）。
    descriptors: Mutex<Vec<AttachmentDescriptor>>,
}

impl AttachmentFfi {
    fn backend(&self) -> Result<std::sync::MutexGuard<'_, Backend>, FfiError> {
        self.backend
            .lock()
            .map_err(|_| poisoned("attachment backend"))
    }

    fn descriptors(&self) -> Result<std::sync::MutexGuard<'_, Vec<AttachmentDescriptor>>, FfiError> {
        self.descriptors
            .lock()
            .map_err(|_| poisoned("attachment records"))
    }

    fn remember(&self, descriptor: &AttachmentDescriptor) -> Result<(), FfiError> {
        let mut descriptors = self.descriptors()?;
        descriptors.retain(|existing| existing.attachment_id != descriptor.attachment_id);
        descriptors.push(descriptor.clone());
        Ok(())
    }

    fn memory_descriptor(&self, attachment_id: &str) -> Result<AttachmentDescriptor, FfiError> {
        self.descriptors()?
            .iter()
            .find(|existing| existing.attachment_id == attachment_id)
            .cloned()
            .ok_or_else(|| FfiError::Store {
                code: AttachmentError::AttachmentNotFound.code().to_owned(),
                reason: "attachment not found in this process".to_owned(),
            })
    }

    fn node_config(
        endpoint_url: String,
        node_id: String,
        node_public_key_hex: Option<String>,
        group_id: String,
        source_seq: u64,
    ) -> Result<AttachmentNodeConfig, FfiError> {
        AttachmentNodeConfig::new(
            endpoint_url,
            node_id,
            node_public_key_hex.as_deref(),
            group_id,
            source_seq,
        )
        .map_err(failure)
    }
}

#[uniffi::export]
impl AttachmentFfi {
    /// 以 cache_root 作为加密分块缓存目录（**仅内存**后端；旧契约，不含上传/下载）。
    #[uniffi::constructor]
    pub fn new(cache_root: String) -> Result<Arc<Self>, FfiError> {
        let store = AttachmentStore::new(cache_root).map_err(failure)?;
        Ok(Arc::new(Self {
            backend: Mutex::new(Backend::Memory(store)),
            descriptors: Mutex::new(Vec::new()),
        }))
    }

    /// 持久后端：正式加密库（attachments 表 + 持久身份）+ 分块缓存目录。
    ///
    /// 数据目录必须已存在，主密钥是 64 位十六进制；同一库是独占单写者，
    /// 调用前请先关闭其它持有该库的对象（例如 `ConversationCore` / `LocalStoreFfi`）。
    #[uniffi::constructor]
    pub fn open(
        database_path: String,
        master_key_hex: String,
        cache_root: String,
    ) -> Result<Arc<Self>, FfiError> {
        let store = open_persisted_attachments(&database_path, &master_key_hex, &cache_root)
            .map_err(failure)?;
        Ok(Arc::new(Self {
            backend: Mutex::new(Backend::Persisted(store)),
            descriptors: Mutex::new(Vec::new()),
        }))
    }

    /// ATTACHMENTS-001：流式导入并加密缓存，返回描述符与真实状态。
    pub fn import_file(&self, path: String, display_name: String) -> Result<AttachmentView, FfiError> {
        let guard = self.backend()?;
        match &*guard {
            Backend::Memory(store) => {
                let descriptor = store
                    .import_file(&FileToken::from_local_path(path), &display_name)
                    .map_err(failure)?;
                let status = store
                    .attachment_status(&descriptor.attachment_id)
                    .map_err(failure)?;
                drop(guard);
                self.remember(&descriptor)?;
                Ok(view_from_descriptor(&descriptor, &status))
            }
            Backend::Persisted(store) => {
                let descriptor = store
                    .import_file(&FileToken::from_local_path(path), &display_name)
                    .map_err(failure)?;
                let record = store
                    .attachment_record(&descriptor.attachment_id)
                    .map_err(failure)?
                    .ok_or_else(|| FfiError::Store {
                        code: AttachmentError::AttachmentNotFound.code().to_owned(),
                        reason: "attachment record was not persisted".to_owned(),
                    })?;
                let status = store
                    .attachment_status(&descriptor.attachment_id)
                    .map_err(failure)?;
                Ok(view_from_record(&record, &status))
            }
        }
    }

    /// ATTACHMENTS-002：状态查询（持久后端读真实记录；内存后端读本进程描述符）。
    pub fn status(&self, attachment_id: String) -> Result<AttachmentView, FfiError> {
        let guard = self.backend()?;
        match &*guard {
            Backend::Memory(store) => {
                let descriptor = self.memory_descriptor(&attachment_id)?;
                let status = store.attachment_status(&attachment_id).map_err(failure)?;
                Ok(view_from_descriptor(&descriptor, &status))
            }
            Backend::Persisted(store) => {
                let record = store
                    .attachment_record(&attachment_id)
                    .map_err(failure)?
                    .ok_or_else(|| FfiError::Store {
                        code: AttachmentError::AttachmentNotFound.code().to_owned(),
                        reason: "attachment record does not exist".to_owned(),
                    })?;
                let status = store.attachment_status(&attachment_id).map_err(failure)?;
                Ok(view_from_record(&record, &status))
            }
        }
    }

    /// 批量读附件显示信息（消息附件卡片用）：只返回存在且可读的记录，未知 ID 跳过并计数。
    ///
    /// 返回 `AttachmentView` 列表；不会解密内容，也不会声称“已上传/可下载”。
    pub fn records(&self, attachment_ids: Vec<String>) -> Result<Vec<AttachmentView>, FfiError> {
        let guard = self.backend()?;
        let mut views = Vec::new();
        for attachment_id in attachment_ids.into_iter().take(32) {
            let view = match &*guard {
                Backend::Memory(store) => {
                    let descriptor = match self.memory_descriptor(&attachment_id) {
                        Ok(descriptor) => descriptor,
                        Err(_) => continue,
                    };
                    let status = match store.attachment_status(&attachment_id) {
                        Ok(status) => status,
                        Err(_) => continue,
                    };
                    view_from_descriptor(&descriptor, &status)
                }
                Backend::Persisted(store) => {
                    let record = match store.attachment_record(&attachment_id) {
                        Ok(Some(record)) => record,
                        Ok(None) => continue,
                        Err(error) => return Err(failure(error)),
                    };
                    let status = match store.attachment_status(&attachment_id) {
                        Ok(status) => status,
                        Err(_) => continue,
                    };
                    view_from_record(&record, &status)
                }
            };
            views.push(view);
        }
        Ok(views)
    }

    /// ATTACHMENTS-006：解密导出到目标路径，返回写出的明文字节数。
    ///
    /// 没有进程内 FileKey（例如重启后）时显式 `ATTACHMENT_KEY_UNAVAILABLE`。
    pub fn export_file(&self, attachment_id: String, target_path: String) -> Result<u64, FfiError> {
        let guard = self.backend()?;
        let token = FileToken::from_local_path(target_path);
        match &*guard {
            Backend::Memory(store) => store
                .export_attachment(&attachment_id, &token)
                .map(|receipt| receipt.written_bytes)
                .map_err(failure),
            Backend::Persisted(store) => store
                .export_attachment(&attachment_id, &token)
                .map(|receipt| receipt.written_bytes)
                .map_err(failure),
        }
    }

    /// ATTACHMENTS-007：请求取消（保留缓存与进度；已完成的传输拒绝取消）。
    pub fn cancel(&self, attachment_id: String) -> Result<AttachmentView, FfiError> {
        let guard = self.backend()?;
        match &*guard {
            Backend::Memory(store) => {
                let descriptor = self.memory_descriptor(&attachment_id)?;
                let status = store.cancel_transfer(&attachment_id).map_err(failure)?;
                Ok(view_from_descriptor(&descriptor, &status))
            }
            Backend::Persisted(store) => {
                let status = store.cancel_transfer(&attachment_id).map_err(failure)?;
                let record = store
                    .attachment_record(&attachment_id)
                    .map_err(failure)?
                    .ok_or_else(|| FfiError::Store {
                        code: AttachmentError::AttachmentNotFound.code().to_owned(),
                        reason: "attachment record does not exist".to_owned(),
                    })?;
                Ok(view_from_record(&record, &status))
            }
        }
    }

    /// ATTACHMENTS-003：把本地加密缓存的分块密文按序上传到真节点。
    ///
    /// - 设备签名钥由库里的**持久身份**派生（本层不接受种子/口令）；
    /// - `group_id` 是 PROTOCOL-001 群文本，`source_seq` 是本设备已接受的日志序号
    ///   （NODE-012 的上传授权关联）；
    /// - 成功后状态真实迁移到 `available`；失败保留缓存与进度、状态停在 `uploading`
    ///   可重试；已 `available` 时重复上传是**幂等无操作**（零发送）。
    #[allow(clippy::too_many_arguments)]
    pub fn upload(
        &self,
        attachment_id: String,
        endpoint_url: String,
        node_id: String,
        node_public_key_hex: Option<String>,
        group_id: String,
        source_seq: u64,
    ) -> Result<AttachmentView, FfiError> {
        let config =
            Self::node_config(endpoint_url, node_id, node_public_key_hex, group_id, source_seq)?;
        let guard = self.backend()?;
        let Backend::Persisted(store) = &*guard else {
            return Err(not_persisted());
        };
        let status = upload_attachment_to_node(store, &attachment_id, &config).map_err(failure)?;
        let record = store
            .attachment_record(&attachment_id)
            .map_err(failure)?
            .ok_or_else(|| FfiError::Store {
                code: AttachmentError::AttachmentNotFound.code().to_owned(),
                reason: "attachment record does not exist".to_owned(),
            })?;
        Ok(view_from_record(&record, &status))
    }

    /// ATTACHMENTS-005：从真节点取回分块密文、认证解密并写出目标文件。
    ///
    /// 清单来自本地持久记录（块数/块大小/明文大小/密文摘要），不一致的远端数据在任何
    /// 写盘之前被拒绝；失败不产生未验证文件。
    pub fn download(
        &self,
        blob_id: String,
        target_path: String,
        endpoint_url: String,
        node_id: String,
        node_public_key_hex: Option<String>,
        group_id: String,
        source_seq: u64,
    ) -> Result<AttachmentDownloadView, FfiError> {
        let config =
            Self::node_config(endpoint_url, node_id, node_public_key_hex, group_id, source_seq)?;
        let guard = self.backend()?;
        let Backend::Persisted(store) = &*guard else {
            return Err(not_persisted());
        };
        let receipt = download_attachment_from_node(
            store,
            &blob_id,
            &FileToken::from_local_path(target_path),
            &config,
        )
        .map_err(failure)?;
        Ok(AttachmentDownloadView {
            attachment_id: receipt.attachment_id,
            written_bytes: receipt.written_bytes,
            content_digest: receipt.content_digest,
        })
    }
}
