//! ID 校验与确定性 SubmissionId 派生。
//!
//! 本层不发明新的 ID 形式：
//!
//! - 来自存储/节点的文本 ID 按正式存储的字符与长度规则校验（非空、ASCII、
//!   ≤ [`MAX_SYNC_ID_BYTES`]、只允许字母数字与 `._-:/`），失败返回 `INVALID_ID`；
//! - 本层**创建**的 SubmissionId 使用 PROTOCOL-001 的强类型 [`SubmissionId`]，
//!   由 (draft_id, mls_epoch, attempt_index) 确定性派生：同一输入永远是同一 ID，
//!   重建序号变化才得到不同 ID。

use chat_protocol::SubmissionId;
use sha2::{Digest, Sha256};

use crate::error::{Result, invalid_id};

/// sync 边界接受的文本 ID 长度上限（与正式存储一致）。
pub const MAX_SYNC_ID_BYTES: usize = 128;

const SUBMISSION_ID_DOMAIN: &[u8] = b"chat/sync/submission-id/v1";

/// 校验通用文本 ID。
pub fn validate_id(kind: &'static str, value: &str) -> Result<()> {
    let valid = !value.is_empty()
        && value.len() <= MAX_SYNC_ID_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/')
        });
    if !valid {
        return Err(invalid_id(kind));
    }
    Ok(())
}

/// 确定性派生 SubmissionId。
///
/// 同一 `(draft_id, mls_epoch, attempt_index)` 永远得到同一 ID，因此同一草稿的
/// 二次调度（进程重启、事务重试）不会产生第二个 SubmissionId，也不会触发第二次
/// 加密；重建（`attempt_index + 1`）则必须得到不同 ID。
pub fn derive_submission_id(
    draft_id: &str,
    mls_epoch: u64,
    attempt_index: u32,
) -> Result<SubmissionId> {
    validate_id("draft", draft_id)?;
    let mut hasher = Sha256::new();
    hasher.update(SUBMISSION_ID_DOMAIN);
    hasher.update((draft_id.len() as u64).to_be_bytes());
    hasher.update(draft_id.as_bytes());
    hasher.update(mls_epoch.to_be_bytes());
    hasher.update(attempt_index.to_be_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(digest.as_slice());
    Ok(SubmissionId::from_bytes(bytes))
}
