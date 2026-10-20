//! MLSDEV（TASK-130）：**设备级** MLS provider 记录的加密持久化。
//!
//! 与 [`super::mls_state`] 的区别：那边存的是**已加入的群**（必须 epoch/group_id），
//! 这边存的是**设备本身**——包括只导出过 KeyPackage、还没入群的设备的 init 私钥，
//! 因此另一进程可以用它直接接受 Welcome。
//!
//! 语义：
//!
//! - 一台设备一行（`device_key` 由调用方给），头与每条记录都**密封**，
//!   AAD 绑定 `(device_key, state_version, record_key)`，跨设备/跨键/跨版本不可互换；
//! - 整组替换是**单笔事务**：头与记录一起提交或一起回滚；
//! - 有界：记录条数与单条字节都有上限（越界显式 `INVALID_INPUT`，绝不静默截断）；
//! - 记录值不进日志、不进错误文本（错误只给稳定码与计数）。

use rusqlite::{OptionalExtension, params};

use super::error::StorageError;
use super::mls_state::MlsRecordInput;
use super::now_ms;
use super::seal::{RecordSealer, SealPurpose};
use super::{Result, Store, WriteScope, commit_transaction, rollback_transaction};

/// 单台设备最多保存多少条 provider 记录。
pub const MAX_MLS_DEVICE_RECORDS: usize = 512;
/// 单条记录的字节上限。
pub const MAX_MLS_DEVICE_RECORD_BYTES: usize = 256 * 1024;
/// 设备标识字节上限。
pub const MAX_MLS_DEVICE_KEY_BYTES: usize = 128;
/// 设备头（state）字节上限。
pub const MAX_MLS_DEVICE_STATE_BYTES: usize = 8 * 1024;

/// 设备头 AAD 版本。
const MLS_DEVICE_STATE_AAD_VERSION: &[u8] = b"mls-device-state-v1";
/// 设备记录 AAD 版本。
const MLS_DEVICE_RECORD_AAD_VERSION: &[u8] = b"mls-device-record-v1";

/// 设备快照写入输入（头 + provider 记录）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MlsDeviceStateInput {
    /// 稳定的设备标识（调用方给；如 `mlsdev/<设备ID>`）。
    pub device_key: String,
    /// 设备头（调用方编码；本层只加密封与长度校验）。
    pub state: Vec<u8>,
    /// provider 记录（`record_key` 是 provider 的原始键字节）。
    pub records: Vec<MlsRecordInput>,
}

impl MlsDeviceStateInput {
    /// 有界结构校验。
    pub fn validate(&self) -> Result<()> {
        validate_device_key(&self.device_key)?;
        if self.state.is_empty() || self.state.len() > MAX_MLS_DEVICE_STATE_BYTES {
            return Err(StorageError::InvalidInput(
                "MLS device state must be 1..=8192 bytes".to_owned(),
            ));
        }
        if self.records.len() > MAX_MLS_DEVICE_RECORDS {
            return Err(StorageError::InvalidInput(
                "too many MLS device records".to_owned(),
            ));
        }
        let mut seen: Vec<&[u8]> = Vec::with_capacity(self.records.len());
        for record in &self.records {
            if record.record_key.is_empty() || record.record_key.len() > 512 {
                return Err(StorageError::InvalidInput(
                    "MLS device record key must be 1..=512 bytes".to_owned(),
                ));
            }
            if record.value.is_empty() || record.value.len() > MAX_MLS_DEVICE_RECORD_BYTES {
                return Err(StorageError::InvalidInput(
                    "MLS device record value must be 1..=256 KiB".to_owned(),
                ));
            }
            if seen.contains(&record.record_key.as_slice()) {
                return Err(StorageError::InvalidInput(
                    "MLS device records repeat a record key".to_owned(),
                ));
            }
            seen.push(record.record_key.as_slice());
        }
        Ok(())
    }
}

