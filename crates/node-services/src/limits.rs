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
/// 客户端附件分块的明文块大小（CRYPTO-011，已冻结，节点侧不改）。
pub const CLIENT_ATTACHMENT_PLAINTEXT_CHUNK_BYTES: usize = 256 * 1024;
/// 单块密文的封装开销：版本字节 + AEAD nonce(12) + tag(16) = 29 字节。
///
/// 本 crate 不依赖 chat-crypto，因此用公开常量独立表达，并用下面的 const 断言锁住
/// “节点默认块上限 ≥ 客户端密文块”这个不变量：任何把默认值调小到 262_173 字节以下
/// 的改动都会直接编译失败，而不是等运行期把多块附件拒成 CONTENT_TOO_LARGE。
pub const CLIENT_ATTACHMENT_CIPHERTEXT_OVERHEAD: usize = 1 + 12 + 16;
/// 客户端一个满密文块的字节数（256 KiB + 29 = 262_173）。
pub const CLIENT_ATTACHMENT_CIPHERTEXT_CHUNK_BYTES: usize =
    CLIENT_ATTACHMENT_PLAINTEXT_CHUNK_BYTES + CLIENT_ATTACHMENT_CIPHERTEXT_OVERHEAD;

/// 声明块大小默认上限：1 MiB。
///
/// 必须 ≥ 客户端密文块（262_173 字节）：客户端按 256 KiB 明文分块（冻结格式），
/// 节点旧默认 32 KiB / 硬上限 60 KiB 会让**任何多块附件**被 CONTENT_TOO_LARGE 拒绝。
pub const DEFAULT_BLOB_CHUNK_BYTES: usize = 1024 * 1024;
/// 声明块大小硬上限：4 MiB。远端节点可通过策略下调，但不得超过此值。
pub const MAX_BLOB_CHUNK_BYTES_LIMIT: usize = 4 * 1024 * 1024;
/// 原始密文块 PUT 的**传输正文上限**（= 分块硬上限：密文按原始字节传输，没有编码开销）。
///
/// 只有 `PUT /v1/blobs/{id}/chunks/{index}` 路由使用这个上限；其余 JSON DTO 路由仍是
/// [`MAX_HTTP_BODY_BYTES`]（64 KiB），因此放宽块传输不会放大小请求的内存面。
pub const MAX_BLOB_CHUNK_BODY_BYTES: usize = MAX_BLOB_CHUNK_BYTES_LIMIT;

// 编译期不变量：默认/硬上限都必须放得下客户端满密文块，且传输上限不越过对象上限。
const _: () = assert!(DEFAULT_BLOB_CHUNK_BYTES >= CLIENT_ATTACHMENT_CIPHERTEXT_CHUNK_BYTES);
const _: () = assert!(MAX_BLOB_CHUNK_BYTES_LIMIT >= DEFAULT_BLOB_CHUNK_BYTES);
const _: () = assert!(MAX_BLOB_CHUNK_BYTES_LIMIT <= MAX_BLOB_OBJECT_BYTES_LIMIT as usize);
/// 单对象块数上限（默认 512 块）。
pub const DEFAULT_BLOB_CHUNK_COUNT: u32 = 512;
pub const MAX_BLOB_CHUNK_COUNT_LIMIT: u32 = 4096;
/// 上传会话默认有效期（1 小时）；过期会话不可再写块或完成。
pub const DEFAULT_BLOB_UPLOAD_TTL_SECONDS: u64 = 60 * 60;
/// 已发布附件的默认保留期（30 天）：到期后由 NODE-016 回收整对象。
pub const DEFAULT_BLOB_PUBLISHED_TTL_SECONDS: u64 = 30 * 24 * 60 * 60;
/// 已发布附件保留期的配置上限（10 年），防止配置成“永不回收”。
pub const MAX_BLOB_PUBLISHED_TTL_LIMIT: u64 = 10 * 365 * 24 * 60 * 60;

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
/// API-005 限流默认参数（固定窗口；窗口内请求数与字节数双限）。
pub const DEFAULT_RATE_LIMIT_WINDOW_SECONDS: u64 = 60;
pub const MAX_RATE_LIMIT_WINDOW_SECONDS: u64 = 3_600;
/// 每设备每窗口请求数（默认保守：1 分钟 120 次）。
pub const DEFAULT_RATE_LIMIT_REQUESTS_PER_DEVICE: u32 = 120;
/// 每账户每窗口请求数（多设备账户的上限，默认是每设备的两倍）。
pub const DEFAULT_RATE_LIMIT_REQUESTS_PER_ACCOUNT: u32 = 240;
/// 匿名端点（挑战/注册）每窗口请求数；挑战本身另有未消费容量上限。
pub const DEFAULT_RATE_LIMIT_REQUESTS_ANONYMOUS: u32 = 600;
/// 任意预算的上限，防止配置成“实际上无限”。
pub const MAX_RATE_LIMIT_REQUESTS: u32 = 1_000_000;
/// 每设备每窗口请求正文字节上限（大上传按字节计，而不是只按次数）。
pub const DEFAULT_RATE_LIMIT_BYTES_PER_DEVICE: u64 = 8 * 1024 * 1024;
pub const MAX_RATE_LIMIT_BYTES_PER_DEVICE_LIMIT: u64 = 1024 * 1024 * 1024;
/// 进程内桶表容量上限：达到上限时先清理过期窗口，仍满则拒绝新键（fail-closed）。
pub const DEFAULT_RATE_LIMIT_BUCKETS: usize = 4_096;
pub const MIN_RATE_LIMIT_BUCKETS: usize = 16;
pub const MAX_RATE_LIMIT_BUCKETS: usize = 65_536;

