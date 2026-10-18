//! 节点鉴权、设备注册、群日志与信箱的全部上限。
//!
//! 每个上限都在解析边界处显式拒绝，不用“截断到上限”冒充成功。
//! 对应契约见 docs/implementation/node.md 与 docs/implementation/api.md 的
//! NODE-001 / API-002 / API-003 / NODE-005~011 / API-006 / API-007。
use crate::error::{NodeError, NodeResult};

/// 单个标识的最长 UTF-8 字节数。
pub const MAX_DEVICE_ID_BYTES: usize = 128;
pub const MAX_ACCOUNT_ID_BYTES: usize = 128;
pub const MAX_NODE_ID_BYTES: usize = 128;
pub const MAX_ENDPOINT_ID_BYTES: usize = 256;
pub const MAX_CHALLENGE_ID_BYTES: usize = 128;
pub const MAX_CAPABILITY_ID_BYTES: usize = 128;

/// HTTP 方法、规范路径与查询串上限。签名原文只覆盖规范后的这些值。
pub const MAX_METHOD_BYTES: usize = 16;
pub const MAX_PATH_BYTES: usize = 512;
/// 查询串上限。必须能放下一个完整的分页游标（PROTOCOL-013 的 token 上限是
/// 1024 字节），否则合法游标无法通过签名原文的规范化；这里仍是有界值，
/// 且整段原文都参与签名。
pub const MAX_QUERY_BYTES: usize = 2048;

/// 单个 HTTP 请求正文上限。注册请求本身远小于此值，超出即 413。
pub const MAX_HTTP_BODY_BYTES: usize = 64 * 1024;

/// 挑战默认有效期（秒）；契约要求挑战短期有效且不可跨重启重放。
pub const DEFAULT_CHALLENGE_TTL_SECONDS: u64 = 60;
/// 允许的时钟偏差（秒），用于发行时间落在未来方向的判定。
pub const DEFAULT_CLOCK_SKEW_SECONDS: u64 = 30;
/// 同时未消费挑战的全局上限，防止匿名端点无限分配内存/磁盘。
pub const DEFAULT_MAX_PENDING_CHALLENGES: usize = 4096;
/// 每账户有效设备上限。默认 2 台，与产品口径（docs/design/ARCHITECTURE.md、
/// docs/design/MVP.md、docs/implementation/TYPES.md 的“每账户最多 2 台设备”）一致。
/// 该值只是 crate::services::NodeAuthPolicy 的默认值，测试用可配上限覆盖其它取值；
/// 节点任何情况下都不隐式放宽，达到上限即 DEVICE_LIMIT_EXCEEDED。
pub const DEFAULT_MAX_DEVICES_PER_ACCOUNT: usize = 2;

/// 过期挑战的保留窗口：保留一段时间以便重放/过期可区分，之后有界清理。
pub const CHALLENGE_RETENTION_SECONDS: u64 = 600;
/// 单次清理事务删除的最大行数，避免一条 SQL 删掉整表。
pub const CHALLENGE_PURGE_BATCH: usize = 256;

/// 设备凭据与注册能力的有效期上限。
pub const MAX_CREDENTIAL_LIFETIME_SECONDS: u64 = 366 * 24 * 60 * 60;
pub const MAX_CAPABILITY_LIFETIME_SECONDS: u64 = 24 * 60 * 60;

/// 能安全写入 SQLite INTEGER 的时间戳上限。
pub const MAX_TIMESTAMP: u64 = i64::MAX as u64;

/// 群、提交与信箱条目标识的最长 UTF-8 字节数（协议规范文本形式）。
pub const MAX_GROUP_ID_BYTES: usize = 128;
pub const MAX_SUBMISSION_ID_BYTES: usize = 128;
pub const MAX_MAILBOX_ITEM_ID_BYTES: usize = 128;
/// 群成员角色字符串上限。
pub const MAX_GROUP_ROLE_BYTES: usize = 16;

/// 群日志分页默认上限（单页条数）与硬上限。
pub const DEFAULT_GROUP_LOG_PAGE_ENTRIES: u32 = 200;
pub const MAX_GROUP_LOG_PAGE_ENTRIES: u32 = 500;
/// 单群成员硬上限（群容量由节点策略执行，不静默截断成员表）。
pub const DEFAULT_MAX_GROUP_MEMBERS: usize = 256;
pub const MAX_GROUP_MEMBERS_LIMIT: usize = 4096;
/// 单条候选密文上限。候选密文以 hex 出现在 JSON DTO 中，因此这里取远小于
/// MAX_HTTP_BODY_BYTES 的值，保证编码后的请求正文仍在正文上限内。
pub const MAX_GROUP_CANDIDATE_CIPHERTEXT_BYTES: usize = 16 * 1024;