/// 设备快照里的一条记录（读回投影；`value` 是已解密明文）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MlsDeviceRecord {
    /// provider 存储键（原始字节）。
    pub record_key: Vec<u8>,
    /// provider 存储值明文。
    pub value: Vec<u8>,
}

/// 设备快照读取结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MlsDeviceStateRecord {
    /// 设备标识。
    pub device_key: String,
    /// 设备头原文。
    pub state: Vec<u8>,
    /// 记录（按 record_key 升序）。
    pub records: Vec<MlsDeviceRecord>,
    /// 更新次数（每次保存 +1）。
    pub state_version: u64,
    /// 最近更新时间（Unix 毫秒）。
    pub updated_at_ms: i64,
}

fn validate_device_key(device_key: &str) -> Result<()> {
    if device_key.is_empty()
        || device_key.len() > MAX_MLS_DEVICE_KEY_BYTES
        || device_key.chars().any(char::is_control)
    {
        return Err(StorageError::InvalidInput(
            "MLS device key must be 1..=128 printable bytes".to_owned(),
        ));
    }
    Ok(())
}

fn state_aad<'a>(device_key: &'a str, version: &'a [u8; 8]) -> [&'a [u8]; 3] {
    [device_key.as_bytes(), version, MLS_DEVICE_STATE_AAD_VERSION]
}

fn record_aad<'a>(
    device_key: &'a str,
    version: &'a [u8; 8],
    record_key: &'a [u8],
) -> [&'a [u8]; 4] {
    [
        device_key.as_bytes(),
        version,
        record_key,
        MLS_DEVICE_RECORD_AAD_VERSION,
    ]
}

