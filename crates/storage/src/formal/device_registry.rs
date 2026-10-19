//! 设备登记与撤销集合（IDENTITY-011/012 的落库形态）。
//!
//! 语义：
//!
//! - `device_id` 是 PROTOCOL-001 设备文本（主键，天然幂等 upsert；重复登记只刷新 label/公钥与时间）；
//! - `device_public_key` 必须 32 字节（表 CHECK + 写路径双重约束）；
//! - 撤销是**单向**的：已经撤销的行**不会**因为重新登记而复活（upsert 保留 revoked 字段）；
//! - 撤销不删行：历史设备留在集合里，默认列表隐藏、`include_revoked` 时可见；
//! - 有界：每账户最多 [`MAX_DEVICE_REGISTRY_ROWS`] 行（越界显式 `INVALID_INPUT`）。

use rusqlite::{OptionalExtension, params};

use super::error::StorageError;
use super::now_ms;
use super::{Result, Store, WriteScope, commit_transaction, rollback_transaction};

/// 每账户的设备登记上限。
pub const MAX_DEVICE_REGISTRY_ROWS: usize = 512;
/// 设备公钥字节数。
pub const DEVICE_PUBLIC_KEY_BYTES: usize = 32;
/// 设备 ID 文本上限。
pub const MAX_DEVICE_ID_BYTES: usize = 128;
/// 账户 ID 文本上限。
pub const MAX_DEVICE_ACCOUNT_BYTES: usize = 128;
/// 设备标签上限（本地展示名）。
pub const MAX_DEVICE_LABEL_BYTES: usize = 64;
/// 撤销原因上限（诊断文本）。
pub const MAX_DEVICE_REASON_BYTES: usize = 128;

/// 一条设备登记记录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceRegistryRecord {
    /// 设备 ID（协议文本）。
    pub device_id: String,
    /// 所属账户 ID（协议文本）。
    pub account_id: String,
    /// 设备签名公钥（32 字节，非秘密）。
    pub device_public_key: [u8; DEVICE_PUBLIC_KEY_BYTES],
    /// 本地标签（可为空）。
    pub label: String,
    /// 登记时间（Unix 毫秒）。
    pub added_at_ms: i64,
    /// 撤销时间（`None` = 未撤销）。
    pub revoked_at_ms: Option<i64>,
    /// 撤销原因（诊断文本）。
    pub revoked_reason: Option<String>,
}

impl DeviceRegistryRecord {
    /// 是否已撤销。
    pub fn is_revoked(&self) -> bool {
        self.revoked_at_ms.is_some()
    }
}

/// 登记输入。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceRegistrationInput {
    /// 设备 ID（协议文本）。
    pub device_id: String,
    /// 所属账户 ID。
    pub account_id: String,
    /// 设备签名公钥。
    pub device_public_key: [u8; DEVICE_PUBLIC_KEY_BYTES],
    /// 本地标签。
    pub label: String,
}

fn validate_text(field: &str, value: &str, limit: usize) -> Result<()> {
    if value.is_empty() || value.len() > limit || value.chars().any(char::is_control) {
        return Err(StorageError::InvalidInput(format!(
            "{field} must be 1..={limit} printable bytes"
        )));
    }
    Ok(())
}