/// 信箱分页默认上限（单页条数）与硬上限。
pub const DEFAULT_MAILBOX_PAGE_ITEMS: u32 = 100;
pub const MAX_MAILBOX_PAGE_ITEMS: u32 = 200;
/// 单页响应预算（密文字节）；超出预算就分页，不截断单条密文。
pub const DEFAULT_MAILBOX_PAGE_BYTES: usize = 512 * 1024;
pub const MAX_MAILBOX_PAGE_BYTES_LIMIT: usize = 4 * 1024 * 1024;
/// 单条信箱密文上限；必须不大于单页预算，否则单条就无法放进一页。
pub const DEFAULT_MAILBOX_ITEM_BYTES: usize = 32 * 1024;
/// 每设备信箱持久上限（未消费条目的密文字节合计）。
pub const DEFAULT_MAILBOX_BYTES_PER_DEVICE: u64 = 8 * 1024 * 1024;
/// 单次 ACK 批量上限。
pub const DEFAULT_MAX_ACK_BATCH: usize = 128;
pub const MAX_ACK_BATCH_LIMIT: usize = 1024;
/// 信箱条目默认 TTL：30 天（NODE-016 的口径）。
pub const DEFAULT_MAILBOX_TTL_SECONDS: u64 = 30 * 24 * 60 * 60;

/// 群日志策略。字段在启动时校验，不接受 0 或越界配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupLogPolicy {
    /// 单页最多返回的日志条数；请求超过该值即显式拒绝。
    pub max_page_entries: u32,
    /// 单群初始成员上限；超过即 GROUP_CAPACITY_EXCEEDED。
    pub max_members: usize,
    /// 单条候选密文上限。
    pub max_candidate_ciphertext_bytes: usize,
}

impl Default for GroupLogPolicy {
    fn default() -> Self {
        Self {
            max_page_entries: DEFAULT_GROUP_LOG_PAGE_ENTRIES,
            max_members: DEFAULT_MAX_GROUP_MEMBERS,
            max_candidate_ciphertext_bytes: MAX_GROUP_CANDIDATE_CIPHERTEXT_BYTES,
        }
    }
}

impl GroupLogPolicy {
    /// 校验策略本身可用；任何越界配置都显式拒绝启动。
    pub fn validate(&self) -> NodeResult<()> {
        if self.max_page_entries == 0 || self.max_page_entries > MAX_GROUP_LOG_PAGE_ENTRIES {
            return Err(NodeError::InvalidConfig("group log page entries"));
        }
        if self.max_members == 0 || self.max_members > MAX_GROUP_MEMBERS_LIMIT {
            return Err(NodeError::InvalidConfig("group max members"));
        }
        if self.max_candidate_ciphertext_bytes == 0
            || self.max_candidate_ciphertext_bytes > MAX_GROUP_CANDIDATE_CIPHERTEXT_BYTES
        {
            return Err(NodeError::InvalidConfig("group candidate ciphertext bytes"));
        }
        Ok(())
    }
}

/// 密文信箱策略。字段在启动时校验，不接受 0 或越界配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailboxPolicy {
    /// 单页最多返回的条目数。
    pub max_page_items: u32,
    /// 单页响应字节预算。
    pub max_page_bytes: usize,
    /// 单条密文上限。
    pub max_item_bytes: usize,
    /// 每设备持久字节上限。
    pub max_bytes_per_device: u64,
    /// 单次 ACK 批量上限。
    pub max_ack_batch: usize,
    /// 条目默认 TTL（秒）。
    pub default_ttl_seconds: u64,
}

impl Default for MailboxPolicy {
    fn default() -> Self {
        Self {
            max_page_items: DEFAULT_MAILBOX_PAGE_ITEMS,
            max_page_bytes: DEFAULT_MAILBOX_PAGE_BYTES,
            max_item_bytes: DEFAULT_MAILBOX_ITEM_BYTES,
            max_bytes_per_device: DEFAULT_MAILBOX_BYTES_PER_DEVICE,
            max_ack_batch: DEFAULT_MAX_ACK_BATCH,
            default_ttl_seconds: DEFAULT_MAILBOX_TTL_SECONDS,
        }
    }
}