/// API-005 限流策略。只在 HTTP 层生效（服务层单测不经过限流）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateLimitPolicy {
    /// 是否启用。默认为 true；关闭必须显式配置（测试或受控环境）。
    pub enabled: bool,
    /// 固定窗口长度（秒）。
    pub window_seconds: u64,
    /// 每设备每窗口请求数上限。
    pub max_requests_per_device: u32,
    /// 每账户每窗口请求数上限（同一账户的多台设备共享）。
    pub max_requests_per_account: u32,
    /// 匿名端点每窗口请求数上限（本进程一个全局桶，见 api.md 的限制说明）。
    pub max_requests_anonymous: u32,
    /// 每设备每窗口请求正文字节上限。
    pub max_bytes_per_device: u64,
    /// 桶表容量上限。
    pub max_buckets: usize,
}

impl Default for RateLimitPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            window_seconds: DEFAULT_RATE_LIMIT_WINDOW_SECONDS,
            max_requests_per_device: DEFAULT_RATE_LIMIT_REQUESTS_PER_DEVICE,
            max_requests_per_account: DEFAULT_RATE_LIMIT_REQUESTS_PER_ACCOUNT,
            max_requests_anonymous: DEFAULT_RATE_LIMIT_REQUESTS_ANONYMOUS,
            max_bytes_per_device: DEFAULT_RATE_LIMIT_BYTES_PER_DEVICE,
            max_buckets: DEFAULT_RATE_LIMIT_BUCKETS,
        }
    }
}

impl RateLimitPolicy {
    /// 校验策略本身可用；任何越界配置都显式拒绝启动。
    pub fn validate(&self) -> NodeResult<()> {
        if self.window_seconds == 0 || self.window_seconds > MAX_RATE_LIMIT_WINDOW_SECONDS {
            return Err(NodeError::InvalidConfig("rate limit window"));
        }
        if self.max_requests_per_device == 0 || self.max_requests_per_device > MAX_RATE_LIMIT_REQUESTS {
            return Err(NodeError::InvalidConfig("rate limit device requests"));
        }
        if self.max_requests_per_account == 0 || self.max_requests_per_account > MAX_RATE_LIMIT_REQUESTS {
            return Err(NodeError::InvalidConfig("rate limit account requests"));
        }
        // 账户预算是多设备上限，不能比单设备还小（否则设备预算形同虚设）。
        if self.max_requests_per_account < self.max_requests_per_device {
            return Err(NodeError::InvalidConfig("rate limit account requests below device requests"));
        }
        if self.max_requests_anonymous == 0 || self.max_requests_anonymous > MAX_RATE_LIMIT_REQUESTS {
            return Err(NodeError::InvalidConfig("rate limit anonymous requests"));
        }
        if self.max_bytes_per_device > MAX_RATE_LIMIT_BYTES_PER_DEVICE_LIMIT {
            return Err(NodeError::InvalidConfig("rate limit device bytes"));
        }
        if self.max_buckets < MIN_RATE_LIMIT_BUCKETS || self.max_buckets > MAX_RATE_LIMIT_BUCKETS {
            return Err(NodeError::InvalidConfig("rate limit buckets"));
        }
        Ok(())
    }
}
/// NODE-016 每轮回收的默认行数上限（每类资源）。
pub const DEFAULT_CLEANUP_ROWS_PER_KIND: u32 = 512;
/// 每类资源每轮回收的行数硬上限。
pub const MAX_CLEANUP_ROWS_PER_KIND: u32 = 8_192;
/**
 * NODE-016 过期回收策略。
 *
 * 每轮每类资源都有行数上限：一次回收不允许把整库扫完（大库不能因为一次维护
 * 就长时间持写锁），因此回收是**多轮收敛**的，重复调用会继续清理剩余部分。
 */
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CleanupPolicy {
    /// 是否启用（关闭时 `collect_expired` 直接返回全零统计）。
    pub enabled: bool,
    /// 每类资源每轮最多处理多少行。
    pub max_rows_per_kind: u32,
    /// 限流桶在内存里最多保留多少个窗口（窗口长度的倍数）。
    pub rate_limit_bucket_windows: u32,
    /// 已发布附件的保留期（秒）：发布时刻 + 该值写入 blob_objects.expires_at。
    pub published_blob_ttl_seconds: u64,
}

