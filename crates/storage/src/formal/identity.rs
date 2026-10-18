//! 身份账户、联系人与邀请一次性消费的正式持久化（P1 增量）。
//!
//! 表结构与隐私边界（迁移 [0003_identity.sql]）：**敏感字段只以密封记录落盘**，
//! 明文列只有不可逆索引哈希与时间戳：
//!
//! - `identity_account`：单行（`id = 1`）单账户/单设备记录。`protected_account` 内含
//!   账户/设备私钥种子、账户/设备公钥、账户/设备 ID、显示名与核验指纹；种子必须
//!   先经 [`SealPurpose::IdentityAccount`] 封装才允许落盘，`account_index` 只是
//!   账户 ID 的域分隔哈希（用于快速冲突判定，不可逆推账户 ID）。
//! - `contacts`：`contact_id` 是 `account_id` 的域分隔哈希派生键（幂等键），
//!   对端账户 ID、显示名、观测指纹与联系人状态都封装在 `protected_record`
//!   （[`SealPurpose::ContactRecord`]）。
//! - `consumed_invites`：`invite_id` 主键 + 邀请摘要；重复消费由主键冲突拒绝。
//!
//! AAD 一律绑定行身份（账户索引 / contact_id）与记录格式版本，因此把密文挪到
//! 另一行、改成另一版本或换主密钥都会认证失败（`STATE_CORRUPTED`）。
//!
//! 本层只做结构/边界校验，不做密码学校验（账户 ID 与公钥、种子之间的自洽由 core
//! 用 chat-identity 在写入前验证）；未知或畸形状态显式拒绝，绝不猜测性降级。

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use super::error::StorageError;
use super::seal::{ProtectedStorageKey, RecordSealer, SealPurpose};
use super::{
    Result, Store, WriteScope, commit_transaction, digest_parts, ensure_id, hex, now_ms,
    rollback_transaction,
};

/// 账户显示名上限（与 `chat_identity::MAX_DISPLAY_NAME_BYTES` 一致）。
pub const MAX_IDENTITY_DISPLAY_NAME_BYTES: usize = 64;
/// 联系人记录条数上限（超过即显式拒绝，不静默丢弃）。
pub const MAX_CONTACTS: u32 = 4096;
/// 账户私钥种子字节数。
pub const ACCOUNT_SEED_BYTES: usize = 32;
/// 设备私钥种子字节数。
pub const DEVICE_SEED_BYTES: usize = 32;
/// 公钥字节数（Ed25519）。
pub const PUBLIC_KEY_BYTES: usize = 32;
/// 核验指纹字节数。
pub const FINGERPRINT_BYTES: usize = 32;
/// 邀请摘要字节数。
pub const INVITE_DIGEST_BYTES: usize = 32;

/// 身份账户密封记录格式版本（进入 AAD）。
const IDENTITY_AAD_VERSION: &[u8] = b"identity-account-v1";
/// 联系人密封记录格式版本（进入 AAD）。
const CONTACT_AAD_VERSION: &[u8] = b"contact-record-v1";
/// 身份账户记录内部 schema 版本。
const IDENTITY_RECORD_VERSION: u8 = 1;
/// 联系人记录内部 schema 版本。
const CONTACT_RECORD_VERSION: u8 = 1;
/// 账户索引派生域。
const ACCOUNT_INDEX_DOMAIN: &[u8] = b"chat/storage/identity-account-index/v1";
/// 联系人键派生域。
const CONTACT_ID_DOMAIN: &[u8] = b"chat/storage/contact-id/v1";

/// 允许的联系人状态（与 identity 的冻结状态机一致）。
pub const CONTACT_STATES: [&str; 5] = [
    "requested",
    "accepted",
    "declined",
    "blocked",
    "identity_changed",
];

