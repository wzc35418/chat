//! MLS exporter 密钥派生（CRYPTO-013：把附件包装密钥从 P0 占位派生换成真实 exporter）。
//!
//! OpenMLS 的 `export_secret` 从当前 epoch 的 exporter secret 派生：同一群里同一 epoch 的
//! 所有成员导出**相同**的字节，epoch 前进后导出值随之改变。因此它适合做"同会话同 epoch
//! 绑定"的包装密钥（换会话/换 epoch 打不开），而不再是占位的哈希派生。
//!
//! 本模块只做有界的派生入口：
//! - label 必须在白名单里（目前只有附件包装密钥）；
//! - context 有上限（调用方用 SHA-256(attachment_id ‖ 会话 ‖ epoch) 之类的绑定串）；
//! - 长度 1..=32（对称密钥上限，避免把 exporter 当 KDF 无界展开）；
//! - **只导出已持久化且无 pending 提交的 epoch**：导出前核对正式库里的已提交 epoch 与内存
//!   epoch 一致，并把 pending 控制提交视为不可导出（否则拿到的密钥可能对应一个尚未落库、
//!   随时会被替换的分支状态）；
//! - 返回值用 `Zeroizing` 缓冲，Drop 时清零；不进入日志/DTO。

use std::fmt;

use openmls::prelude::OpenMlsProvider;
use zeroize::Zeroizing;

use chat_storage::formal::Store as FormalStore;

use crate::engine::MlsDevice;

/// 附件 FileKey 包装密钥的 exporter label（稳定文本，改动即协议不兼容）。
pub const EXPORTER_LABEL_ATTACHMENT_FILE_KEY: &str = "chat attachment file key";
/// 允许导出的 label 白名单（新增 label 必须显式登记）。
pub const EXPORTER_LABELS: [&str; 1] = [EXPORTER_LABEL_ATTACHMENT_FILE_KEY];
/// exporter context 上限（字节）。
pub const MAX_EXPORTER_CONTEXT_BYTES: usize = 256;
/// 单次导出的密钥长度上限（字节）。
pub const MAX_EXPORTER_KEY_BYTES: usize = 32;

/// exporter 派生失败原因（稳定错误码）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExporterError {
    /// label 不在白名单里。
    UnsupportedLabel,
    /// context 超过上限。
    ContextTooLong,
    /// 密钥长度越界（0 或 > 32）。
    KeyLengthInvalid,
    /// 群未初始化。
    GroupNotInitialized,
    /// 本机有 pending 控制提交（或 MLS 有未跟踪的 pending commit）。
    PendingCommit,
    /// 当前 epoch 还没有持久化的已提交快照。
    NotPersisted,
    /// 正式库里的已提交 epoch 与内存 epoch 不一致（不能拿旧密钥封装新状态）。
    StaleEpoch {
        /// 内存里的当前 epoch。
        expected: u64,
        /// 正式库里的已提交 epoch。
        found: u64,
    },
    /// 本机已被移出群（MLS group 不再 active）。
    NotActive,
    /// 正式存储读取失败（保留稳定码与脱敏消息）。
    Storage {
        /// 稳定错误码（来自 `StorageError::code`）。
        code: &'static str,
        /// 脱敏错误消息。
        detail: String,
    },
    /// OpenMLS 派生失败。
    Crypto(&'static str),
}

impl ExporterError {
    /// 稳定错误码字符串。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedLabel => "UNSUPPORTED_EXPORTER_LABEL",
            Self::ContextTooLong => "EXPORTER_CONTEXT_TOO_LONG",
            Self::KeyLengthInvalid => "EXPORTER_KEY_LENGTH_INVALID",
            Self::GroupNotInitialized => "CRYPTO_FAILURE",
            Self::PendingCommit => "EXPORTER_PENDING_COMMIT",
            Self::NotPersisted => "EXPORTER_NOT_PERSISTED",
            Self::StaleEpoch { .. } => "STALE_EPOCH",
            Self::NotActive => "USE_AFTER_EVICTION",
            Self::Storage { code, .. } => code,
            Self::Crypto(_) => "CRYPTO_FAILURE",
        }
    }
}

