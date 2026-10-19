//! 凭据目录：把群内**凭据文本**映射成真实账户/设备 ID（STORAGE-016）。
//!
//! 语义：
//!
//! - `credential` 是群/协议层的公开凭据文本（1..=128 字节、无控制字符），主键保证同凭据幂等；
//! - `account_id` / `device_id` 是 32 字节协议 ID（BLOB，非秘密），长度由表 CHECK 与写入校验双重约束；
//! - `updated_at_ms` 由本层给出：重复 upsert 只刷新时间戳，不改已有映射（映射不一致 → 覆盖并刷新时间，
//!   由调用方决定是否先核对）；
//! - **有界**：条目总数上限 `MAX_CREDENTIAL_DIRECTORY_ROWS`（1024），越界显式 `INVALID_INPUT`，
//!   绝不静默丢弃旧条目；`load_all` 同样被该上限挡住；
//! - 凭据与 ID 都进不了普通日志（本层不打印任何值）。

use rusqlite::{OptionalExtension, params};

use super::error::StorageError;
use super::now_ms;
use super::{Result, Store, WriteScope, commit_transaction, rollback_transaction};

/// 凭据目录的条目上限（写路径与读取上限共用）。
pub const MAX_CREDENTIAL_DIRECTORY_ROWS: usize = 1024;
/// 单条凭据的字节上限。
pub const MAX_CREDENTIAL_BYTES: usize = 128;
/// 协议 ID 长度（AccountId / DeviceId）。
pub const CREDENTIAL_DIRECTORY_ID_BYTES: usize = 32;

/// 一条凭据 → 账户/设备的映射记录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialDirectoryRecord {
    /// 群内凭据文本。
    pub credential: String,
    /// 真实账户 ID（32 字节）。
    pub account_id: [u8; CREDENTIAL_DIRECTORY_ID_BYTES],
    /// 该凭据对应的设备 ID（32 字节）。
    pub device_id: [u8; CREDENTIAL_DIRECTORY_ID_BYTES],
    /// 最近写入时间（Unix 毫秒）。
    pub updated_at_ms: i64,
}

fn validate_credential(credential: &str) -> Result<()> {
    if credential.is_empty()
        || credential.len() > MAX_CREDENTIAL_BYTES
        || credential.chars().any(char::is_control)
    {
        return Err(StorageError::InvalidInput(
            "credential must be 1..=128 printable bytes".to_owned(),
        ));
    }
    Ok(())
}