impl Store {
    /// 保存（并整组替换）一台设备的快照（单笔事务）。
    pub fn save_mls_device_state(
        &mut self,
        input: MlsDeviceStateInput,
    ) -> Result<MlsDeviceStateRecord> {
        self.ensure_ready()?;
        input.validate()?;
        let mut transaction = self.begin_transaction(WriteScope::inbound("mls_device"))?;
        let outcome = (|| -> Result<MlsDeviceStateRecord> {
            let stored_version: Option<i64> = transaction
                .transaction
                .query_row(
                    "SELECT state_version FROM mls_device_state WHERE device_key = ?1",
                    [&input.device_key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(StorageError::from)?;
            let state_version = match stored_version {
                Some(version) => u64::try_from(version).unwrap_or(0).saturating_add(1),
                None => 1,
            };
            let version_sql = i64::try_from(state_version)
                .map_err(|_| StorageError::InvalidInput("device version is out of range".to_owned()))?;
            let updated_at = now_ms();
            let version_bytes = state_version.to_be_bytes();
            let sealed_state = {
                let sealer = RecordSealer::new(transaction.key, SealPurpose::MlsGroupState);
                sealer.seal(&state_aad(&input.device_key, &version_bytes), &input.state)?
            };
            transaction
                .transaction
                .execute(
                    "INSERT INTO mls_device_state (device_key, state_version, protected_state, updated_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(device_key) DO UPDATE SET
                         state_version = excluded.state_version,
                         protected_state = excluded.protected_state,
                         updated_at = excluded.updated_at",
                    params![input.device_key, version_sql, sealed_state, updated_at],
                )
                .map_err(StorageError::from)?;
            transaction
                .transaction
                .execute(
                    "DELETE FROM mls_device_record WHERE device_key = ?1",
                    [&input.device_key],
                )
                .map_err(StorageError::from)?;
            let mut records = Vec::with_capacity(input.records.len());
            for record in &input.records {
                let sealed = {
                    let sealer = RecordSealer::new(transaction.key, SealPurpose::MlsRecord);
                    sealer.seal(
                        &record_aad(&input.device_key, &version_bytes, &record.record_key),
                        &record.value,
                    )?
                };
                transaction
                    .transaction
                    .execute(
                        "INSERT INTO mls_device_record (device_key, record_key, protected_value, updated_at)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![input.device_key, record.record_key, sealed, updated_at],
                    )
                    .map_err(StorageError::from)?;
                records.push(MlsDeviceRecord {
                    record_key: record.record_key.clone(),
                    value: record.value.clone(),
                });
            }
            transaction.note(input.records.len());
            Ok(MlsDeviceStateRecord {
                device_key: input.device_key.clone(),
                state: input.state.clone(),
                records,
                state_version,
                updated_at_ms: updated_at,
            })
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

    /// 读取一台设备的快照；不存在返回 `None`（调用方据此报「没有 kp」，绝不伪造空设备）。
    pub fn load_mls_device_state(&self, device_key: &str) -> Result<Option<MlsDeviceStateRecord>> {
        self.ensure_ready()?;
        validate_device_key(device_key)?;
        let row: Option<(u64, Vec<u8>, i64)> = self
            .connection()
            .query_row(
                "SELECT state_version, protected_state, updated_at FROM mls_device_state WHERE device_key = ?1",
                [device_key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StorageError::from)?;
        let Some((state_version, sealed_state, updated_at)) = row else {
            return Ok(None);
        };
        if updated_at < 0 {
            return Err(StorageError::StateCorrupted(
                "stored MLS device timestamp is negative".to_owned(),
            ));
        }
        let version_bytes = state_version.to_be_bytes();
        let state = {
            let sealer = RecordSealer::new(&self.key, SealPurpose::MlsGroupState);
            sealer
                .open(&state_aad(device_key, &version_bytes), &sealed_state)?
                .to_vec()
        };
        if state.is_empty() || state.len() > MAX_MLS_DEVICE_STATE_BYTES {
            return Err(StorageError::StateCorrupted(
                "stored MLS device state has an invalid length".to_owned(),
            ));
        }
        let mut records = Vec::new();
        {
            let connection = self.connection();
            let mut statement = connection
                .prepare(
                    "SELECT record_key, protected_value FROM mls_device_record
                      WHERE device_key = ?1 ORDER BY record_key ASC",
                )
                .map_err(StorageError::from)?;
            let rows = statement
                .query_map([device_key], |row| {
                    Ok((row.get::<usize, Vec<u8>>(0)?, row.get::<usize, Vec<u8>>(1)?))
                })
                .map_err(StorageError::from)?;
            for row in rows {
                let (record_key, sealed) = row.map_err(StorageError::from)?;
                let sealer = RecordSealer::new(&self.key, SealPurpose::MlsRecord);
                let value = sealer
                    .open(
                        &record_aad(device_key, &version_bytes, &record_key),
                        &sealed,
                    )?
                    .to_vec();
                if value.is_empty() || value.len() > MAX_MLS_DEVICE_RECORD_BYTES {
                    return Err(StorageError::StateCorrupted(
                        "stored MLS device record has an invalid length".to_owned(),
                    ));
                }
                records.push(MlsDeviceRecord { record_key, value });
            }
        }
        if records.len() > MAX_MLS_DEVICE_RECORDS {
            return Err(StorageError::StateCorrupted(
                "stored MLS device has too many records".to_owned(),
            ));
        }
        Ok(Some(MlsDeviceStateRecord {
            device_key: device_key.to_owned(),
            state,
            records,
            state_version,
            updated_at_ms: updated_at,
        }))
    }

    /// 删除一台设备的快照（记录随外键级联删除）。返回是否真的删了行。
    pub fn clear_mls_device_state(&mut self, device_key: &str) -> Result<bool> {
        self.ensure_ready()?;
        validate_device_key(device_key)?;
        let mut transaction = self.begin_transaction(WriteScope::inbound("mls_device"))?;
        let outcome = transaction
            .transaction
            .execute("DELETE FROM mls_device_state WHERE device_key = ?1", [device_key])
            .map_err(StorageError::from);
        match outcome {
            Ok(rows) => {
                transaction.note(rows);
                commit_transaction(transaction)?;
                Ok(rows > 0)
            }
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }
}
