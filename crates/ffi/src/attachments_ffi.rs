//! 附件竖直链路的 UniFFI 对象（ATTACHMENTS-001/002/006 本地部分）。
//!
//! 只做薄封装：真正的分块加密、摘要与临时缓存都在 chat_core::attachments。
//! 网络上传/下载（NODE-012~015）尚未接线，状态机里的 uploading/available 只是占位。
use std::sync::{Arc, Mutex};

use chat_core::attachments::{
    AttachmentDescriptor, AttachmentError, AttachmentState, AttachmentStatus, AttachmentStore,
    FileToken,
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
    /// 状态码：imported / cancelled 等。
    pub state: String,
    /// 本地缓存中实际存在的块数。
    pub available_chunks: u32,
    /// 是否已请求取消。
    pub cancel_requested: bool,
}

fn view(descriptor: &AttachmentDescriptor, status: Option<&AttachmentStatus>) -> AttachmentView {
    AttachmentView {
        attachment_id: descriptor.attachment_id.clone(),
        plain_size: descriptor.plain_size,
        chunk_size: descriptor.chunk_size,
        chunk_count: descriptor.chunk_count,
        content_digest: descriptor.content_digest.clone(),
        display_name: descriptor.display_name.clone(),
        state: status
            .map(|value| value.state.code().to_owned())
            .unwrap_or_else(|| AttachmentState::Imported.code().to_owned()),
        available_chunks: status.map(|value| value.available_chunks).unwrap_or(0),
        cancel_requested: status.map(|value| value.cancel_requested).unwrap_or(false),
    }
}

/// 本地附件存储的 FFI 门面。
#[derive(uniffi::Object)]
pub struct AttachmentFfi {
    store: Mutex<AttachmentStore>,
    descriptors: Mutex<Vec<AttachmentDescriptor>>,
}

#[uniffi::export]
impl AttachmentFfi {
    /// 以 cache_root 作为加密分块缓存目录（应用数据目录下的子目录）。
    #[uniffi::constructor]
    pub fn new(cache_root: String) -> Result<Arc<Self>, FfiError> {
        let store = AttachmentStore::new(cache_root).map_err(failure)?;
        Ok(Arc::new(Self {
            store: Mutex::new(store),
            descriptors: Mutex::new(Vec::new()),
        }))
    }

    /// ATTACHMENTS-001：流式导入并加密缓存，返回描述符。
    pub fn import_file(&self, path: String, display_name: String) -> Result<AttachmentView, FfiError> {
        let store = self.store.lock().map_err(|_| poisoned("attachment store"))?;
        let descriptor = store
            .import_file(&FileToken::from_local_path(path), &display_name)
            .map_err(failure)?;
        let status = store
            .attachment_status(&descriptor.attachment_id)
            .map_err(failure)?;
        let mut descriptors = self.descriptors.lock().map_err(|_| poisoned("attachment records"))?;
        descriptors.retain(|existing| existing.attachment_id != descriptor.attachment_id);
        descriptors.push(descriptor.clone());
        Ok(view(&descriptor, Some(&status)))
    }

    /// ATTACHMENTS-002：状态查询。
    pub fn status(&self, attachment_id: String) -> Result<AttachmentView, FfiError> {
        let descriptor = self.find(&attachment_id)?;
        let store = self.store.lock().map_err(|_| poisoned("attachment store"))?;
        let status = store.attachment_status(&attachment_id).map_err(failure)?;
        Ok(view(&descriptor, Some(&status)))
    }

    /// ATTACHMENTS-006：解密导出到目标路径，返回写出的明文字节数。
    pub fn export_file(&self, attachment_id: String, target_path: String) -> Result<u64, FfiError> {
        let descriptor = self.find(&attachment_id)?;
        let store = self.store.lock().map_err(|_| poisoned("attachment store"))?;
        store
            .export_attachment(&descriptor.attachment_id, &FileToken::from_local_path(target_path))
            .map(|receipt| receipt.written_bytes)
            .map_err(failure)
    }

    /// ATTACHMENTS-007：请求取消（保留缓存与进度；已完成的传输拒绝取消）。
    pub fn cancel(&self, attachment_id: String) -> Result<AttachmentView, FfiError> {
        let descriptor = self.find(&attachment_id)?;
        let store = self.store.lock().map_err(|_| poisoned("attachment store"))?;
        let status = store.cancel_transfer(&attachment_id).map_err(failure)?;
        Ok(view(&descriptor, Some(&status)))
    }
}

impl AttachmentFfi {
    fn find(&self, attachment_id: &str) -> Result<AttachmentDescriptor, FfiError> {
        self.descriptors
            .lock()
            .map_err(|_| poisoned("attachment records"))?
            .iter()
            .find(|existing| existing.attachment_id == attachment_id)
            .cloned()
            .ok_or_else(|| FfiError::Store {
                code: AttachmentError::AttachmentNotFound.code().to_owned(),
                reason: "attachment not found in this process".to_owned(),
            })
    }
}
