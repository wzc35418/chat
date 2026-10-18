//! MLS 群状态与 provider 记录的正式加密持久化（P1 增量）。
//!
//! 目标（CRYPTO-004/010 的持久化切片）：让已提交的 MLS 群状态与 provider 秘密在
//! **正式加密库**里耐久保存，重启后仍能继续加解密。表结构见迁移
//! [0004_mls_state.sql]（只新增表，0001/0002/0003 未被改动）：
//!
//! - `mls_group_state`：每个群一行。`protected_state` 是群级状态的密封记录
//!   （用途 [`SealPurpose::MlsGroupState`]），`epoch` 与 `state_version` 明文；
//! - `mls_record`：provider 记录（群状态、epoch 秘密、签名密钥等），主键
//!   `(group_id, record_key)`，值用 [`SealPurpose::MlsRecord`] 密封。
//!
//! 硬性语义：
//!
//! - **AAD 绑定行身份**：群状态绑定 `(group_id, epoch, 格式版本)`；记录绑定
//!   `(group_id, epoch, record_key, 格式版本)`。把密文挪到另一个群/键/epoch 或换
//!   主密钥都会认证失败并返回 `STATE_CORRUPTED`。
//! - **epoch 单调**：保存旧 epoch（`offered < stored`）返回 `STALE_EPOCH`，绝不覆盖；
//!   epoch 前进时整组替换 provider 记录，旧 epoch 的秘密不会留下可读副本；
//!   读取时若记录的 epoch 与群当前 epoch 不一致，同样返回 `STALE_EPOCH`（显式拒绝）。
//! - **原子**：一次 `save_mls_group_state` 在单笔 `WriteScope::mls_state` 事务里
//!   写群状态并替换全部记录；失败（含进程 abort）整体回滚，不存在半状态。
//! - **有界**：群 ID/记录键/单条记录/单群总字节/记录条数/状态字节都有上限，越界
//!   显式 `INVALID_INPUT`，不截断。

use rusqlite::{OptionalExtension, params};

use super::error::StorageError;
use super::seal::{RecordSealer, SealPurpose};
use super::{
    CoreTransaction, Result, Store, WriteScope, commit_transaction, ensure_id, now_ms,
    rollback_transaction,
};

/// MLS 群 ID 文本上限（字节）。
pub const MAX_MLS_GROUP_ID_BYTES: usize = 128;
/// 单条 provider 记录键上限（字节）。
pub const MAX_MLS_RECORD_KEY_BYTES: usize = 512;
/// 单条 provider 记录值上限（字节）。
pub const MAX_MLS_RECORD_BYTES: usize = 256 * 1024;
/// 单个群的 provider 记录条数上限。
pub const MAX_MLS_RECORDS_PER_GROUP: usize = 4096;
/// 单个群的 provider 记录总字节上限。
pub const MAX_MLS_GROUP_RECORD_BYTES: usize = 4 * 1024 * 1024;
/// 群级状态明文上限（字节）。
pub const MAX_MLS_GROUP_STATE_BYTES: usize = 512 * 1024;

/// 群状态密封记录的格式版本（进入 AAD）。
const MLS_STATE_AAD_VERSION: &[u8] = b"mls-group-state-v1";
/// provider 记录密封记录的格式版本（进入 AAD）。
const MLS_RECORD_AAD_VERSION: &[u8] = b"mls-record-v1";

/// 一条 provider 记录的写入输入。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MlsRecordInput {
    /// provider 存储键（不透明字节）。
    pub record_key: Vec<u8>,
    /// provider 存储值（机密材料，只在内存与密封缓冲中出现）。
    pub value: Vec<u8>,
}

/// 一次「群状态 + provider 记录快照」的写入输入。
///
/// 语义是**快照替换**：同一 `(group_id, epoch)` 重复写入会用新快照替换旧记录；
/// epoch 前进时整组替换，旧 epoch 的记录被删除。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MlsGroupStateInput {
    /// 群 ID（不透明文本，存储层只做有界校验）。
    pub group_id: String,
    /// 已提交的 MLS epoch。
    pub epoch: u64,
    /// 群级状态明文（crypto 层序列化；落盘前密封）。
    pub state: Vec<u8>,
    /// 该 epoch 的 provider 记录快照。
    pub records: Vec<MlsRecordInput>,
}

