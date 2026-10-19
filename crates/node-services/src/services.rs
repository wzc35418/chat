//! 节点服务核心：固定节点身份、一次性挑战、请求鉴权与设备注册。
//!
//! 覆盖 docs/implementation/api.md 的 API-002 / API-003 与 docs/implementation/node.md 的
//! NODE-001。事务边界按 TRANSACTIONS.md T4：设备登记、注册能力消费与回执写入同一
//! IMMEDIATE 事务；挑战消费是独立的短事务，失败一律显式返回，绝不返回“模拟成功”。
use std::path::Path;
use std::time::Duration;

use ed25519_dalek::{Signature, Signer, SigningKey};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::auth::{
    AuthChallenge, ChallengePurpose, RequestMeta, SignedAuth, decode_signed_auth,
    request_signing_bytes,
};
use crate::canonical::{hex_encode, sha256, validate_bounded_id, validate_timestamp, verify_ed25519};
use crate::error::{NodeError, NodeResult};
use crate::limits::{
    CHALLENGE_PURGE_BATCH, CHALLENGE_RETENTION_SECONDS, DEFAULT_CHALLENGE_TTL_SECONDS,
    DEFAULT_CLOCK_SKEW_SECONDS, DEFAULT_MAX_DEVICES_PER_ACCOUNT, DEFAULT_MAX_PENDING_CHALLENGES,
    BlobPolicy, CapabilityPolicy, GroupLogPolicy, MAX_ACCOUNT_ID_BYTES, MAX_DEVICE_ID_BYTES,
    MAX_HTTP_BODY_BYTES, CleanupPolicy, MAX_NODE_ID_BYTES, MailboxPolicy, RateLimitPolicy,
};
use crate::registration::{DeviceRegistration, RegisterDeviceRequest};

const MIGRATION_0001: &str = include_str!("../migrations/0001_initial.sql");
const MIGRATION_0002: &str = include_str!("../migrations/0002_node_auth.sql");
const MIGRATION_0003: &str = include_str!("../migrations/0003_group_mailbox.sql");
const MIGRATION_0004: &str = include_str!("../migrations/0004_blob_attachments.sql");
const MIGRATION_0005: &str = include_str!("../migrations/0005_retention_capabilities.sql");
const MIGRATION_0006: &str = include_str!("../migrations/0006_domain_operation.sql");
const MIGRATION_0007: &str = include_str!("../migrations/0007_capability_revocations.sql");
const MIGRATION_CHECKSUM_SLOT: &str = ":checksum";

/// 节点鉴权与注册策略。所有字段在启动时校验，不接受 0 或越界配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeAuthPolicy {
    /// 每账户有效设备上限。
    pub max_devices_per_account: usize,
    /// 挑战有效期（秒）。
    pub challenge_ttl_seconds: u64,
    /// 未消费挑战的全局上限。
    pub max_pending_challenges: usize,
    /// 单请求正文上限。
    pub max_request_body_bytes: usize,
    /// 允许的时钟偏差（秒）。
    pub clock_skew_seconds: u64,
    /// 群日志上限。
    pub group_log: GroupLogPolicy,
    /// 密文信箱上限。
    pub mailbox: MailboxPolicy,
    /// 附件 blob 上传/读取上限。
    pub blob: BlobPolicy,
    /// API-005 HTTP 层限流策略。
    pub rate_limit: RateLimitPolicy,
    /// NODE-016 过期回收策略。
    pub cleanup: CleanupPolicy,
    /// CAP 包：能力令牌强制模式。
    pub capability: CapabilityPolicy,
}

impl Default for NodeAuthPolicy {
    fn default() -> Self {
        Self {
            max_devices_per_account: DEFAULT_MAX_DEVICES_PER_ACCOUNT,
            challenge_ttl_seconds: DEFAULT_CHALLENGE_TTL_SECONDS,
            max_pending_challenges: DEFAULT_MAX_PENDING_CHALLENGES,
            max_request_body_bytes: MAX_HTTP_BODY_BYTES,
            clock_skew_seconds: DEFAULT_CLOCK_SKEW_SECONDS,
            group_log: GroupLogPolicy::default(),
            mailbox: MailboxPolicy::default(),
            blob: BlobPolicy::default(),
            rate_limit: RateLimitPolicy::default(),
            cleanup: CleanupPolicy::default(),
            capability: CapabilityPolicy::default(),
        }
    }
}

impl NodeAuthPolicy {
    fn validate(&self) -> NodeResult<()> {
        if self.max_devices_per_account == 0 || self.max_devices_per_account > 32 {
            return Err(NodeError::InvalidConfig("max devices per account"));
        }
        if self.challenge_ttl_seconds == 0 || self.challenge_ttl_seconds > 600 {
            return Err(NodeError::InvalidConfig("challenge ttl"));
        }
        if self.max_pending_challenges == 0 || self.max_pending_challenges > 1_000_000 {
            return Err(NodeError::InvalidConfig("max pending challenges"));
        }
        if self.max_request_body_bytes < 1024 || self.max_request_body_bytes > 1024 * 1024 {
            return Err(NodeError::InvalidConfig("max request body bytes"));
        }
        if self.clock_skew_seconds > 300 {
            return Err(NodeError::InvalidConfig("clock skew"));
        }
        self.group_log.validate()?;
        self.mailbox.validate()?;
        self.blob.validate()?;
        self.rate_limit.validate()?;
        self.cleanup.validate()?;
        self.capability.validate()?;
        Ok(())
    }
}