impl fmt::Display for ExporterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedLabel => formatter.write_str("exporter label is not allowed"),
            Self::ContextTooLong => formatter.write_str("exporter context exceeds the limit"),
            Self::KeyLengthInvalid => formatter.write_str("exporter key length is out of range"),
            Self::GroupNotInitialized => formatter.write_str("MLS group is not initialized"),
            Self::PendingCommit => formatter.write_str("a control commit is pending"),
            Self::NotPersisted => formatter.write_str("the current epoch is not persisted yet"),
            Self::StaleEpoch { expected, found } => write!(
                formatter,
                "persisted epoch {found} does not match the local epoch {expected}"
            ),
            Self::NotActive => formatter.write_str("this device is no longer an active member"),
            Self::Storage { code, detail } => {
                write!(formatter, "storage failure {code}: {detail}")
            }
            Self::Crypto(detail) => write!(formatter, "MLS exporter failure: {detail}"),
        }
    }
}

impl std::error::Error for ExporterError {}

impl MlsDevice {
    /// 用 MLS exporter 从当前已提交 epoch 派生一个密钥。
    ///
    /// `store` 用于确认"导出的密钥对应正式库里已提交的 epoch"：
    /// - 本机有 pending 控制提交或 MLS 持有未跟踪 pending commit → [ExporterError::PendingCommit]；
    /// - 正式库没有该群的已提交快照 → [ExporterError::NotPersisted]；
    /// - 正式库 epoch 与内存 epoch 不一致 → [ExporterError::StaleEpoch]。
    ///
    /// 满足条件后调用 OpenMLS `export_secret`：同群同 epoch 的所有成员得到相同字节。
    pub fn export_key(
        &self,
        store: &FormalStore,
        label: &str,
        context: &[u8],
        len: usize,
    ) -> Result<Zeroizing<Vec<u8>>, ExporterError> {
        if !EXPORTER_LABELS.contains(&label) {
            return Err(ExporterError::UnsupportedLabel);
        }
        if context.len() > MAX_EXPORTER_CONTEXT_BYTES {
            return Err(ExporterError::ContextTooLong);
        }
        if len == 0 || len > MAX_EXPORTER_KEY_BYTES {
            return Err(ExporterError::KeyLengthInvalid);
        }
        if self.pending_record().is_some() {
            return Err(ExporterError::PendingCommit);
        }
        let group = self
            .group_ref()
            .map_err(|_| ExporterError::GroupNotInitialized)?;
        if group.pending_commit().is_some() {
            return Err(ExporterError::PendingCommit);
        }
        let local_epoch = group.epoch().as_u64();
        let group_key = self
            .group_key()
            .map_err(|_| ExporterError::GroupNotInitialized)?;
        let persisted = store
            .load_mls_group_state(&group_key)
            .map_err(|error| ExporterError::Storage {
                code: error.code(),
                detail: error.to_string(),
            })?;
        match persisted {
            Some(record) if record.epoch == local_epoch => {}
            Some(record) => {
                return Err(ExporterError::StaleEpoch {
                    expected: local_epoch,
                    found: record.epoch,
                });
            }
            None => return Err(ExporterError::NotPersisted),
        }
        let secret = group
            .export_secret(self.provider().crypto(), label, context, len)
            .map_err(|error| match error {
                openmls::prelude::ExportSecretError::GroupStateError(_) => ExporterError::NotActive,
                _ => ExporterError::Crypto("MLS exporter derivation failed"),
            })?;
        Ok(Zeroizing::new(secret))
    }
}

/// 附件包装密钥的 exporter context：SHA-256(attachment_id ‖ 会话 ‖ epoch)。
///
/// 把附件 ID、会话与 epoch 一起绑定：换附件/换会话/换 epoch 都导不出同一个密钥。
/// 返回 32 字节，调用方作为 `context` 传给 [MlsDevice::export_key]。
pub fn attachment_wrapping_context(
    attachment_id: &str,
    conversation_id: &str,
    mls_epoch: u64,
) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"chat/crypto/attachment-wrapping-context/v1");
    hasher.update((attachment_id.len() as u32).to_be_bytes());
    hasher.update(attachment_id.as_bytes());
    hasher.update((conversation_id.len() as u32).to_be_bytes());
    hasher.update(conversation_id.as_bytes());
    hasher.update(mls_epoch.to_be_bytes());
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_slice());
    out
}