/// 身份账户写入输入。
///
/// `account_seed` / `device_seed` 是机密材料：本类型不实现 `Clone`，`Debug` 只打印
/// 长度；storage 内部立即转为 `Zeroizing` 缓冲并在返回前清零，调用方仍需清理
/// 自己持有的副本。
pub struct IdentityAccountInput {
    /// 账户 ID（PROTOCOL-001 文本或存储可接受的有界 ID）。
    pub account_id: String,
    /// 设备 ID。
    pub device_id: String,
    /// 账户长期公钥。
    pub account_public_key: [u8; PUBLIC_KEY_BYTES],
    /// 设备签名公钥。
    pub device_public_key: [u8; PUBLIC_KEY_BYTES],
    /// 账户私钥种子（32 字节；封装后才落盘）。
    pub account_seed: Vec<u8>,
    /// 设备私钥种子（32 字节；封装后才落盘）。
    pub device_seed: Vec<u8>,
    /// 账户显示名（1..=64 字节，不含控制字符）。
    pub display_name: String,
    /// 账户核验指纹。
    pub fingerprint: [u8; FINGERPRINT_BYTES],
    /// 账户创建时间（Unix 毫秒；只在首次写入时使用）。
    pub created_at_ms: i64,
}

impl std::fmt::Debug for IdentityAccountInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IdentityAccountInput")
            .field("account_id", &self.account_id)
            .field("device_id", &self.device_id)
            .field("display_name", &self.display_name)
            .field("account_seed_len", &self.account_seed.len())
            .field("device_seed_len", &self.device_seed.len())
            .finish_non_exhaustive()
    }
}

impl IdentityAccountInput {
    /// 有界结构校验（密码学自洽由 core 负责）。
    pub fn validate(&self) -> Result<()> {
        ensure_id("identity account", &self.account_id)?;
        ensure_id("identity device", &self.device_id)?;
        validate_display_name(&self.display_name)?;
        if self.account_seed.len() != ACCOUNT_SEED_BYTES {
            return Err(StorageError::InvalidInput(format!(
                "account seed must be exactly {ACCOUNT_SEED_BYTES} bytes"
            )));
        }
        if self.device_seed.len() != DEVICE_SEED_BYTES {
            return Err(StorageError::InvalidInput(format!(
                "device seed must be exactly {DEVICE_SEED_BYTES} bytes"
            )));
        }
        if self.created_at_ms < 0 {
            return Err(StorageError::InvalidInput(
                "created_at_ms must be a non-negative Unix millisecond timestamp".to_owned(),
            ));
        }
        Ok(())
    }
}

/// 身份账户投影：**不含任何种子或私钥材料**。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityAccountRecord {
    /// 账户 ID。
    pub account_id: String,
    /// 设备 ID。
    pub device_id: String,
    /// 显示名。
    pub display_name: String,
    /// 核验指纹。
    pub fingerprint: [u8; FINGERPRINT_BYTES],
    /// 创建时间。
    pub created_at_ms: i64,
    /// 最近更新时间。
    pub updated_at_ms: i64,
    /// 本次调用是否命中既有记录（读取路径恒为 `true`）。
    pub already_existed: bool,
}

/// 联系人写入输入。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactUpsertInput {
    /// 对端账户 ID。
    pub account_id: String,
    /// 显示名（1..=64 字节，不含控制字符）。
    pub display_name: String,
    /// 观测/固定指纹（32 字节）。
    pub fingerprint: [u8; FINGERPRINT_BYTES],
    /// 联系人状态（见 [`CONTACT_STATES`]）。
    pub state: String,
    /// 事件时间（Unix 毫秒，用于列表排序）。
    pub updated_at_ms: i64,
}

impl ContactUpsertInput {
    /// 有界结构校验。
    pub fn validate(&self) -> Result<()> {
        ensure_id("contact account", &self.account_id)?;
        validate_display_name(&self.display_name)?;
        if !CONTACT_STATES.contains(&self.state.as_str()) {
            return Err(StorageError::InvalidInput(
                "contact state is not one of the frozen identity states".to_owned(),
            ));
        }
        if self.updated_at_ms < 0 {
            return Err(StorageError::InvalidInput(
                "updated_at_ms must be a non-negative Unix millisecond timestamp".to_owned(),
            ));
        }
        Ok(())
    }
}