/// 节点服务启动配置：节点标识、节点 Ed25519 签名种子与策略。
///
/// 种子由部署侧托管；节点在首次启动时把 node_id 与公钥固定写入库，之后不一致即拒绝启动。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeServiceConfig {
    pub node_id: String,
    pub signing_seed: [u8; 32],
    pub policy: NodeAuthPolicy,
}

impl NodeServiceConfig {
    /// 用默认策略建立配置。
    pub fn new(node_id: impl Into<String>, signing_seed: [u8; 32]) -> Self {
        Self {
            node_id: node_id.into(),
            signing_seed,
            policy: NodeAuthPolicy::default(),
        }
    }
}

/// 通过请求鉴权得到的已登记设备身份。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthenticatedDevice {
    pub account_id: String,
    pub device_id: String,
    pub credential_version: u64,
    pub challenge_id: String,
}

/// 已消费一次挑战并验证过设备签名的请求证明。
///
/// 字段私有：只有本模块能构造，因此“持有证明”等价于“刚刚通过了一次挑战鉴权”。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedRequestProof {
    device_id: String,
    challenge_id: String,
    device_public_key: [u8; 32],
    registered_account_id: Option<String>,
}

impl VerifiedRequestProof {
    /// 通过鉴权的设备标识。
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// 被消费的挑战标识。
    pub fn challenge_id(&self) -> &str {
        &self.challenge_id
    }

    /// 通过鉴权的设备签名公钥。
    pub fn device_public_key(&self) -> &[u8; 32] {
        &self.device_public_key
    }

    /// 若该设备已登记，返回其账户；新设备（首次注册）为 None。
    pub fn registered_account_id(&self) -> Option<&str> {
        self.registered_account_id.as_deref()
    }
}

/// 设备只读视图（不含任何私钥或可用来重放的材料）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredDevice {
    pub account_id: String,
    pub device_id: String,
    pub device_public_key: String,
    pub endpoint_id: String,
    pub credential_version: u64,
    pub state: String,
}

struct DeviceRow {
    account_id: String,
    state: String,
    credential_version: u64,
    public_key: Option<[u8; 32]>,
}

impl std::fmt::Debug for NodeService {
    /// 只暴露节点标识与公钥；签名种子不入日志。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NodeService")
            .field("node_id", &self.node_id)
            .field("node_public_key", &hex_encode(&self.node_public_key))
            .finish_non_exhaustive()
    }
}

/// 独立节点库上的服务对象。一个进程内单连接串行执行，事务由本对象控制。
pub struct NodeService {
    connection: Connection,
    signing_key: SigningKey,
    node_public_key: [u8; 32],
    node_id: String,
    policy: NodeAuthPolicy,
}

impl NodeService {
    /// 打开（或创建）独立节点库并固定节点身份。
    pub fn open(path: &Path, config: NodeServiceConfig) -> NodeResult<Self> {
        let connection = Connection::open(path)?;
        Self::initialize(connection, config)
    }

    /// 内存节点库，供测试与本地实验使用；语义与文件库一致。
    pub fn open_in_memory(config: NodeServiceConfig) -> NodeResult<Self> {
        let connection = Connection::open_in_memory()?;
        Self::initialize(connection, config)
    }

    fn initialize(connection: Connection, config: NodeServiceConfig) -> NodeResult<Self> {
        validate_bounded_id("node id", &config.node_id, MAX_NODE_ID_BYTES)?;
        config.policy.validate()?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        migrate(&connection)?;
        let signing_key = SigningKey::from_bytes(&config.signing_seed);
        let node_public_key = signing_key.verifying_key().to_bytes();
        connection.execute(
            "INSERT OR IGNORE INTO node_identity (singleton, node_id, public_key) VALUES (1, ?1, ?2)",
            params![config.node_id, node_public_key.as_slice()],
        )?;
        let (stored_node_id, stored_public_key): (String, Vec<u8>) = connection.query_row(
            "SELECT node_id, public_key FROM node_identity WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if stored_node_id != config.node_id || stored_public_key.as_slice() != node_public_key {
            return Err(NodeError::NodeIdentityMismatch);
        }
        Ok(Self {
            connection,
            signing_key,
            node_public_key,
            node_id: config.node_id,
            policy: config.policy,
        })
    }

    /// 节点标识。
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// 节点签名公钥。
    pub fn node_public_key(&self) -> [u8; 32] {
        self.node_public_key
    }

    /// 当前策略。
    pub fn policy(&self) -> &NodeAuthPolicy {
        &self.policy
    }