impl MlsGroupStateInput {
    /// 有界结构校验。
    pub fn validate(&self) -> Result<()> {
        validate_group_id(&self.group_id)?;
        validate_epoch(self.epoch)?;
        if self.state.is_empty() || self.state.len() > MAX_MLS_GROUP_STATE_BYTES {
            return Err(StorageError::InvalidInput(format!(
                "MLS group state must be 1..={MAX_MLS_GROUP_STATE_BYTES} bytes"
            )));
        }
        if self.records.len() > MAX_MLS_RECORDS_PER_GROUP {
            return Err(StorageError::InvalidInput(format!(
                "MLS group carries more than {MAX_MLS_RECORDS_PER_GROUP} provider records"
            )));
        }
        let mut total = 0usize;
        let mut seen = std::collections::BTreeSet::new();
        for record in &self.records {
            validate_record(&record.record_key, &record.value)?;
            if !seen.insert(record.record_key.as_slice()) {
                return Err(StorageError::InvalidInput(
                    "MLS group snapshot repeats a provider record key".to_owned(),
                ));
            }
            total = total.checked_add(record.value.len()).ok_or_else(|| {
                StorageError::InvalidInput("MLS provider record byte total overflows".to_owned())
            })?;
        }
        if total > MAX_MLS_GROUP_RECORD_BYTES {
            return Err(StorageError::InvalidInput(format!(
                "MLS provider records exceed {MAX_MLS_GROUP_RECORD_BYTES} bytes"
            )));
        }
        Ok(())
    }
}

/// 群状态键列表的单次上限（有界查询，防止一次拉出整库）。
pub const MAX_GROUP_STATE_LIST: usize = 256;

/// 群状态键的读回投影（**只含键与时间**，不含密封正文）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MlsGroupStateKey {
    /// 群 ID（本模块里是命名空间 + 摘要，例如 `grpmeta/<hex>`）。
    pub group_id: String,
    /// 状态版本（每次成功写入 +1）。
    pub state_version: u64,
    /// 最近写入时间（Unix 毫秒）。
    pub updated_at_ms: i64,
}

/// 已提交群状态的读回投影（`state` 是**已解密**的明文，只在可信核心路径内使用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MlsGroupStateRecord {
    /// 群 ID。
    pub group_id: String,
    /// 当前已提交 epoch。
    pub epoch: u64,
    /// 状态版本（每次成功写入 +1）。
    pub state_version: u64,
    /// 群级状态明文。
    pub state: Vec<u8>,
    /// 最近写入时间（Unix 毫秒）。
    pub updated_at_ms: i64,
}

/// provider 记录的读回投影（`value` 是已解密明文）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MlsRecordRecord {
    /// 群 ID。
    pub group_id: String,
    /// provider 存储键。
    pub record_key: Vec<u8>,
    /// 该记录所属 epoch。
    pub epoch: u64,
    /// provider 存储值明文。
    pub value: Vec<u8>,
}

/// 写入期间的故障注入点（只用于崩溃/回滚测试；生产路径用空回调）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MlsWriteCheckpoint {
    /// 群状态行已写，provider 记录未替换。
    AfterGroupState,
    /// provider 记录已替换，提交未发生。
    AfterRecords,
    /// 提交前的最后断点。
    BeforeCommit,
}