/// 联系人投影（明文只在内存中出现）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactRecord {
    /// 派生联系人键（幂等键）。
    pub contact_id: String,
    /// 对端账户 ID。
    pub account_id: String,
    /// 显示名。
    pub display_name: String,
    /// 观测/固定指纹。
    pub fingerprint: [u8; FINGERPRINT_BYTES],
    /// 联系人状态。
    pub state: String,
    /// 事件时间。
    pub updated_at_ms: i64,
    /// 本次调用是否命中既有记录（读取路径恒为 `true`）。
    pub already_existed: bool,
}

/// 邀请消费结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsumedInviteRecord {
    /// 邀请 ID。
    pub invite_id: String,
    /// 消费时间。
    pub consumed_at_ms: i64,
}

fn validate_display_name(display_name: &str) -> Result<()> {
    let trimmed = display_name.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_IDENTITY_DISPLAY_NAME_BYTES {
        return Err(StorageError::InvalidInput(format!(
            "display name must be 1..={MAX_IDENTITY_DISPLAY_NAME_BYTES} bytes"
        )));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(StorageError::InvalidInput(
            "display name must not contain control characters".to_owned(),
        ));
    }
    Ok(())
}

fn account_index(account_id: &str) -> String {
    hex(&digest_parts(ACCOUNT_INDEX_DOMAIN, &[account_id.as_bytes()]))
}