impl Default for CleanupPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_rows_per_kind: DEFAULT_CLEANUP_ROWS_PER_KIND,
            rate_limit_bucket_windows: 2,
            published_blob_ttl_seconds: DEFAULT_BLOB_PUBLISHED_TTL_SECONDS,
        }
    }
}

impl CleanupPolicy {
    /// 有界校验。
    pub fn validate(&self) -> NodeResult<()> {
        if self.max_rows_per_kind == 0 || self.max_rows_per_kind > MAX_CLEANUP_ROWS_PER_KIND {
            return Err(NodeError::InvalidConfig("cleanup rows per kind"));
        }
        if self.rate_limit_bucket_windows == 0 || self.rate_limit_bucket_windows > 64 {
            return Err(NodeError::InvalidConfig("cleanup rate limit windows"));
        }
        if self.published_blob_ttl_seconds == 0
            || self.published_blob_ttl_seconds > MAX_BLOB_PUBLISHED_TTL_LIMIT
        {
            return Err(NodeError::InvalidConfig("cleanup published blob ttl"));
        }
        Ok(())
    }
}

/// CAP-AUTHZ：撤销授权策略。
///
/// 默认 `self_only = true`：只有**签发该令牌的设备**能撤销它（任何设备都能撤销任何令牌是实现缺陷）。
/// 部署侧显式放宽后，跨设备撤销会写入审计列（`authorization = policy_relaxed`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevokePolicy {
    /// 只允许签发设备撤销自己的令牌。
    pub self_only: bool,
}

impl Default for RevokePolicy {
    fn default() -> Self {
        Self { self_only: true }
    }
}

/// CAP 包：能力令牌强制模式策略。
///
/// 默认**关闭**（`required = false`），因此既有客户端与既有 HTTP 调用路径完全不变；
/// 打开后附件块 `PUT/GET` 必须带有效能力令牌（缺失 → `AUTH_REQUIRED`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityPolicy {
    /// 是否强制附件块端点携带能力令牌。
    pub required: bool,
    /// 节点签发能力令牌的有效期上限（秒，硬上限仍是 `MAX_CAPABILITY_TTL_SECONDS`）。
    pub max_ttl_seconds: u64,
    /// CAP-AUTHZ：撤销授权策略（默认只允许签发设备撤销）。
    pub revoke: RevokePolicy,
}

/// 能力令牌默认有效期上限（秒）。
pub const DEFAULT_CAPABILITY_MAX_TTL_SECONDS: u64 = 3_600;

impl Default for CapabilityPolicy {
    fn default() -> Self {
        Self {
            required: false,
            max_ttl_seconds: DEFAULT_CAPABILITY_MAX_TTL_SECONDS,
            revoke: RevokePolicy::default(),
        }
    }
}

impl CapabilityPolicy {
    /// 校验策略；TTL 必须落在 `1..=MAX_CAPABILITY_TTL_SECONDS`。
    pub fn validate(&self) -> NodeResult<()> {
        if self.max_ttl_seconds == 0
            || self.max_ttl_seconds > crate::capability::MAX_CAPABILITY_TTL_SECONDS
        {
            return Err(NodeError::InvalidConfig("capability max ttl"));
        }
        Ok(())
    }

    /// 从环境变量读强制开关（部署侧/CLI 之外的运行期开关）。
    ///
    /// - `CHAT_NODE_CAPABILITY_REQUIRED`：`1/true/yes/on` 打开，其他值（含缺失）关闭；
    /// - `CHAT_NODE_CAPABILITY_MAX_TTL_SECONDS`：正整数，缺省用默认值。
    ///
    /// 非法 TTL 直接返回策略错误（不静默回退到默认值，避免“以为开/关”的错觉）。
    pub fn from_env() -> NodeResult<Self> {
        let required = std::env::var("CHAT_NODE_CAPABILITY_REQUIRED")
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false);
        let max_ttl_seconds = match std::env::var("CHAT_NODE_CAPABILITY_MAX_TTL_SECONDS") {
            Ok(value) => value
                .trim()
                .parse::<u64>()
                .map_err(|_| NodeError::InvalidConfig("capability max ttl env"))?,
            Err(_) => DEFAULT_CAPABILITY_MAX_TTL_SECONDS,
        };
        let self_only = match std::env::var("CHAT_NODE_CAPABILITY_REVOKE_ANY_DEVICE") {
            Ok(value) => !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ),
            Err(_) => true,
        };
        let policy = Self {
            required,
            max_ttl_seconds,
            revoke: RevokePolicy { self_only },
        };
        policy.validate()?;
        Ok(policy)
    }
}