fn validate_group_id(group_id: &str) -> Result<()> {
    ensure_id("mls group", group_id)?;
    if group_id.len() > MAX_MLS_GROUP_ID_BYTES {
        return Err(StorageError::InvalidInput(format!(
            "MLS group ID exceeds {MAX_MLS_GROUP_ID_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_epoch(epoch: u64) -> Result<()> {
    if i64::try_from(epoch).is_err() {
        return Err(StorageError::InvalidInput(
            "MLS epoch exceeds the supported integer range".to_owned(),
        ));
    }
    Ok(())
}

fn validate_record(record_key: &[u8], value: &[u8]) -> Result<()> {
    if record_key.is_empty() || record_key.len() > MAX_MLS_RECORD_KEY_BYTES {
        return Err(StorageError::InvalidInput(format!(
            "MLS record key must be 1..={MAX_MLS_RECORD_KEY_BYTES} bytes"
        )));
    }
    if value.len() > MAX_MLS_RECORD_BYTES {
        return Err(StorageError::InvalidInput(format!(
            "MLS record value exceeds {MAX_MLS_RECORD_BYTES} bytes"
        )));
    }
    Ok(())
}

fn group_state_aad<'a>(group_id: &'a str, epoch: &'a [u8; 8]) -> [&'a [u8]; 3] {
    [group_id.as_bytes(), epoch, MLS_STATE_AAD_VERSION]
}

fn record_aad<'a>(group_id: &'a str, epoch: &'a [u8; 8], record_key: &'a [u8]) -> [&'a [u8]; 4] {
    [group_id.as_bytes(), epoch, record_key, MLS_RECORD_AAD_VERSION]
}

fn seal_group_state(
    transaction: &CoreTransaction<'_>,
    group_id: &str,
    epoch: u64,
    state: &[u8],
) -> Result<Vec<u8>> {
    let epoch_bytes = epoch.to_be_bytes();
    let sealer = RecordSealer::new(transaction.key, SealPurpose::MlsGroupState);
    sealer.seal(&group_state_aad(group_id, &epoch_bytes), state)
}

fn seal_record(
    transaction: &CoreTransaction<'_>,
    group_id: &str,
    epoch: u64,
    record_key: &[u8],
    value: &[u8],
) -> Result<Vec<u8>> {
    let epoch_bytes = epoch.to_be_bytes();
    let sealer = RecordSealer::new(transaction.key, SealPurpose::MlsRecord);
    sealer.seal(&record_aad(group_id, &epoch_bytes, record_key), value)
}

fn stored_epoch(transaction: &CoreTransaction<'_>, group_id: &str) -> Result<Option<u64>> {
    transaction
        .connection()
        .query_row(
            "SELECT epoch FROM mls_group_state WHERE group_id = ?1",
            [group_id],
            |row| row.get::<_, u64>(0),
        )
        .optional()
        .map_err(StorageError::from)
}

fn save_group_state_inner(
    transaction: &mut CoreTransaction<'_>,
    input: &MlsGroupStateInput,
    checkpoint: &mut dyn FnMut(MlsWriteCheckpoint) -> Result<()>,
) -> Result<MlsGroupStateRecord> {
    let stored = transaction
        .connection()
        .query_row(
            "SELECT epoch, state_version FROM mls_group_state WHERE group_id = ?1",
            [&input.group_id],
            |row| Ok((row.get::<_, u64>(0)?, row.get::<_, u64>(1)?)),
        )
        .optional()?;
    let state_version = match stored {
        Some((stored_epoch, stored_version)) => {
            if input.epoch < stored_epoch {
                return Err(StorageError::StaleEpoch {
                    stored: stored_epoch,
                    offered: input.epoch,
                });
            }
            stored_version.saturating_add(1)
        }
        None => 1,
    };
    let epoch_sql = i64::try_from(input.epoch)
        .map_err(|_| StorageError::InvalidInput("MLS epoch is out of range".to_owned()))?;
    let version_sql = i64::try_from(state_version)
        .map_err(|_| StorageError::InvalidInput("MLS state version is out of range".to_owned()))?;
    let updated_at = now_ms();
    let sealed_state = seal_group_state(transaction, &input.group_id, input.epoch, &input.state)?;
    let rows = transaction.transaction.execute(
        "INSERT INTO mls_group_state (group_id, epoch, state_version, protected_state, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(group_id) DO UPDATE SET
             epoch = excluded.epoch,
             state_version = excluded.state_version,
             protected_state = excluded.protected_state,
             updated_at = excluded.updated_at",
        params![input.group_id, epoch_sql, version_sql, sealed_state, updated_at],
    )?;
    if rows != 1 {
        return Err(StorageError::StateCorrupted(
            "MLS group state upsert affected an unexpected row count".to_owned(),
        ));
    }
    transaction.note(rows);
    checkpoint(MlsWriteCheckpoint::AfterGroupState)?;

    // epoch 前进/同 epoch 快照替换：整组替换记录，旧 epoch 秘密不残留。
    transaction.transaction.execute(
        "DELETE FROM mls_record WHERE group_id = ?1",
        [&input.group_id],
    )?;
    for record in &input.records {
        let sealed = seal_record(
            transaction,
            &input.group_id,
            input.epoch,
            &record.record_key,
            &record.value,
        )?;
        transaction.transaction.execute(
            "INSERT INTO mls_record (group_id, record_key, record_epoch, protected_value, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                input.group_id,
                record.record_key,
                epoch_sql,
                sealed,
                updated_at
            ],
        )?;
    }
    transaction.note(input.records.len());
    checkpoint(MlsWriteCheckpoint::AfterRecords)?;
    checkpoint(MlsWriteCheckpoint::BeforeCommit)?;

    Ok(MlsGroupStateRecord {
        group_id: input.group_id.clone(),
        epoch: input.epoch,
        state_version,
        state: input.state.clone(),
        updated_at_ms: updated_at,
    })
}

impl Store {
    /// 保存（并替换）一个群的已提交状态与 provider 记录快照（单笔事务）。
    pub fn save_mls_group_state(
        &mut self,
        input: MlsGroupStateInput,
    ) -> Result<MlsGroupStateRecord> {
        self.save_mls_group_state_with_checkpoint(input, &mut |_| Ok(()))
    }

    /// 带故障注入点的保存：checkpoint 返回错误即整体回滚（供崩溃/回滚测试使用）。
    pub fn save_mls_group_state_with_checkpoint(
        &mut self,
        input: MlsGroupStateInput,
        checkpoint: &mut dyn FnMut(MlsWriteCheckpoint) -> Result<()>,
    ) -> Result<MlsGroupStateRecord> {
        self.ensure_ready()?;
        input.validate()?;
        let mut transaction = self.begin_transaction(WriteScope::mls_state())?;
        match save_group_state_inner(&mut transaction, &input, checkpoint) {
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


    /// 列出前缀匹配的群状态键（按更新时间倒序、同时间按 key 升序），最多 `limit` 条。
    ///
    /// - `limit` 必须 `1..=MAX_GROUP_STATE_LIST`（越界显式拒绝，不夹取）；
    /// - 只返回 `group_id/state_version/updated_at_ms`，**不解密**任何正文（列表页零成本）；
    /// - `prefix` 用 `substr` 比较（不走 LIKE，避免通配符注入）。
    pub fn list_mls_group_state_keys(
        &self,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<MlsGroupStateKey>> {
        self.ensure_ready()?;
        if !(1..=MAX_GROUP_STATE_LIST).contains(&limit) {
            return Err(StorageError::InvalidInput(format!(
                "group state list limit must be 1..={MAX_GROUP_STATE_LIST}"
            )));
        }
        if prefix.len() > 256 {
            return Err(StorageError::InvalidInput(
                "group state list prefix is too long".to_owned(),
            ));
        }
        let limit_sql = i64::try_from(limit)
            .map_err(|_| StorageError::InvalidInput("limit is out of range".to_owned()))?;
        let prefix_len = i64::try_from(prefix.len())
            .map_err(|_| StorageError::InvalidInput("prefix is out of range".to_owned()))?;
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT group_id, state_version, updated_at
                   FROM mls_group_state
                  WHERE substr(group_id, 1, ?2) = ?1
                  ORDER BY updated_at DESC, group_id ASC
                  LIMIT ?3",
            )
            .map_err(StorageError::from)?;
        let rows = statement
            .query_map(params![prefix, prefix_len, limit_sql], |row| {
                Ok((
                    row.get::<usize, String>(0)?,
                    row.get::<usize, i64>(1)?,
                    row.get::<usize, i64>(2)?,
                ))
            })
            .map_err(StorageError::from)?;
        let mut keys = Vec::new();
        for row in rows {
            let (group_id, state_version, updated_at) = row.map_err(StorageError::from)?;
            if state_version < 0 || updated_at < 0 {
                return Err(StorageError::StateCorrupted(
                    "stored group state key has a negative counter or timestamp".to_owned(),
                ));
            }
            keys.push(MlsGroupStateKey {
                group_id,
                state_version: u64::try_from(state_version)
                    .map_err(|_| StorageError::StateCorrupted("state version overflow".to_owned()))?,
                updated_at_ms: updated_at,
            });
        }
        Ok(keys)
    }

    /// 读取一个群的已提交状态；不存在返回 `None`。
    pub fn load_mls_group_state(&self, group_id: &str) -> Result<Option<MlsGroupStateRecord>> {
        self.ensure_ready()?;
        validate_group_id(group_id)?;
        let row: Option<(u64, u64, Vec<u8>, i64)> = self
            .connection()
            .query_row(
                "SELECT epoch, state_version, protected_state, updated_at
                   FROM mls_group_state WHERE group_id = ?1",
                [group_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((epoch, state_version, sealed, updated_at)) = row else {
            return Ok(None);
        };
        if updated_at < 0 {
            return Err(StorageError::StateCorrupted(
                "stored MLS group timestamp is negative".to_owned(),
            ));
        }
        let state = {
            // 只读路径复用同一个 sealer，不需要写事务句柄。
            let epoch_bytes = epoch.to_be_bytes();
            let sealer = RecordSealer::new(&self.key, SealPurpose::MlsGroupState);
            sealer.open(&group_state_aad(group_id, &epoch_bytes), &sealed)?
        };
        if state.is_empty() || state.len() > MAX_MLS_GROUP_STATE_BYTES {
            return Err(StorageError::StateCorrupted(
                "stored MLS group state has an invalid length".to_owned(),
            ));
        }
        Ok(Some(MlsGroupStateRecord {
            group_id: group_id.to_owned(),
            epoch,
            state_version,
            state: state.to_vec(),
            updated_at_ms: updated_at,
        }))
    }

    /// 在已保存的群上更新单条 provider 记录（必须与当前 epoch 一致）。
    pub fn save_mls_record(
        &mut self,
        group_id: &str,
        epoch: u64,
        record_key: &[u8],
        value: &[u8],
    ) -> Result<MlsRecordRecord> {
        self.ensure_ready()?;
        validate_group_id(group_id)?;
        validate_epoch(epoch)?;
        validate_record(record_key, value)?;
        let mut transaction = self.begin_transaction(WriteScope::mls_state())?;
        let outcome = (|| -> Result<MlsRecordRecord> {
            let Some(stored) = stored_epoch(&transaction, group_id)? else {
                return Err(StorageError::InvalidInput(
                    "MLS group state is not initialized".to_owned(),
                ));
            };
            if epoch < stored {
                return Err(StorageError::StaleEpoch {
                    stored,
                    offered: epoch,
                });
            }
            if epoch > stored {
                return Err(StorageError::InvalidInput(
                    "MLS record epoch is ahead of the stored group epoch".to_owned(),
                ));
            }
            let epoch_sql = i64::try_from(epoch)
                .map_err(|_| StorageError::InvalidInput("MLS epoch is out of range".to_owned()))?;
            let sealed = seal_record(&transaction, group_id, epoch, record_key, value)?;
            let updated_at = now_ms();
            let rows = transaction.transaction.execute(
                "INSERT INTO mls_record (group_id, record_key, record_epoch, protected_value, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(group_id, record_key) DO UPDATE SET
                     record_epoch = excluded.record_epoch,
                     protected_value = excluded.protected_value,
                     updated_at = excluded.updated_at",
                params![group_id, record_key, epoch_sql, sealed, updated_at],
            )?;
            transaction.note(rows);
            Ok(MlsRecordRecord {
                group_id: group_id.to_owned(),
                record_key: record_key.to_vec(),
                epoch,
                value: value.to_vec(),
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

    /// 读取单条 provider 记录；不存在返回 `None`。
    ///
    /// 记录 epoch 与群当前 epoch 不一致时返回 `STALE_EPOCH`（旧状态不可用于新 epoch）。
    pub fn load_mls_record(
        &self,
        group_id: &str,
        record_key: &[u8],
    ) -> Result<Option<MlsRecordRecord>> {
        self.ensure_ready()?;
        validate_group_id(group_id)?;
        validate_record(record_key, &[])?;
        let Some(group_epoch) = self.group_epoch(group_id)? else {
            return Ok(None);
        };
        let row: Option<(u64, Vec<u8>)> = self
            .connection()
            .query_row(
                "SELECT record_epoch, protected_value FROM mls_record
                   WHERE group_id = ?1 AND record_key = ?2",
                params![group_id, record_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((record_epoch, sealed)) = row else {
            return Ok(None);
        };
        if record_epoch != group_epoch {
            return Err(StorageError::StaleEpoch {
                stored: group_epoch,
                offered: record_epoch,
            });
        }
        let epoch_bytes = record_epoch.to_be_bytes();
        let sealer = RecordSealer::new(&self.key, SealPurpose::MlsRecord);
        let value = sealer.open(&record_aad(group_id, &epoch_bytes, record_key), &sealed)?;
        if value.len() > MAX_MLS_RECORD_BYTES {
            return Err(StorageError::StateCorrupted(
                "stored MLS record exceeds the supported size".to_owned(),
            ));
        }
        Ok(Some(MlsRecordRecord {
            group_id: group_id.to_owned(),
            record_key: record_key.to_vec(),
            epoch: record_epoch,
            value: value.to_vec(),
        }))
    }

    /// 读取一个群在当前 epoch 的全部 provider 记录（用于重开后的状态恢复）。
    pub fn load_mls_records(&self, group_id: &str) -> Result<Vec<MlsRecordRecord>> {
        self.ensure_ready()?;
        validate_group_id(group_id)?;
        let Some(group_epoch) = self.group_epoch(group_id)? else {
            return Ok(Vec::new());
        };
        let limit = i64::try_from(MAX_MLS_RECORDS_PER_GROUP + 1).unwrap_or(i64::MAX);
        let mut statement = self.connection().prepare(
            "SELECT record_key, record_epoch, protected_value FROM mls_record
               WHERE group_id = ?1 ORDER BY record_key ASC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![group_id, limit], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        let sealer = RecordSealer::new(&self.key, SealPurpose::MlsRecord);
        let mut records = Vec::new();
        let mut total = 0usize;
        for row in rows {
            let (record_key, record_epoch, sealed) = row?;
            if records.len() >= MAX_MLS_RECORDS_PER_GROUP {
                return Err(StorageError::StateCorrupted(format!(
                    "MLS group carries more than {MAX_MLS_RECORDS_PER_GROUP} provider records"
                )));
            }
            if record_epoch != group_epoch {
                return Err(StorageError::StaleEpoch {
                    stored: group_epoch,
                    offered: record_epoch,
                });
            }
            let epoch_bytes = record_epoch.to_be_bytes();
            let value = sealer.open(&record_aad(group_id, &epoch_bytes, &record_key), &sealed)?;
            total = total.saturating_add(value.len());
            if total > MAX_MLS_GROUP_RECORD_BYTES {
                return Err(StorageError::StateCorrupted(
                    "stored MLS provider records exceed the supported size".to_owned(),
                ));
            }
            records.push(MlsRecordRecord {
                group_id: group_id.to_owned(),
                record_key,
                epoch: record_epoch,
                value: value.to_vec(),
            });
        }
        Ok(records)
    }

    /// 群当前 epoch（用于记录 epoch 校验）。
    fn group_epoch(&self, group_id: &str) -> Result<Option<u64>> {
        self.connection()
            .query_row(
                "SELECT epoch FROM mls_group_state WHERE group_id = ?1",
                [group_id],
                |row| row.get::<_, u64>(0),
            )
            .optional()
            .map_err(StorageError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aad_binding_covers_group_epoch_and_record_key() {
        let epoch_two = 2u64.to_be_bytes();
        let first = group_state_aad("group-a", &epoch_two);
        let second = group_state_aad("group-b", &epoch_two);
        assert_ne!(first[0], second[0]);
        let record = record_aad("group-a", &epoch_two, b"record-1");
        assert_eq!(record[0], b"group-a");
        assert_eq!(record[3], MLS_RECORD_AAD_VERSION);
    }

    #[test]
    fn validation_rejects_unbounded_input() {
        let mut input = MlsGroupStateInput {
            group_id: "group-a".to_owned(),
            epoch: 1,
            state: b"state".to_vec(),
            records: vec![MlsRecordInput {
                record_key: b"key".to_vec(),
                value: b"value".to_vec(),
            }],
        };
        input.validate().expect("valid input");
        input.records.push(MlsRecordInput {
            record_key: b"key".to_vec(),
            value: b"duplicate".to_vec(),
        });
        assert_eq!(
            input.validate().unwrap_err().code(),
            "INVALID_INPUT"
        );
        input.records.clear();
        input.state = Vec::new();
        assert_eq!(input.validate().unwrap_err().code(), "INVALID_INPUT");
        input.state = vec![0u8; MAX_MLS_GROUP_STATE_BYTES + 1];
        assert_eq!(input.validate().unwrap_err().code(), "INVALID_INPUT");
        assert_eq!(
            validate_epoch(u64::MAX).unwrap_err().code(),
            "INVALID_INPUT"
        );
    }
}