impl MailboxPolicy {
    /// 校验策略本身可用，并保证单条密文能放进一页预算。
    pub fn validate(&self) -> NodeResult<()> {
        if self.max_page_items == 0 || self.max_page_items > MAX_MAILBOX_PAGE_ITEMS {
            return Err(NodeError::InvalidConfig("mailbox page items"));
        }
        if self.max_page_bytes == 0 || self.max_page_bytes > MAX_MAILBOX_PAGE_BYTES_LIMIT {
            return Err(NodeError::InvalidConfig("mailbox page bytes"));
        }
        if self.max_item_bytes == 0 || self.max_item_bytes > self.max_page_bytes {
            return Err(NodeError::InvalidConfig("mailbox item bytes"));
        }
        if self.max_bytes_per_device < self.max_item_bytes as u64 {
            return Err(NodeError::InvalidConfig("mailbox device quota"));
        }
        if self.max_ack_batch == 0 || self.max_ack_batch > MAX_ACK_BATCH_LIMIT {
            return Err(NodeError::InvalidConfig("mailbox ack batch"));
        }
        if self.default_ttl_seconds == 0 || self.default_ttl_seconds > MAX_TIMESTAMP {
            return Err(NodeError::InvalidConfig("mailbox ttl"));
        }
        Ok(())
    }
}
/// 附件 blob 的标识上限（blob id / upload id 都是 PROTOCOL-001 文本或节点随机 ID）。
pub const MAX_BLOB_ID_BYTES: usize = 128;
pub const MAX_BLOB_UPLOAD_ID_BYTES: usize = 128;

/// 每设备已发布+在传密文字节上限（默认 32 MiB）。
pub const DEFAULT_BLOB_BYTES_PER_DEVICE: u64 = 32 * 1024 * 1024;
pub const MAX_BLOB_BYTES_PER_DEVICE_LIMIT: u64 = 1024 * 1024 * 1024;
/// 单个密文对象总字节上限（默认 8 MiB）。
pub const DEFAULT_BLOB_OBJECT_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_BLOB_OBJECT_BYTES_LIMIT: u64 = 64 * 1024 * 1024;
/// 声明块大小上限（默认 32 KiB；仍必须小于 HTTP 正文上限，密文按原始字节传输）。
pub const DEFAULT_BLOB_CHUNK_BYTES: usize = 32 * 1024;
pub const MAX_BLOB_CHUNK_BYTES_LIMIT: usize = 60 * 1024;
/// 单对象块数上限（默认 512 块）。
pub const DEFAULT_BLOB_CHUNK_COUNT: u32 = 512;
pub const MAX_BLOB_CHUNK_COUNT_LIMIT: u32 = 4096;
/// 上传会话默认有效期（1 小时）；过期会话不可再写块或完成。
pub const DEFAULT_BLOB_UPLOAD_TTL_SECONDS: u64 = 60 * 60;

/// 附件 blob 策略。字段在启动时校验，不接受 0 或越界配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobPolicy {
    /// 每设备“已发布 + 在传”密文字节上限。
    pub max_bytes_per_device: u64,
    /// 单个对象总密文字节上限。
    pub max_object_bytes: u64,
    /// 声明单块字节上限。
    pub max_chunk_bytes: usize,
    /// 单对象块数上限。
    pub max_chunk_count: u32,
    /// 上传会话有效期（秒）。
    pub upload_ttl_seconds: u64,
}

impl Default for BlobPolicy {
    fn default() -> Self {
        Self {
            max_bytes_per_device: DEFAULT_BLOB_BYTES_PER_DEVICE,
            max_object_bytes: DEFAULT_BLOB_OBJECT_BYTES,
            max_chunk_bytes: DEFAULT_BLOB_CHUNK_BYTES,
            max_chunk_count: DEFAULT_BLOB_CHUNK_COUNT,
            upload_ttl_seconds: DEFAULT_BLOB_UPLOAD_TTL_SECONDS,
        }
    }
}

impl BlobPolicy {
    /// 校验策略本身可用；任何越界配置都显式拒绝启动。
    pub fn validate(&self) -> NodeResult<()> {
        if self.max_bytes_per_device == 0 || self.max_bytes_per_device > MAX_BLOB_BYTES_PER_DEVICE_LIMIT {
            return Err(NodeError::InvalidConfig("blob device quota"));
        }
        if self.max_object_bytes == 0
            || self.max_object_bytes > MAX_BLOB_OBJECT_BYTES_LIMIT
            || self.max_object_bytes > self.max_bytes_per_device
        {
            return Err(NodeError::InvalidConfig("blob object bytes"));
        }
        if self.max_chunk_bytes == 0 || self.max_chunk_bytes > MAX_BLOB_CHUNK_BYTES_LIMIT {
            return Err(NodeError::InvalidConfig("blob chunk bytes"));
        }
        if u64::try_from(self.max_chunk_bytes).map_or(true, |chunk| chunk > self.max_object_bytes) {
            return Err(NodeError::InvalidConfig("blob chunk bytes exceed object bytes"));
        }
        if self.max_chunk_count == 0 || self.max_chunk_count > MAX_BLOB_CHUNK_COUNT_LIMIT {
            return Err(NodeError::InvalidConfig("blob chunk count"));
        }
        if self.upload_ttl_seconds == 0 || self.upload_ttl_seconds > MAX_TIMESTAMP {
            return Err(NodeError::InvalidConfig("blob upload ttl"));
        }
        Ok(())
    }
}