/// 由账户 ID 派生确定性联系人键（幂等键；不可逆推出账户 ID）。
pub fn contact_id_for(account_id: &str) -> Result<String> {
    ensure_id("contact account", account_id)?;
    let digest = digest_parts(CONTACT_ID_DOMAIN, &[account_id.as_bytes()]);
    Ok(format!("contact-{}", hex(&digest[..16])))
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredIdentityAccount {
    version: u8,
    account_id: String,
    device_id: String,
    account_public_key: Vec<u8>,
    device_public_key: Vec<u8>,
    account_seed: Vec<u8>,
    device_seed: Vec<u8>,
    display_name: String,
    fingerprint: Vec<u8>,
    created_at_ms: i64,
}

impl Drop for StoredIdentityAccount {
    fn drop(&mut self) {
        self.account_seed.zeroize();
        self.device_seed.zeroize();
    }
}

impl StoredIdentityAccount {
    fn validate(&self) -> Result<()> {
        if self.version != IDENTITY_RECORD_VERSION {
            return Err(StorageError::StateCorrupted(
                "stored identity account has an unsupported record version".to_owned(),
            ));
        }
        ensure_id("identity account", &self.account_id)?;
        ensure_id("identity device", &self.device_id)?;
        if self.account_public_key.len() != PUBLIC_KEY_BYTES
            || self.device_public_key.len() != PUBLIC_KEY_BYTES
        {
            return Err(StorageError::StateCorrupted(
                "stored identity public key has an invalid length".to_owned(),
            ));
        }
        if self.account_seed.len() != ACCOUNT_SEED_BYTES || self.device_seed.len() != DEVICE_SEED_BYTES
        {
            return Err(StorageError::StateCorrupted(
                "stored identity seed has an invalid length".to_owned(),
            ));
        }
        if self.fingerprint.len() != FINGERPRINT_BYTES {
            return Err(StorageError::StateCorrupted(
                "stored identity fingerprint has an invalid length".to_owned(),
            ));
        }
        validate_display_name(&self.display_name).map_err(|_| {
            StorageError::StateCorrupted("stored identity display name is invalid".to_owned())
        })?;
        if self.created_at_ms < 0 {
            return Err(StorageError::StateCorrupted(
                "stored identity has a negative creation timestamp".to_owned(),
            ));
        }
        Ok(())
    }

    fn fingerprint_array(&self) -> Result<[u8; FINGERPRINT_BYTES]> {
        self.fingerprint.as_slice().try_into().map_err(|_| {
            StorageError::StateCorrupted(
                "stored identity fingerprint has an invalid length".to_owned(),
            )
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredContact {
    version: u8,
    account_id: String,
    display_name: String,
    fingerprint: Vec<u8>,
    state: String,
}

impl StoredContact {
    fn validate(&self) -> Result<()> {
        if self.version != CONTACT_RECORD_VERSION {
            return Err(StorageError::StateCorrupted(
                "stored contact has an unsupported record version".to_owned(),
            ));
        }
        ensure_id("contact account", &self.account_id)?;
        validate_display_name(&self.display_name).map_err(|_| {
            StorageError::StateCorrupted("stored contact display name is invalid".to_owned())
        })?;
        if self.fingerprint.len() != FINGERPRINT_BYTES {
            return Err(StorageError::StateCorrupted(
                "stored contact fingerprint has an invalid length".to_owned(),
            ));
        }
        if !CONTACT_STATES.contains(&self.state.as_str()) {
            return Err(StorageError::StateCorrupted(
                "stored contact state is not one of the frozen states".to_owned(),
            ));
        }
        Ok(())
    }

    fn fingerprint_array(&self) -> Result<[u8; FINGERPRINT_BYTES]> {
        self.fingerprint.as_slice().try_into().map_err(|_| {
            StorageError::StateCorrupted(
                "stored contact fingerprint has an invalid length".to_owned(),
            )
        })
    }
}

fn identity_aad(index: &str) -> [&[u8]; 2] {
    [index.as_bytes(), IDENTITY_AAD_VERSION]
}

fn contact_aad(contact_id: &str) -> [&[u8]; 2] {
    [contact_id.as_bytes(), CONTACT_AAD_VERSION]
}

fn seal_identity_account(
    key: &ProtectedStorageKey,
    index: &str,
    stored: &StoredIdentityAccount,
) -> Result<Vec<u8>> {
    let plaintext = serde_json::to_vec(stored)
        .map_err(|error| StorageError::Failure(format!("cannot encode identity record: {error}")))?;
    let plaintext = Zeroizing::new(plaintext);
    let sealer = RecordSealer::new(key, SealPurpose::IdentityAccount);
    sealer.seal(&identity_aad(index), plaintext.as_slice())
}

fn open_identity_account(
    key: &ProtectedStorageKey,
    index: &str,
    sealed: &[u8],
) -> Result<StoredIdentityAccount> {
    let sealer = RecordSealer::new(key, SealPurpose::IdentityAccount);
    let plaintext = sealer.open(&identity_aad(index), sealed)?;
    let stored: StoredIdentityAccount = serde_json::from_slice(plaintext.as_slice()).map_err(|_| {
        StorageError::StateCorrupted("stored identity account is not readable".to_owned())
    })?;
    stored.validate()?;
    Ok(stored)
}

fn seal_contact(key: &ProtectedStorageKey, contact_id: &str, stored: &StoredContact) -> Result<Vec<u8>> {
    let plaintext = serde_json::to_vec(stored)
        .map_err(|error| StorageError::Failure(format!("cannot encode contact record: {error}")))?;
    let plaintext = Zeroizing::new(plaintext);
    let sealer = RecordSealer::new(key, SealPurpose::ContactRecord);
    sealer.seal(&contact_aad(contact_id), plaintext.as_slice())
}

fn open_contact(key: &ProtectedStorageKey, contact_id: &str, sealed: &[u8]) -> Result<StoredContact> {
    let sealer = RecordSealer::new(key, SealPurpose::ContactRecord);
    let plaintext = sealer.open(&contact_aad(contact_id), sealed)?;
    let stored: StoredContact = serde_json::from_slice(plaintext.as_slice()).map_err(|_| {
        StorageError::StateCorrupted("stored contact record is not readable".to_owned())
    })?;
    stored.validate()?;
    Ok(stored)
}

fn load_identity_row(
    connection: &Connection,
    key: &ProtectedStorageKey,
) -> Result<Option<(StoredIdentityAccount, i64, i64)>> {
    let row: Option<(String, Vec<u8>, i64, i64)> = connection
        .query_row(
            "SELECT account_index, protected_account, created_at, updated_at
               FROM identity_account WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((index, sealed, created_at, updated_at)) = row else {
        return Ok(None);
    };
    if created_at < 0 || updated_at < 0 {
        return Err(StorageError::StateCorrupted(
            "stored identity timestamps are negative".to_owned(),
        ));
    }
    let stored = open_identity_account(key, &index, &sealed)?;
    Ok(Some((stored, created_at, updated_at)))
}

fn load_contact_row(
    connection: &Connection,
    key: &ProtectedStorageKey,
    contact_id: &str,
) -> Result<Option<(StoredContact, i64)>> {
    let row: Option<(Vec<u8>, i64)> = connection
        .query_row(
            "SELECT protected_record, updated_at FROM contacts WHERE contact_id = ?1",
            [contact_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((sealed, updated_at)) = row else {
        return Ok(None);
    };
    if updated_at < 0 {
        return Err(StorageError::StateCorrupted(
            "stored contact timestamp is negative".to_owned(),
        ));
    }
    let stored = open_contact(key, contact_id, &sealed)?;
    Ok(Some((stored, updated_at)))
}

fn identity_record(
    stored: &StoredIdentityAccount,
    created_at_ms: i64,
    updated_at_ms: i64,
    already_existed: bool,
) -> Result<IdentityAccountRecord> {
    Ok(IdentityAccountRecord {
        account_id: stored.account_id.clone(),
        device_id: stored.device_id.clone(),
        display_name: stored.display_name.clone(),
        fingerprint: stored.fingerprint_array()?,
        created_at_ms,
        updated_at_ms,
        already_existed,
    })
}

fn contact_record(
    contact_id: String,
    stored: &StoredContact,
    updated_at_ms: i64,
    already_existed: bool,
) -> Result<ContactRecord> {
    Ok(ContactRecord {
        contact_id,
        account_id: stored.account_id.clone(),
        display_name: stored.display_name.clone(),
        fingerprint: stored.fingerprint_array()?,
        state: stored.state.clone(),
        updated_at_ms,
        already_existed,
    })
}

fn map_invite_write_error(error: rusqlite::Error, invite_id: &str) -> StorageError {
    if matches!(
        &error,
        rusqlite::Error::SqliteFailure(code, _) if code.code == rusqlite::ErrorCode::ConstraintViolation
    ) {
        return StorageError::InvalidStateTransition(format!(
            "invite {invite_id} was already consumed"
        ));
    }
    error.into()
}

impl Store {
    /// 保存（或更新展示信息）单账户身份；同一账户重复保存同内容幂等。
    ///
    /// 语义：
    /// - 首次写入：插入单行记录（`created_at` 取输入）；
    /// - 同账户、同种子、同显示名/指纹/公钥：幂等返回，不写行、不推进库版本；
    /// - 同账户、密钥或显示信息不同：更新密封记录与 `updated_at`（`created_at` 保留）；
    /// - 另一账户：`IDENTITY_ACCOUNT_CONFLICT`，绝不覆盖已绑定身份。
    pub fn save_identity_account(
        &mut self,
        input: IdentityAccountInput,
    ) -> Result<IdentityAccountRecord> {
        self.ensure_ready()?;
        input.validate()?;
        let account_seed = Zeroizing::new(input.account_seed);
        let device_seed = Zeroizing::new(input.device_seed);
        let index = account_index(&input.account_id);

        if let Some((stored, created_at, updated_at)) =
            load_identity_row(self.connection(), &self.key)?
        {
            if stored.account_id != input.account_id {
                return Err(StorageError::IdentityAccountConflict(format!(
                    "identity store is already bound to account {}",
                    stored.account_id
                )));
            }
            if stored.account_seed.as_slice() != account_seed.as_slice()
                || stored.device_seed.as_slice() != device_seed.as_slice()
            {
                return Err(StorageError::IdentityAccountConflict(
                    "identity store is already bound to different key material".to_owned(),
                ));
            }
            let same_public_material = stored.account_public_key.as_slice()
                == input.account_public_key.as_slice()
                && stored.device_public_key.as_slice() == input.device_public_key.as_slice()
                && stored.fingerprint.as_slice() == input.fingerprint.as_slice();
            if same_public_material && stored.display_name == input.display_name {
                return identity_record(&stored, created_at, updated_at, true);
            }
            let next = StoredIdentityAccount {
                version: IDENTITY_RECORD_VERSION,
                account_id: input.account_id,
                device_id: input.device_id,
                account_public_key: input.account_public_key.to_vec(),
                device_public_key: input.device_public_key.to_vec(),
                account_seed: account_seed.to_vec(),
                device_seed: device_seed.to_vec(),
                display_name: input.display_name,
                fingerprint: input.fingerprint.to_vec(),
                created_at_ms: stored.created_at_ms,
            };
            let sealed = seal_identity_account(&self.key, &index, &next)?;
            let updated_at = now_ms();
            let mut transaction = self.begin_transaction(WriteScope::identity())?;
            let rows = match transaction.transaction.execute(
                "UPDATE identity_account
                    SET account_index = ?2, protected_account = ?3, updated_at = ?4
                  WHERE id = 1",
                params![1i64, index, sealed, updated_at],
            ) {
                Ok(rows) => rows,
                Err(error) => {
                    transaction.poison();
                    let _ = rollback_transaction(transaction);
                    return Err(error.into());
                }
            };
            if rows != 1 {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                return Err(StorageError::StateCorrupted(
                    "identity account row disappeared while updating".to_owned(),
                ));
            }
            transaction.note(rows);
            commit_transaction(transaction)?;
            return identity_record(&next, created_at, updated_at, true);
        }

        let stored = StoredIdentityAccount {
            version: IDENTITY_RECORD_VERSION,
            account_id: input.account_id,
            device_id: input.device_id,
            account_public_key: input.account_public_key.to_vec(),
            device_public_key: input.device_public_key.to_vec(),
            account_seed: account_seed.to_vec(),
            device_seed: device_seed.to_vec(),
            display_name: input.display_name,
            fingerprint: input.fingerprint.to_vec(),
            created_at_ms: input.created_at_ms,
        };
        let sealed = seal_identity_account(&self.key, &index, &stored)?;
        let updated_at = now_ms();
        let mut transaction = self.begin_transaction(WriteScope::identity())?;
        let inserted = transaction.transaction.execute(
            "INSERT INTO identity_account (id, account_index, protected_account, created_at, updated_at)
             VALUES (1, ?1, ?2, ?3, ?4)",
            params![index, sealed, input.created_at_ms, updated_at],
        );
        let rows = match inserted {
            Ok(rows) => rows,
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                return Err(error.into());
            }
        };
        transaction.note(rows);
        commit_transaction(transaction)?;
        identity_record(&stored, input.created_at_ms, updated_at, false)
    }

    /// 读取本机身份账户投影（不含任何种子/私钥）；未保存时返回 `None`。
    ///
    /// 密封记录被篡改、挪行或主密钥不匹配时一律 `STATE_CORRUPTED`，绝不猜测性降级。
    pub fn load_identity_account(&self) -> Result<Option<IdentityAccountRecord>> {
        self.ensure_ready()?;
        match load_identity_row(self.connection(), &self.key)? {
            Some((stored, created_at, updated_at)) => {
                identity_record(&stored, created_at, updated_at, true).map(Some)
            }
            None => Ok(None),
        }
    }

    /// 读取单个联系人（按对端账户 ID）；不存在返回 `None`。
    pub fn read_contact(&self, account_id: &str) -> Result<Option<ContactRecord>> {
        self.ensure_ready()?;
        let contact_id = contact_id_for(account_id)?;
        match load_contact_row(self.connection(), &self.key, &contact_id)? {
            Some((stored, updated_at)) => {
                contact_record(contact_id, &stored, updated_at, true).map(Some)
            }
            None => Ok(None),
        }
    }

    /// 按 `contact_id` 幂等写入联系人；同内容幂等返回，不同内容更新密封记录。
    pub fn upsert_contact(&mut self, input: ContactUpsertInput) -> Result<ContactRecord> {
        self.ensure_ready()?;
        input.validate()?;
        let contact_id = contact_id_for(&input.account_id)?;

        if let Some((stored, updated_at)) =
            load_contact_row(self.connection(), &self.key, &contact_id)?
        {
            if stored.account_id != input.account_id {
                return Err(StorageError::StateCorrupted(format!(
                    "contact {contact_id} is bound to a different account"
                )));
            }
            let unchanged = stored.display_name == input.display_name
                && stored.fingerprint.as_slice() == input.fingerprint.as_slice()
                && stored.state == input.state;
            if unchanged {
                return contact_record(contact_id, &stored, updated_at, true);
            }
            let next = StoredContact {
                version: CONTACT_RECORD_VERSION,
                account_id: input.account_id,
                display_name: input.display_name,
                fingerprint: input.fingerprint.to_vec(),
                state: input.state,
            };
            let sealed = seal_contact(&self.key, &contact_id, &next)?;
            let mut transaction = self.begin_transaction(WriteScope::identity())?;
            let rows = match transaction.transaction.execute(
                "UPDATE contacts SET protected_record = ?2, updated_at = ?3 WHERE contact_id = ?1",
                params![contact_id, sealed, input.updated_at_ms],
            ) {
                Ok(rows) => rows,
                Err(error) => {
                    transaction.poison();
                    let _ = rollback_transaction(transaction);
                    return Err(error.into());
                }
            };
            if rows != 1 {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                return Err(StorageError::StateCorrupted(
                    "contact row disappeared while updating".to_owned(),
                ));
            }
            transaction.note(rows);
            commit_transaction(transaction)?;
            return contact_record(contact_id, &next, input.updated_at_ms, true);
        }

        let existing: i64 = self
            .connection()
            .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))?;
        if existing >= i64::from(MAX_CONTACTS) {
            return Err(StorageError::InvalidInput(format!(
                "contact store reached the {MAX_CONTACTS} record limit"
            )));
        }
        let stored = StoredContact {
            version: CONTACT_RECORD_VERSION,
            account_id: input.account_id,
            display_name: input.display_name,
            fingerprint: input.fingerprint.to_vec(),
            state: input.state,
        };
        let sealed = seal_contact(&self.key, &contact_id, &stored)?;
        let mut transaction = self.begin_transaction(WriteScope::identity())?;
        let inserted = transaction.transaction.execute(
            "INSERT INTO contacts (contact_id, protected_record, updated_at) VALUES (?1, ?2, ?3)",
            params![contact_id, sealed, input.updated_at_ms],
        );
        let rows = match inserted {
            Ok(rows) => rows,
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                return Err(error.into());
            }
        };
        transaction.note(rows);
        commit_transaction(transaction)?;
        contact_record(contact_id, &stored, input.updated_at_ms, false)
    }

    /// 列出全部联系人（按事件时间降序，其次 `contact_id` 升序）。
    pub fn list_contacts(&self) -> Result<Vec<ContactRecord>> {
        self.ensure_ready()?;
        let limit = i64::from(MAX_CONTACTS) + 1;
        let mut statement = self.connection().prepare(
            "SELECT contact_id, protected_record, updated_at
               FROM contacts ORDER BY updated_at DESC, contact_id ASC LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut contacts = Vec::new();
        for row in rows {
            let (contact_id, sealed, updated_at) = row?;
            if updated_at < 0 {
                return Err(StorageError::StateCorrupted(
                    "stored contact timestamp is negative".to_owned(),
                ));
            }
            let stored = open_contact(&self.key, &contact_id, &sealed)?;
            contacts.push(contact_record(contact_id, &stored, updated_at, true)?);
        }
        if contacts.len() > usize::try_from(MAX_CONTACTS).unwrap_or(usize::MAX) {
            return Err(StorageError::StateCorrupted(
                "stored contact count exceeds the supported limit".to_owned(),
            ));
        }
        Ok(contacts)
    }

    /// 一次性消费邀请：同一 `invite_id` 只允许成功一次。
    ///
    /// 第二次（或同一 ID 但不同摘要）返回 `INVALID_STATE_TRANSITION`；插入失败整体
    /// 回滚，不存在“半消费”。
    pub fn consume_invite(
        &mut self,
        invite_id: &str,
        invite_digest: &[u8; INVITE_DIGEST_BYTES],
        consumed_at_ms: i64,
    ) -> Result<ConsumedInviteRecord> {
        self.ensure_ready()?;
        ensure_id("invite", invite_id)?;
        if consumed_at_ms < 0 {
            return Err(StorageError::InvalidInput(
                "consumed_at_ms must be a non-negative Unix millisecond timestamp".to_owned(),
            ));
        }
        let mut transaction = self.begin_transaction(WriteScope::identity())?;
        let existing: Option<Vec<u8>> = match transaction.connection().query_row(
            "SELECT invite_digest FROM consumed_invites WHERE invite_id = ?1",
            [invite_id],
            |row| row.get(0),
        ) {
            Ok(value) => Some(value),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(error) => {
                let _ = rollback_transaction(transaction);
                return Err(error.into());
            }
        };
        if let Some(stored) = existing {
            let _ = rollback_transaction(transaction);
            let reason = if stored.as_slice() == invite_digest.as_slice() {
                format!("invite {invite_id} was already consumed")
            } else {
                format!("invite {invite_id} is bound to a different invitation")
            };
            return Err(StorageError::InvalidStateTransition(reason));
        }
        let inserted = transaction.transaction.execute(
            "INSERT INTO consumed_invites (invite_id, invite_digest, consumed_at)
             VALUES (?1, ?2, ?3)",
            params![invite_id, invite_digest.as_slice(), consumed_at_ms],
        );
        let rows = match inserted {
            Ok(rows) => rows,
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                return Err(map_invite_write_error(error, invite_id));
            }
        };
        transaction.note(rows);
        commit_transaction(transaction)?;
        Ok(ConsumedInviteRecord {
            invite_id: invite_id.to_owned(),
            consumed_at_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_ids_are_deterministic_bounded_and_domain_separated() {
        let first = contact_id_for("acct-1").expect("contact id");
        assert_eq!(first, contact_id_for("acct-1").expect("contact id"));
        assert_ne!(first, contact_id_for("acct-2").expect("contact id"));
        assert!(first.starts_with("contact-"));
        assert!(first.len() <= super::super::MAX_ID_BYTES);
        assert_eq!(
            contact_id_for("bad id!").unwrap_err().code(),
            "INVALID_INPUT"
        );
        // 无明文账户 ID 回显。
        assert!(!first.contains("acct-1"));
    }

    #[test]
    fn identity_aad_binds_the_row_index() {
        assert_eq!(identity_aad("index-a")[0], b"index-a");
        assert_eq!(contact_aad("contact-x")[0], b"contact-x");
        assert_ne!(identity_aad("a"), identity_aad("b"));
    }
}