impl Store {
    /// 批量 upsert 凭据映射（幂等；条目总数不得超过上限）。
    ///
    /// 返回真正写入/更新的行数；任一条不合法或会超出上限 → 整笔回滚，一条都不写。
    pub fn upsert_credential_directory(
        &mut self,
        entries: &[CredentialDirectoryRecord],
    ) -> Result<usize> {
        self.ensure_ready()?;
        if entries.is_empty() {
            return Ok(0);
        }
        for entry in entries {
            validate_credential(&entry.credential)?;
        }
        let now = now_ms();
        let mut transaction = self.begin_transaction(WriteScope::inbound("credential_directory"))?;
        let outcome = (|| -> Result<usize> {
            let existing: i64 = transaction
                .transaction
                .query_row("SELECT COUNT(*) FROM credential_directory", [], |row| row.get(0))
                .map_err(StorageError::from)?;
            let mut new_rows = 0i64;
            for entry in entries {
                let known: Option<i64> = transaction
                    .transaction
                    .query_row(
                        "SELECT 1 FROM credential_directory WHERE credential = ?1",
                        [entry.credential.as_str()],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(StorageError::from)?;
                if known.is_none() {
                    new_rows = new_rows.saturating_add(1);
                }
            }
            let limit = i64::try_from(MAX_CREDENTIAL_DIRECTORY_ROWS)
                .map_err(|_| StorageError::InvalidInput("credential directory limit".to_owned()))?;
            if existing.saturating_add(new_rows) > limit {
                return Err(StorageError::InvalidInput(format!(
                    "credential directory is limited to {MAX_CREDENTIAL_DIRECTORY_ROWS} entries"
                )));
            }
            let mut written = 0usize;
            for entry in entries {
                transaction
                    .transaction
                    .execute(
                        "INSERT INTO credential_directory (credential, account_id, device_id, updated_at_ms)
                         VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT(credential) DO UPDATE SET
                             account_id = excluded.account_id,
                             device_id = excluded.device_id,
                             updated_at_ms = excluded.updated_at_ms",
                        params![entry.credential, entry.account_id.as_slice(), entry.device_id.as_slice(), now],
                    )
                    .map_err(StorageError::from)?;
                written = written.saturating_add(1);
            }
            Ok(written)
        })();
        match outcome {
            Ok(written) => {
                commit_transaction(transaction)?;
                Ok(written)
            }
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }

    /// 按凭据读取单条映射（未知凭据返回 None，不猜）。
    pub fn load_credential_directory(
        &self,
        credential: &str,
    ) -> Result<Option<CredentialDirectoryRecord>> {
        self.ensure_ready()?;
        validate_credential(credential)?;
        let connection = self.connection();
        connection
            .query_row(
                "SELECT credential, account_id, device_id, updated_at_ms
                   FROM credential_directory WHERE credential = ?1",
                [credential],
                |row| {
                    Ok(CredentialDirectoryRecord {
                        credential: row.get(0)?,
                        account_id: id_column(row.get::<_, Vec<u8>>(1)?)?,
                        device_id: id_column(row.get::<_, Vec<u8>>(2)?)?,
                        updated_at_ms: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// 删除一条映射（未知凭据返回 false；用于身份撤销/测试场景）。
    pub fn delete_credential_directory(&mut self, credential: &str) -> Result<bool> {
        self.ensure_ready()?;
        validate_credential(credential)?;
        let mut transaction = self.begin_transaction(WriteScope::inbound("credential_directory"))?;
        let outcome = (|| -> Result<bool> {
            let rows = transaction
                .transaction
                .execute(
                    "DELETE FROM credential_directory WHERE credential = ?1",
                    [credential],
                )
                .map_err(StorageError::from)?;
            Ok(rows > 0)
        })();
        match outcome {
            Ok(removed) => {
                commit_transaction(transaction)?;
                Ok(removed)
            }
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }
    /// 列出全部映射（按凭据升序，最多 `MAX_CREDENTIAL_DIRECTORY_ROWS` 条）。
    pub fn load_all_credential_directory(&self) -> Result<Vec<CredentialDirectoryRecord>> {
        self.ensure_ready()?;
        let limit = i64::try_from(MAX_CREDENTIAL_DIRECTORY_ROWS)
            .map_err(|_| StorageError::InvalidInput("credential directory limit".to_owned()))?;
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT credential, account_id, device_id, updated_at_ms
                   FROM credential_directory ORDER BY credential ASC LIMIT ?1",
            )
            .map_err(StorageError::from)?;
        let rows = statement
            .query_map([limit], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(StorageError::from)?;
        let mut out = Vec::new();
        for row in rows {
            let (credential, account_id, device_id, updated_at_ms) = row.map_err(StorageError::from)?;
            out.push(CredentialDirectoryRecord {
                credential,
                account_id: id_column(account_id)?,
                device_id: id_column(device_id)?,
                updated_at_ms,
            });
        }
        Ok(out)
    }
}

fn id_column(bytes: Vec<u8>) -> rusqlite::Result<[u8; CREDENTIAL_DIRECTORY_ID_BYTES]> {
    let array: [u8; CREDENTIAL_DIRECTORY_ID_BYTES] = bytes.as_slice().try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Blob,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "credential directory id must be 32 bytes",
            )),
        )
    })?;
    Ok(array)
}