    /// 只读连接（供同 crate 的群日志/信箱实现与单元测试使用）。
    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    /// 可写连接（供同 crate 的群日志/信箱实现开启 IMMEDIATE 事务）。
    pub(crate) fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }

    /// 节点签名钥（供同 crate 的群日志/信箱回执签名；不对外暴露）。
    pub(crate) fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// 已登记设备的签名公钥；未登记或数据损坏都是显式错误。
    pub(crate) fn registered_device_key(&self, device_id: &str) -> NodeResult<[u8; 32]> {
        let key: Option<Vec<u8>> = self
            .connection
            .query_row(
                "SELECT public_key FROM device_public_keys WHERE device_id = ?1",
                [device_id],
                |row| row.get(0),
            )
            .optional()?;
        let key = key.ok_or(NodeError::InvalidCredential)?;
        <[u8; 32]>::try_from(key.as_slice()).map_err(|_| NodeError::StorageUnavailable)
    }

    /// 设备记录的账户、状态与凭据版本；未登记返回 None。
    pub(crate) fn device_account_state(
        &self,
        device_id: &str,
    ) -> NodeResult<Option<(String, String, u64)>> {
        validate_bounded_id("device id", device_id, MAX_DEVICE_ID_BYTES)?;
        let row: Option<(String, String, i64)> = self
            .connection
            .query_row(
                "SELECT account_id, state, credential_version FROM devices WHERE device_id = ?1",
                [device_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        row.map(|(account_id, state, credential_version)| {
            Ok((
                account_id,
                state,
                u64::try_from(credential_version).map_err(|_| NodeError::StorageUnavailable)?,
            ))
        })
        .transpose()
    }

    /// 演员必须是本节点已登记的有效设备，且账户与声明一致（跨账户直接拒绝）。
    ///
    /// 群日志、信箱与附件端点共用这一条前置校验；失败一律显式返回，不继续推进。
    pub(crate) fn require_active_device(&self, actor: &AuthenticatedDevice) -> NodeResult<()> {
        let (account_id, state, _version) = self
            .device_account_state(&actor.device_id)?
            .ok_or(NodeError::InvalidCredential)?;
        if state != "active" {
            return Err(NodeError::DeviceRevoked);
        }
        if account_id != actor.account_id {
            return Err(NodeError::PermissionDenied);
        }
        Ok(())
    }

    /// 认证游标的派生密钥：由节点签名种子派生，跨重启稳定且不对外可见。
    pub(crate) fn cursor_key(&self) -> chat_protocol::CursorKey {
        chat_protocol::CursorKey::from_bytes(crate::canonical::sha256_concat(&[
            crate::canonical::CURSOR_KEY_DOMAIN,
            &self.signing_key.to_bytes(),
        ]))
    }

    /// 账户当前有效设备数。
    pub fn active_device_count(&self, account_id: &str) -> NodeResult<usize> {
        validate_bounded_id("account id", account_id, MAX_ACCOUNT_ID_BYTES)?;
        let count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM devices WHERE account_id = ?1 AND state = 'active'",
            [account_id],
            |row| row.get(0),
        )?;
        usize::try_from(count).map_err(|_| NodeError::StorageUnavailable)
    }

    /// 读取单台设备的公开视图。
    pub fn device(&self, device_id: &str) -> NodeResult<Option<StoredDevice>> {
        validate_bounded_id("device id", device_id, MAX_DEVICE_ID_BYTES)?;
        let row: Option<(String, String, String, String, i64, Option<Vec<u8>>)> = self
            .connection
            .query_row(
                "SELECT d.account_id, d.device_id, d.endpoint_id, d.state, d.credential_version,
                        k.public_key
                 FROM devices d LEFT JOIN device_public_keys k ON k.device_id = d.device_id
                 WHERE d.device_id = ?1",
                [device_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        row.map(
            |(account_id, device_id, endpoint_id, state, credential_version, key)| {
                let key = key.ok_or(NodeError::StorageUnavailable)?;
                Ok(StoredDevice {
                    account_id,
                    device_id,
                    device_public_key: hex_encode(&key),
                    endpoint_id,
                    credential_version: u64::try_from(credential_version)
                        .map_err(|_| NodeError::StorageUnavailable)?,
                    state,
                })
            },
        )
        .transpose()
    }

    /// API-002：签发一次性短期挑战。
    pub fn issue_auth_challenge(
        &mut self,
        purpose: ChallengePurpose,
        now: u64,
    ) -> NodeResult<AuthChallenge> {
        validate_timestamp("now", now)?;
        let expires_at = now
            .checked_add(self.policy.challenge_ttl_seconds)
            .ok_or(NodeError::InvalidRequest("challenge expiry overflow"))?;
        validate_timestamp("expires at", expires_at)?;
        let mut nonce = [0_u8; 32];
        rand::fill(&mut nonce);
        let mut identifier = [0_u8; 16];
        rand::fill(&mut identifier);
        let challenge_id = format!("chal_{}", hex_encode(&identifier));
        let mut challenge = AuthChallenge {
            challenge_id,
            nonce: hex_encode(&nonce),
            node_id: self.node_id.clone(),
            purpose,
            issued_at: now,
            expires_at,
            node_public_key: hex_encode(&self.node_public_key),
            signature: String::new(),
        };
        let signature: Signature = self.signing_key.sign(&challenge.signing_bytes()?);
        challenge.signature = hex_encode(&signature.to_bytes());
        let nonce_digest = challenge.nonce_digest()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let retention_cutoff = now.saturating_sub(CHALLENGE_RETENTION_SECONDS);
        transaction.execute(
            "DELETE FROM auth_challenges WHERE challenge_id IN (
                 SELECT challenge_id FROM auth_challenges
                 WHERE expires_at < ?1 ORDER BY expires_at LIMIT ?2)",
            params![to_i64("retention cutoff", retention_cutoff)?, CHALLENGE_PURGE_BATCH as i64],
        )?;
        let pending: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM auth_challenges WHERE consumed_at IS NULL AND expires_at >= ?1",
            params![to_i64("now", now)?],
            |row| row.get(0),
        )?;
        if usize::try_from(pending).unwrap_or(usize::MAX) >= self.policy.max_pending_challenges {
            return Err(NodeError::ChallengeCapacityExceeded);
        }
        transaction.execute(
            "INSERT INTO auth_challenges
                 (challenge_id, nonce_hash, expires_at, consumed_at, node_id, purpose, issued_at)
             VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6)",
            params![
                challenge.challenge_id,
                nonce_digest.as_slice(),
                to_i64("expires at", expires_at)?,
                self.node_id,
                purpose.as_str(),
                to_i64("now", now)?
            ],
        )?;
        transaction.commit()?;
        Ok(challenge)
    }

    /// API-003（设备未登记路径）：用调用方给出的设备公钥验证请求并消费挑战。
    ///
    /// 用于设备注册这种“设备尚未入库”的场景；设备公钥必须来自已验签的凭据，
    /// 调用顺序为“先解码凭据、再验签请求”，注册阶段还会再次核对凭据。
    pub fn verify_device_signature(
        &mut self,
        meta: &RequestMeta,
        body_hash: &[u8; 32],
        auth: &SignedAuth,
        device_public_key: &[u8; 32],
        expected_purpose: ChallengePurpose,
        now: u64,
    ) -> NodeResult<VerifiedRequestProof> {
        let stored = self.load_device(&auth.device_id)?;
        let registered_account_id = match stored {
            Some(row) => {
                if row.state != "active" {
                    return Err(NodeError::DeviceRevoked);
                }
                match row.public_key {
                    Some(key) if key == *device_public_key => {}
                    _ => return Err(NodeError::InvalidCredential),
                }
                Some(row.account_id)
            }
            None => None,
        };
        self.consume_and_verify(
            meta,
            body_hash,
            auth,
            device_public_key,
            registered_account_id,
            expected_purpose,
            now,
        )
    }

    /// API-003：已登记设备鉴权，返回账户与设备身份。
    pub fn authenticate_request(
        &mut self,
        meta: &RequestMeta,
        body_hash: &[u8; 32],
        auth: &SignedAuth,
        expected_purpose: ChallengePurpose,
        now: u64,
    ) -> NodeResult<AuthenticatedDevice> {
        validate_bounded_id("device id", &auth.device_id, MAX_DEVICE_ID_BYTES)?;
        let row = self
            .load_device(&auth.device_id)?
            .ok_or(NodeError::InvalidCredential)?;
        if row.state != "active" {
            return Err(NodeError::DeviceRevoked);
        }
        let public_key = row.public_key.ok_or(NodeError::InvalidCredential)?;
        let proof = self.consume_and_verify(
            meta,
            body_hash,
            auth,
            &public_key,
            Some(row.account_id.clone()),
            expected_purpose,
            now,
        )?;
        Ok(AuthenticatedDevice {
            account_id: row.account_id,
            device_id: proof.device_id.clone(),
            credential_version: row.credential_version,
            challenge_id: proof.challenge_id.clone(),
        })
    }

    fn consume_and_verify(
        &mut self,
        meta: &RequestMeta,
        body_hash: &[u8; 32],
        auth: &SignedAuth,
        device_public_key: &[u8; 32],
        registered_account_id: Option<String>,
        expected_purpose: ChallengePurpose,
        now: u64,
    ) -> NodeResult<VerifiedRequestProof> {
        validate_timestamp("now", now)?;
        // 防御性再规范化：签名原文只允许唯一写法。
        let meta = RequestMeta::canonical(&meta.method, &meta.path, &meta.query)?;
        let (nonce, signature) = decode_signed_auth(auth)?;
        let nonce_digest = sha256(&nonce);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<(Vec<u8>, i64, Option<i64>, String, String, i64)> = transaction
            .query_row(
                "SELECT nonce_hash, expires_at, consumed_at, node_id, purpose, issued_at
                 FROM auth_challenges WHERE challenge_id = ?1",
                [&auth.challenge_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((stored_digest, expires_at, consumed_at, node_id, purpose, issued_at)) = row else {
            return Err(NodeError::InvalidChallenge);
        };
        if stored_digest.as_slice() != nonce_digest {
            return Err(NodeError::InvalidChallenge);
        }
        if node_id != self.node_id {
            return Err(NodeError::NodeIdMismatch);
        }
        let purpose = ChallengePurpose::parse(&purpose)?;
        if purpose != expected_purpose {
            return Err(NodeError::InvalidChallenge);
        }
        let expires_at = u64::try_from(expires_at).map_err(|_| NodeError::InvalidChallenge)?;
        let issued_at = u64::try_from(issued_at).map_err(|_| NodeError::InvalidChallenge)?;
        if issued_at > now.saturating_add(self.policy.clock_skew_seconds) {
            return Err(NodeError::InvalidChallenge);
        }
        if consumed_at.is_some() {
            return Err(NodeError::AuthReplay);
        }
        if now >= expires_at {
            return Err(NodeError::AuthExpired);
        }
        let signed_bytes = request_signing_bytes(
            &auth.device_id,
            &meta,
            body_hash,
            &nonce,
            &self.node_id,
            purpose,
        );
        verify_ed25519(device_public_key, &signed_bytes, &signature, || {
            NodeError::InvalidSignature
        })?;
        let consumed = transaction.execute(
            "UPDATE auth_challenges SET consumed_at = ?2 WHERE challenge_id = ?1 AND consumed_at IS NULL",
            params![auth.challenge_id, to_i64("now", now)?],
        )?;
        if consumed != 1 {
            return Err(NodeError::AuthReplay);
        }
        transaction.commit()?;
        Ok(VerifiedRequestProof {
            device_id: auth.device_id.clone(),
            challenge_id: auth.challenge_id.clone(),
            device_public_key: *device_public_key,
            registered_account_id,
        })
    }

    fn load_device(&self, device_id: &str) -> NodeResult<Option<DeviceRow>> {
        validate_bounded_id("device id", device_id, MAX_DEVICE_ID_BYTES)?;
        let row: Option<(String, i64, String, Option<Vec<u8>>)> = self
            .connection
            .query_row(
                "SELECT d.account_id, d.credential_version, d.state, k.public_key
                 FROM devices d LEFT JOIN device_public_keys k ON k.device_id = d.device_id
                 WHERE d.device_id = ?1",
                [device_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        row.map(|(account_id, credential_version, state, public_key)| {
            let credential_version =
                u64::try_from(credential_version).map_err(|_| NodeError::StorageUnavailable)?;
            let public_key = match public_key {
                Some(bytes) => Some(
                    <[u8; 32]>::try_from(bytes.as_slice())
                        .map_err(|_| NodeError::InvalidCredential)?,
                ),
                None => None,
            };
            Ok(DeviceRow {
                account_id,
                state,
                credential_version,
                public_key,
            })
        })
        .transpose()
    }

    /// NODE-001：验证主管授权并幂等登记设备公开信息。
    pub fn register_device(
        &mut self,
        request: &RegisterDeviceRequest,
        proof: &VerifiedRequestProof,
        now: u64,
    ) -> NodeResult<DeviceRegistration> {
        validate_timestamp("now", now)?;
        request.validate_shape()?;
        let credential = &request.credential;
        let capability = &request.capability;
        validate_timestamp("credential version", credential.credential_version)?;
        validate_timestamp("capability revocation version", capability.revocation_version)?;
        if proof.device_id != credential.device_id
            || proof.device_public_key != credential.device_key()?
        {
            return Err(NodeError::PermissionDenied);
        }
        if let Some(account_id) = proof.registered_account_id() {
            if account_id != request.account_id {
                return Err(NodeError::PermissionDenied);
            }
        }
        credential.validate_lifetime()?;
        capability.validate_lifetime()?;
        let skew = self.policy.clock_skew_seconds;
        if credential.not_before > now.saturating_add(skew) {
            return Err(NodeError::InvalidCredential);
        }
        if credential.expires_at <= now {
            return Err(NodeError::InvalidCredential);
        }
        if capability.issued_at > now.saturating_add(skew) {
            return Err(NodeError::InvalidCapability);
        }
        if capability.expires_at <= now {
            return Err(NodeError::InvalidCapability);
        }
        credential.verify()?;
        capability.verify()?;
        let supervisor = credential.supervisor_key()?;
        let device_public_key = credential.device_key()?;
        let credential_digest = credential.digest()?;
        let credential_json = serde_json::to_vec(credential)?;
        let credential_version = to_i64("credential version", credential.credential_version)?;
        let now_sql = to_i64("now", now)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // 账户主管公钥按账户固定：首次注册固定，之后不一致即拒绝。
        let pinned: Option<Vec<u8>> = transaction
            .query_row(
                "SELECT supervisor_public_key FROM account_supervisors WHERE account_id = ?1",
                [&request.account_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(pinned) = pinned {
            if pinned.as_slice() != supervisor.as_slice() {
                return Err(NodeError::InvalidCredential);
            }
        }
        // 能力撤销版本。
        let revoked_version: Option<i64> = transaction
            .query_row(
                "SELECT version FROM capability_revocations
                 WHERE issuer_id = ?1 AND capability_id = ?2",
                params![request.account_id, capability.capability_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(version) = revoked_version {
            let version = u64::try_from(version).map_err(|_| NodeError::StorageUnavailable)?;
            if version >= capability.revocation_version {
                return Err(NodeError::InvalidCapability);
            }
        }
        // 重复注册：同摘要同结果；同设备换凭据拒绝。
        let existing: Option<(String, Vec<u8>, String, i64, String, Option<Vec<u8>>, Option<Vec<u8>>)> =
            transaction
                .query_row(
                    "SELECT d.account_id, d.credential, d.endpoint_id, d.credential_version, d.state,
                            g.credential_hash, g.receipt
                     FROM devices d
                     LEFT JOIN device_registrations g ON g.device_id = d.device_id
                     WHERE d.device_id = ?1",
                    [&credential.device_id],
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
        if let Some((account_id, stored_credential, endpoint_id, version, state, hash, receipt)) =
            existing
        {
            if state != "active" {
                return Err(NodeError::DeviceRevoked);
            }
            let same = account_id == request.account_id
                && endpoint_id == credential.endpoint_id
                && version == credential_version
                && stored_credential == credential_json
                && hash.as_deref() == Some(credential_digest.as_slice());
            if !same {
                return Err(NodeError::InvalidCredential);
            }
            let receipt = receipt.ok_or(NodeError::StorageUnavailable)?;
            let registration: DeviceRegistration = serde_json::from_slice(&receipt)?;
            registration.verify(&self.node_public_key)?;
            if registration.account_id != request.account_id
                || registration.device_id != credential.device_id
                || registration.credential_version != credential.credential_version
            {
                return Err(NodeError::StorageUnavailable);
            }
            transaction.commit()?;
            return Ok(registration);
        }
        // 注册能力只能被一台设备消费。
        let consumed: Option<(String, String)> = transaction
            .query_row(
                "SELECT account_id, device_id FROM consumed_registration_capabilities
                 WHERE capability_id = ?1",
                [&capability.capability_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((account_id, device_id)) = consumed {
            if account_id != request.account_id || device_id != credential.device_id {
                return Err(NodeError::InvalidCapability);
            }
        }
        // 账户设备上限：超过即明确拒绝，不静默替换旧设备。
        let active: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM devices WHERE account_id = ?1 AND state = 'active'",
            [&request.account_id],
            |row| row.get(0),
        )?;
        if usize::try_from(active).unwrap_or(usize::MAX) >= self.policy.max_devices_per_account {
            return Err(NodeError::DeviceLimitExceeded);
        }
        transaction.execute(
            "INSERT OR IGNORE INTO account_supervisors (account_id, supervisor_public_key, pinned_at)
             VALUES (?1, ?2, ?3)",
            params![request.account_id, supervisor.as_slice(), now_sql],
        )?;
        transaction.execute(
            "INSERT INTO devices
                 (device_id, account_id, credential, endpoint_id, credential_version, state)
             VALUES (?1, ?2, ?3, ?4, ?5, 'active')",
            params![
                credential.device_id,
                request.account_id,
                credential_json,
                credential.endpoint_id,
                credential_version
            ],
        )?;
        transaction.execute(
            "INSERT INTO device_public_keys (device_id, public_key) VALUES (?1, ?2)",
            params![credential.device_id, device_public_key.as_slice()],
        )?;
        transaction.execute(
            "INSERT INTO consumed_registration_capabilities
                 (capability_id, account_id, device_id, consumed_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                capability.capability_id,
                request.account_id,
                credential.device_id,
                now_sql
            ],
        )?;
        let mut registration = DeviceRegistration {
            account_id: request.account_id.clone(),
            device_id: credential.device_id.clone(),
            device_public_key: credential.device_public_key.clone(),
            endpoint_id: credential.endpoint_id.clone(),
            credential_version: credential.credential_version,
            registered_at: now,
            node_id: self.node_id.clone(),
            node_public_key: hex_encode(&self.node_public_key),
            signature: String::new(),
        };
        let signature: Signature = self.signing_key.sign(&registration.signing_bytes()?);
        registration.signature = hex_encode(&signature.to_bytes());
        transaction.execute(
            "INSERT INTO device_registrations
                 (device_id, account_id, credential_hash, registered_at, receipt)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                credential.device_id,
                request.account_id,
                credential_digest.as_slice(),
                now_sql,
                serde_json::to_vec(&registration)?
            ],
        )?;
        transaction.commit()?;
        Ok(registration)
    }
}

/// 校验注册回执与设备记录一致（供客户端/节点运维独立验证已登记状态）。
pub fn verify_device_registration(
    expected_node_public_key: &[u8; 32],
    registration: &DeviceRegistration,
) -> NodeResult<()> {
    registration.verify(expected_node_public_key)
}

pub(crate) fn to_i64(field: &'static str, value: u64) -> NodeResult<i64> {
    i64::try_from(value).map_err(|_| NodeError::InvalidRequest(field))
}

fn table_exists(connection: &Connection, name: &str) -> NodeResult<bool> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn checksum(sql: &str) -> String {
    hex_encode(&sha256(sql.as_bytes()))
}

fn migrate(connection: &Connection) -> NodeResult<()> {
    if !table_exists(connection, "schema_migrations")? {
        connection
            .execute_batch(MIGRATION_0001)
            .map_err(|_| NodeError::MigrationFailed("initial migration failed"))?;
    }
    if !table_exists(connection, "devices")? {
        return Err(NodeError::MigrationFailed(
            "initial schema is missing the devices table",
        ));
    }
    let current: i64 = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    if current < 1 {
        connection.execute(
            "INSERT OR REPLACE INTO schema_migrations (version, checksum, applied_at)
             VALUES (1, ?1, CAST(strftime('%s','now') AS INTEGER))",
            params![checksum(MIGRATION_0001)],
        )?;
    } else {
        verify_migration_checksum(connection, 1, MIGRATION_0001)?;
    }
    let current: i64 = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    if current < 2 {
        let sql = MIGRATION_0002.replace(MIGRATION_CHECKSUM_SLOT, &checksum(MIGRATION_0002));
        connection
            .execute_batch(&sql)
            .map_err(|_| NodeError::MigrationFailed("node auth migration failed"))?;
        let applied: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if applied < 2 {
            return Err(NodeError::MigrationFailed(
                "node auth migration did not record its version",
            ));
        }
    }
    verify_migration_checksum(connection, 2, MIGRATION_0002)?;
    let current: i64 = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    if current < 3 {
        let sql = MIGRATION_0003.replace(MIGRATION_CHECKSUM_SLOT, &checksum(MIGRATION_0003));
        connection
            .execute_batch(&sql)
            .map_err(|_| NodeError::MigrationFailed("group and mailbox migration failed"))?;
        let applied: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if applied < 3 {
            return Err(NodeError::MigrationFailed(
                "group and mailbox migration did not record its version",
            ));
        }
    }
    verify_migration_checksum(connection, 3, MIGRATION_0003)?;
    let current: i64 = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    if current < 4 {
        let sql = MIGRATION_0004.replace(MIGRATION_CHECKSUM_SLOT, &checksum(MIGRATION_0004));
        connection
            .execute_batch(&sql)
            .map_err(|_| NodeError::MigrationFailed("blob attachment migration failed"))?;
        let applied: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if applied < 4 {
            return Err(NodeError::MigrationFailed(
                "blob attachment migration did not record its version",
            ));
        }
    }
    verify_migration_checksum(connection, 4, MIGRATION_0004)?;
    let current: i64 = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    if current < 5 {
        let sql = MIGRATION_0005.replace(MIGRATION_CHECKSUM_SLOT, &checksum(MIGRATION_0005));
        connection
            .execute_batch(&sql)
            .map_err(|_| NodeError::MigrationFailed("retention migration failed"))?;
        let applied: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if applied < 5 {
            return Err(NodeError::MigrationFailed(
                "retention migration did not record its version",
            ));
        }
    }
    verify_migration_checksum(connection, 5, MIGRATION_0005)?;
    let current: i64 = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    if current < 6 {
        let sql = MIGRATION_0006.replace(MIGRATION_CHECKSUM_SLOT, &checksum(MIGRATION_0006));
        connection
            .execute_batch(&sql)
            .map_err(|_| NodeError::MigrationFailed("domain operation migration failed"))?;
        let applied: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if applied < 6 {
            return Err(NodeError::MigrationFailed(
                "domain operation migration did not record its version",
            ));
        }
    }
    verify_migration_checksum(connection, 6, MIGRATION_0006)?;
    let current: i64 = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    if current < 7 {
        let sql = MIGRATION_0007.replace(MIGRATION_CHECKSUM_SLOT, &checksum(MIGRATION_0007));
        connection
            .execute_batch(&sql)
            .map_err(|_| NodeError::MigrationFailed("capability revocation migration failed"))?;
        let applied: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if applied < 7 {
            return Err(NodeError::MigrationFailed(
                "capability revocation migration did not record its version",
            ));
        }
    }
    verify_migration_checksum(connection, 7, MIGRATION_0007)?;
    Ok(())
}

fn verify_migration_checksum(connection: &Connection, version: i64, sql: &str) -> NodeResult<()> {
    let stored: Option<String> = connection
        .query_row(
            "SELECT checksum FROM schema_migrations WHERE version = ?1",
            [version],
            |row| row.get(0),
        )
        .optional()?;
    match stored {
        Some(stored) if stored == checksum(sql) => Ok(()),
        _ => Err(NodeError::MigrationFailed("migration checksum mismatch")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{ChallengePurpose, RequestMeta, SignedAuth, request_signing_bytes};
    use crate::canonical::{hex_decode, sha256};
    use crate::registration::{
        DeviceCredential, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest,
        RegistrationCapability,
    };
    use ed25519_dalek::{Signature, Signer, SigningKey};

    const NOW: u64 = 1_700_000_000;

    fn supervisor() -> SigningKey {
        SigningKey::from_bytes(&[11_u8; 32])
    }

    fn service() -> NodeService {
        NodeService::open_in_memory(NodeServiceConfig {
            node_id: "node-unit-1".into(),
            signing_seed: [7_u8; 32],
            policy: NodeAuthPolicy::default(),
        })
        .expect("node opens")
    }

    fn credential(
        supervisor: &SigningKey,
        account_id: &str,
        device_id: &str,
        device: &SigningKey,
        now: u64,
    ) -> DeviceCredential {
        let mut credential = DeviceCredential {
            account_id: account_id.to_owned(),
            device_id: device_id.to_owned(),
            device_public_key: hex_encode(&device.verifying_key().to_bytes()),
            endpoint_id: format!("endpoint-{device_id}"),
            credential_version: 1,
            not_before: now - 10,
            expires_at: now + 3_600,
            supervisor_public_key: hex_encode(&supervisor.verifying_key().to_bytes()),
            signature: String::new(),
        };
        let signature: Signature = supervisor.sign(&credential.signing_bytes().unwrap());
        credential.signature = hex_encode(&signature.to_bytes());
        credential
    }

    fn capability(
        supervisor: &SigningKey,
        account_id: &str,
        capability_id: &str,
        device_id: &str,
        revocation_version: u64,
        now: u64,
    ) -> RegistrationCapability {
        let mut capability = RegistrationCapability {
            capability_id: capability_id.to_owned(),
            account_id: account_id.to_owned(),
            device_id: device_id.to_owned(),
            purpose: REGISTRATION_CAPABILITY_PURPOSE.to_owned(),
            issued_at: now - 10,
            expires_at: now + 600,
            revocation_version,
            supervisor_public_key: hex_encode(&supervisor.verifying_key().to_bytes()),
            signature: String::new(),
        };
        let signature: Signature = supervisor.sign(&capability.signing_bytes().unwrap());
        capability.signature = hex_encode(&signature.to_bytes());
        capability
    }

    fn signed_registration(
        service: &mut NodeService,
        supervisor: &SigningKey,
        account_id: &str,
        device_id: &str,
        device: &SigningKey,
        capability_id: &str,
        revocation_version: u64,
        now: u64,
    ) -> (RegisterDeviceRequest, VerifiedRequestProof) {
        let request = RegisterDeviceRequest {
            account_id: account_id.to_owned(),
            credential: credential(supervisor, account_id, device_id, device, now),
            capability: capability(
                supervisor,
                account_id,
                capability_id,
                device_id,
                revocation_version,
                now,
            ),
        };
        let body_hash = sha256(&serde_json::to_vec(&request).unwrap());
        let challenge = service
            .issue_auth_challenge(ChallengePurpose::DeviceRegistration, now)
            .unwrap();
        let nonce = hex_decode::<32>("nonce", &challenge.nonce).unwrap();
        let meta = RequestMeta::canonical("POST", "/v1/devices", "").unwrap();
        let signed = request_signing_bytes(
            device_id,
            &meta,
            &body_hash,
            &nonce,
            service.node_id(),
            ChallengePurpose::DeviceRegistration,
        );
        let signature: Signature = device.sign(&signed);
        let auth = SignedAuth {
            challenge_id: challenge.challenge_id,
            device_id: device_id.to_owned(),
            nonce: challenge.nonce,
            signature: hex_encode(&signature.to_bytes()),
        };
        let proof = service
            .verify_device_signature(
                &meta,
                &body_hash,
                &auth,
                &device.verifying_key().to_bytes(),
                ChallengePurpose::DeviceRegistration,
                now,
            )
            .unwrap();
        (request, proof)
    }

    /// 能力撤销版本达到或超过能力版本时必须拒绝。
    #[test]
    fn revoked_capability_version_is_rejected() {
        let mut service = service();
        let supervisor = supervisor();
        service
            .connection
            .execute(
                "INSERT INTO capability_revocations (issuer_id, capability_id, version, revoked_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params!["acct-1", "cap-1", 2_i64, NOW as i64],
            )
            .unwrap();
        let device = SigningKey::from_bytes(&[21_u8; 32]);
        let (request, proof) = signed_registration(
            &mut service,
            &supervisor,
            "acct-1",
            "dev-1",
            &device,
            "cap-1",
            2,
            NOW,
        );
        assert_eq!(
            service.register_device(&request, &proof, NOW).unwrap_err().code(),
            "INVALID_CAPABILITY"
        );
        // 更高版本的能力不受旧撤销记录影响。
        let (request, proof) = signed_registration(
            &mut service,
            &supervisor,
            "acct-1",
            "dev-1",
            &device,
            "cap-1",
            3,
            NOW,
        );
        assert!(service.register_device(&request, &proof, NOW).is_ok());
    }

    /// 挑战绑定的 node_id 被篡改后必须按节点不匹配拒绝。
    #[test]
    fn tampered_challenge_node_binding_is_rejected() {
        let mut service = service();
        let device = SigningKey::from_bytes(&[22_u8; 32]);
        let supervisor = supervisor();
        let (request, _) = signed_registration(
            &mut service,
            &supervisor,
            "acct-1",
            "dev-1",
            &device,
            "cap-1",
            1,
            NOW,
        );
        let body_hash = sha256(&serde_json::to_vec(&request).unwrap());
        let challenge = service
            .issue_auth_challenge(ChallengePurpose::DeviceRegistration, NOW)
            .unwrap();
        let nonce = hex_decode::<32>("nonce", &challenge.nonce).unwrap();
        let meta = RequestMeta::canonical("POST", "/v1/devices", "").unwrap();
        let signed = request_signing_bytes(
            "dev-1",
            &meta,
            &body_hash,
            &nonce,
            service.node_id(),
            ChallengePurpose::DeviceRegistration,
        );
        let signature: Signature = device.sign(&signed);
        let auth = SignedAuth {
            challenge_id: challenge.challenge_id.clone(),
            device_id: "dev-1".into(),
            nonce: challenge.nonce.clone(),
            signature: hex_encode(&signature.to_bytes()),
        };
        service
            .connection
            .execute(
                "UPDATE auth_challenges SET node_id = 'node-forged' WHERE challenge_id = ?1",
                [&challenge.challenge_id],
            )
            .unwrap();
        let error = service
            .verify_device_signature(
                &meta,
                &body_hash,
                &auth,
                &device.verifying_key().to_bytes(),
                ChallengePurpose::DeviceRegistration,
                NOW,
            )
            .expect_err("tampered node binding rejected");
        assert_eq!(error.code(), "NODE_ID_MISMATCH");
    }

    /// 迁移必须记录校验和；schema 被改动时报 MIGRATION_FAILED 而不是继续运行。
    #[test]
    fn migration_checksum_drift_is_rejected() {
        let directory = std::env::temp_dir().join(format!("chat-node-migration-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("node.db");
        let config = NodeServiceConfig {
            node_id: "node-migration".into(),
            signing_seed: [8_u8; 32],
            policy: NodeAuthPolicy::default(),
        };
        {
            let service = NodeService::open(&path, config.clone()).expect("first open");
            drop(service);
        }
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute("UPDATE schema_migrations SET checksum = 'drifted' WHERE version = 2", [])
                .unwrap();
        }
        assert_eq!(
            NodeService::open(&path, config).unwrap_err().code(),
            "MIGRATION_FAILED"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }
}