impl Store {
    /// 幂等登记一台设备（重复登记只刷新公钥/标签与更新时间，**不会**复活已撤销设备）。
    pub fn upsert_identity_device(
        &mut self,
        input: &DeviceRegistrationInput,
    ) -> Result<DeviceRegistryRecord> {
        self.ensure_ready()?;
        validate_text("device id", &input.device_id, MAX_DEVICE_ID_BYTES)?;
        validate_text("account id", &input.account_id, MAX_DEVICE_ACCOUNT_BYTES)?;
        if !input.label.is_empty() {
            validate_text("device label", &input.label, MAX_DEVICE_LABEL_BYTES)?;
        }
        let now = now_ms();
        let mut transaction = self.begin_transaction(WriteScope::inbound("identity_devices"))?;
        let outcome = (|| -> Result<DeviceRegistryRecord> {
            // 上限只在"新设备"上检查（重复登记必须始终可用，否则撤销/重装会卡住）。
            let existing: Option<(i64, Option<i64>, Option<String>)> = transaction
                .transaction
                .query_row(
                    "SELECT added_at_ms, revoked_at_ms, revoked_reason
                       FROM identity_devices WHERE device_id = ?1",
                    [input.device_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(StorageError::from)?;
            if existing.is_none() {
                let rows: i64 = transaction
                    .transaction
                    .query_row(
                        "SELECT COUNT(*) FROM identity_devices WHERE account_id = ?1",
                        [input.account_id.as_str()],
                        |row| row.get(0),
                    )
                    .map_err(StorageError::from)?;
                if rows as usize >= MAX_DEVICE_REGISTRY_ROWS {
                    return Err(StorageError::InvalidInput(
                        "device registry row limit reached for this account".to_owned(),
                    ));
                }
            }
            transaction
                .transaction
                .execute(
                    "INSERT INTO identity_devices
                        (device_id, account_id, device_public_key, label, added_at_ms,
                         revoked_at_ms, revoked_reason)
                     VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL)
                     ON CONFLICT(device_id) DO UPDATE SET
                         account_id = excluded.account_id,
                         device_public_key = excluded.device_public_key,
                         label = excluded.label",
                    params![
                        input.device_id,
                        input.account_id,
                        input.device_public_key.as_slice(),
                        input.label,
                        now
                    ],
                )
                .map_err(StorageError::from)?;
            let record = load_device(&transaction.transaction, &input.device_id)?;
            Ok(record)
        })();
        match outcome {
            Ok(record) => {
                commit_transaction(transaction)?;
                Ok(record)
            }
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }

    /// 列出一个账户的设备（`include_revoked == false` 时隐藏已撤销行）。
    pub fn list_identity_devices(
        &self,
        account_id: &str,
        include_revoked: bool,
    ) -> Result<Vec<DeviceRegistryRecord>> {
        self.ensure_ready()?;
        validate_text("account id", account_id, MAX_DEVICE_ACCOUNT_BYTES)?;
        let connection = self.connection();
        let sql = if include_revoked {
            "SELECT device_id, account_id, device_public_key, label, added_at_ms,
                    revoked_at_ms, revoked_reason
               FROM identity_devices WHERE account_id = ?1
               ORDER BY added_at_ms ASC, device_id ASC LIMIT ?2"
        } else {
            "SELECT device_id, account_id, device_public_key, label, added_at_ms,
                    revoked_at_ms, revoked_reason
               FROM identity_devices WHERE account_id = ?1 AND revoked_at_ms IS NULL
               ORDER BY added_at_ms ASC, device_id ASC LIMIT ?2"
        };
        let mut statement = connection.prepare(sql).map_err(StorageError::from)?;
        let rows = statement
            .query_map(
                params![account_id, MAX_DEVICE_REGISTRY_ROWS as i64],
                device_from_row,
            )
            .map_err(StorageError::from)?;
        let mut devices = Vec::new();
        for row in rows {
            devices.push(row.map_err(StorageError::from)??);
        }
        Ok(devices)
    }

    /// 撤销一台设备（幂等；已撤销则保持首次的撤销时间与原因）。返回是否本次真的撤销。
    pub fn revoke_identity_device(
        &mut self,
        device_id: &str,
        reason: &str,
        now_ms: i64,
    ) -> Result<bool> {
        self.ensure_ready()?;
        validate_text("device id", device_id, MAX_DEVICE_ID_BYTES)?;
        validate_text("revocation reason", reason, MAX_DEVICE_REASON_BYTES)?;
        if now_ms < 0 {
            return Err(StorageError::InvalidInput(
                "revocation timestamp must be non-negative".to_owned(),
            ));
        }
        let mut transaction = self.begin_transaction(WriteScope::inbound("identity_devices"))?;
        let outcome = (|| -> Result<bool> {
            let rows = transaction
                .transaction
                .execute(
                    "UPDATE identity_devices
                        SET revoked_at_ms = ?2, revoked_reason = ?3
                      WHERE device_id = ?1 AND revoked_at_ms IS NULL",
                    params![device_id, now_ms, reason],
                )
                .map_err(StorageError::from)?;
            Ok(rows > 0)
        })();
        match outcome {
            Ok(revoked) => {
                commit_transaction(transaction)?;
                Ok(revoked)
            }
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }

    /// 设备是否已撤销（不存在 → `false`）。
    pub fn identity_device_is_revoked(&self, device_id: &str) -> Result<bool> {
        self.ensure_ready()?;
        validate_text("device id", device_id, MAX_DEVICE_ID_BYTES)?;
        let revoked: Option<Option<i64>> = self
            .connection()
            .query_row(
                "SELECT revoked_at_ms FROM identity_devices WHERE device_id = ?1",
                [device_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(StorageError::from)?;
        Ok(revoked.flatten().is_some())
    }

    /// 单台设备的登记记录（不存在 → `None`）。
    pub fn load_identity_device(&self, device_id: &str) -> Result<Option<DeviceRegistryRecord>> {
        self.ensure_ready()?;
        validate_text("device id", device_id, MAX_DEVICE_ID_BYTES)?;
        let row = self
            .connection()
            .query_row(
                "SELECT device_id, account_id, device_public_key, label, added_at_ms,
                        revoked_at_ms, revoked_reason
                   FROM identity_devices WHERE device_id = ?1",
                [device_id],
                device_from_row,
            )
            .optional()
            .map_err(StorageError::from)?;
        row.transpose()
    }
}

fn load_device(
    transaction: &rusqlite::Transaction<'_>,
    device_id: &str,
) -> Result<DeviceRegistryRecord> {
    let row = transaction
        .query_row(
            "SELECT device_id, account_id, device_public_key, label, added_at_ms,
                    revoked_at_ms, revoked_reason
               FROM identity_devices WHERE device_id = ?1",
            [device_id],
            device_from_row,
        )
        .map_err(StorageError::from)?;
    row
}

fn device_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<DeviceRegistryRecord>> {
    let device_public_key: Vec<u8> = row.get(2)?;
    let key: [u8; DEVICE_PUBLIC_KEY_BYTES] = match device_public_key.as_slice().try_into() {
        Ok(key) => key,
        Err(_) => {
            return Ok(Err(StorageError::StateCorrupted(
                "stored device public key has an invalid length".to_owned(),
            )));
        }
    };
    Ok(Ok(DeviceRegistryRecord {
        device_id: row.get(0)?,
        account_id: row.get(1)?,
        device_public_key: key,
        label: row.get(3)?,
        added_at_ms: row.get(4)?,
        revoked_at_ms: row.get(5)?,
        revoked_reason: row.get(6)?,
    }))
}
