//! 插件宿主持久化（PLUGINS P4 增量的最小落库）：已安装清单 + 每插件私有键值。
//!
//! 语义：
//!
//! - plugins 表：清单原文（JSON，公开材料）与宿主状态（installed / stopped）明文存，时间戳由本层给出；
//!   upsert_plugin 幂等（同 id 覆盖清单与状态，保留首次安装时间）；
//! - plugin_kv 表：值**密封**存储（用途 SealPurpose::PluginKv，AAD 绑定 plugin_id + key），
//!   主键 (plugin_id, key) 保证同键幂等；plugin_id 外键 ON DELETE CASCADE，删插件行即清 KV；
//! - 有界配额：条目数 / 单值字节 / 单插件总字节（越界显式 INVALID_INPUT，绝不截断）；
//! - 键与值都不进日志；读回的值只在可信核心路径内使用。

use rusqlite::{OptionalExtension, params};

use super::error::StorageError;
use super::now_ms;
use super::seal::{RecordSealer, SealPurpose};
use super::{Result, Store, WriteScope, commit_transaction, rollback_transaction};

/// 单个插件的 KV 条目上限。
pub const MAX_PLUGIN_KV_ENTRIES: usize = 512;
/// 单个 KV 值的明文字节上限。
pub const MAX_PLUGIN_KV_VALUE_BYTES: usize = 64 * 1024;
/// 单个插件 KV 的总字节上限（按密封值计，是保守上界）。
pub const MAX_PLUGIN_KV_TOTAL_BYTES: usize = 1024 * 1024;
/// 插件 ID 字节上限。
pub const MAX_PLUGIN_ID_BYTES: usize = 64;
/// 单个 KV 键的字节上限。
pub const MAX_PLUGIN_KEY_BYTES: usize = 128;
/// 清单 JSON 的字节上限。
pub const MAX_PLUGIN_MANIFEST_BYTES: usize = 64 * 1024;
/// 插件状态文本的字节上限。
pub const MAX_PLUGIN_STATE_BYTES: usize = 32;
/// load_plugins 的单次上限。
pub const MAX_PLUGIN_ROWS: usize = 256;

/// 一条已安装插件的持久记录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginRecord {
    /// 插件 ID。
    pub id: String,
    /// 清单 JSON 原文（公开材料）。
    pub manifest_json: String,
    /// 宿主状态文本（installed / stopped / running）。
    pub state: String,
    /// 首次安装时间（Unix 毫秒）。
    pub installed_at_ms: i64,
    /// 最近更新时间（Unix 毫秒）。
    pub updated_at_ms: i64,
}

fn validate_plugin_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > MAX_PLUGIN_ID_BYTES || id.chars().any(char::is_control) {
        return Err(StorageError::InvalidInput(
            "plugin id must be 1..=64 printable bytes".to_owned(),
        ));
    }
    Ok(())
}

fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > MAX_PLUGIN_KEY_BYTES || key.chars().any(char::is_control) {
        return Err(StorageError::InvalidInput(
            "plugin key must be 1..=128 printable bytes".to_owned(),
        ));
    }
    Ok(())
}

fn validate_state(state: &str) -> Result<()> {
    if state.is_empty() || state.len() > MAX_PLUGIN_STATE_BYTES || state.chars().any(char::is_control)
    {
        return Err(StorageError::InvalidInput(
            "plugin state must be 1..=32 printable bytes".to_owned(),
        ));
    }
    Ok(())
}

impl Store {
    /// 写入（或更新）一条已安装插件；同 id 保留首次安装时间。
    pub fn upsert_plugin(
        &mut self,
        id: &str,
        manifest_json: &str,
        state: &str,
    ) -> Result<PluginRecord> {
        self.ensure_ready()?;
        validate_plugin_id(id)?;
        validate_state(state)?;
        if manifest_json.is_empty() || manifest_json.len() > MAX_PLUGIN_MANIFEST_BYTES {
            return Err(StorageError::InvalidInput(
                "plugin manifest is outside the supported bounds".to_owned(),
            ));
        }
        let now = now_ms();
        let mut transaction = self.begin_transaction(WriteScope::inbound("plugins"))?;
        let outcome = (|| -> Result<PluginRecord> {
            transaction
                .transaction
                .execute(
                    "INSERT INTO plugins (id, manifest_json, state, installed_at_ms, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?4)
                     ON CONFLICT(id) DO UPDATE SET
                         manifest_json = excluded.manifest_json,
                         state = excluded.state,
                         updated_at_ms = excluded.updated_at_ms",
                    params![id, manifest_json, state, now],
                )
                .map_err(StorageError::from)?;
            let record: (String, String, String, i64, i64) = transaction
                .transaction
                .query_row(
                    "SELECT id, manifest_json, state, installed_at_ms, updated_at_ms
                       FROM plugins WHERE id = ?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
                )
                .map_err(StorageError::from)?;
            Ok(PluginRecord {
                id: record.0,
                manifest_json: record.1,
                state: record.2,
                installed_at_ms: record.3,
                updated_at_ms: record.4,
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

    /// 列出全部已安装插件（按安装时间升序，最多 MAX_PLUGIN_ROWS 条）。
    pub fn load_plugins(&self) -> Result<Vec<PluginRecord>> {
        self.ensure_ready()?;
        let limit = i64::try_from(MAX_PLUGIN_ROWS)
            .map_err(|_| StorageError::InvalidInput("plugin row limit".to_owned()))?;
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT id, manifest_json, state, installed_at_ms, updated_at_ms
                   FROM plugins ORDER BY installed_at_ms ASC, id ASC LIMIT ?1",
            )
            .map_err(StorageError::from)?;
        let rows = statement
            .query_map([limit], |row| {
                Ok(PluginRecord {
                    id: row.get(0)?,
                    manifest_json: row.get(1)?,
                    state: row.get(2)?,
                    installed_at_ms: row.get(3)?,
                    updated_at_ms: row.get(4)?,
                })
            })
            .map_err(StorageError::from)?;
        let mut plugins = Vec::new();
        for row in rows {
            plugins.push(row.map_err(StorageError::from)?);
        }
        Ok(plugins)
    }

    /// 更新插件状态（不存在 → INVALID_INPUT）。
    pub fn set_plugin_state(&mut self, id: &str, state: &str) -> Result<()> {
        self.ensure_ready()?;
        validate_plugin_id(id)?;
        validate_state(state)?;
        let now = now_ms();
        let mut transaction = self.begin_transaction(WriteScope::inbound("plugins"))?;
        let outcome = (|| -> Result<()> {
            let rows = transaction
                .transaction
                .execute(
                    "UPDATE plugins SET state = ?2, updated_at_ms = ?3 WHERE id = ?1",
                    params![id, state, now],
                )
                .map_err(StorageError::from)?;
            if rows == 0 {
                return Err(StorageError::InvalidInput(
                    "plugin is not installed".to_owned(),
                ));
            }
            Ok(())
        })();
        match outcome {
            Ok(()) => {
                commit_transaction(transaction)?;
                Ok(())
            }
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }

    /// 删除插件记录（plugin_kv 随外键级联一起删除）。
    pub fn delete_plugin(&mut self, id: &str) -> Result<bool> {
        self.ensure_ready()?;
        validate_plugin_id(id)?;
        let mut transaction = self.begin_transaction(WriteScope::inbound("plugins"))?;
        let outcome = (|| -> Result<bool> {
            let rows = transaction
                .transaction
                .execute("DELETE FROM plugins WHERE id = ?1", [id])
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

    /// 读插件的某个键（不存在返回 None）。
    pub fn plugin_kv_get(&self, plugin_id: &str, key: &str) -> Result<Option<Vec<u8>>> {
        self.ensure_ready()?;
        validate_plugin_id(plugin_id)?;
        validate_key(key)?;
        let row: Option<Vec<u8>> = self
            .connection()
            .query_row(
                "SELECT value FROM plugin_kv WHERE plugin_id = ?1 AND key = ?2",
                params![plugin_id, key],
                |row| row.get(0),
            )
            .optional()
            .map_err(StorageError::from)?;
        let Some(sealed) = row else {
            return Ok(None);
        };
        let sealer = RecordSealer::new(&self.key, SealPurpose::PluginKv);
        let aad = plugin_kv_aad(plugin_id, key);
        let plaintext = sealer.open(&[aad.as_slice()], &sealed)?;
        Ok(Some(plaintext.to_vec()))
    }

    /// 写插件的某个键（有界配额；越界显式 INVALID_INPUT，绝不截断）。
    pub fn plugin_kv_set(&mut self, plugin_id: &str, key: &str, value: &[u8]) -> Result<()> {
        self.ensure_ready()?;
        validate_plugin_id(plugin_id)?;
        validate_key(key)?;
        if value.len() > MAX_PLUGIN_KV_VALUE_BYTES {
            return Err(StorageError::InvalidInput(
                "plugin value exceeds the per-value byte limit".to_owned(),
            ));
        }
        let sealer = RecordSealer::new(&self.key, SealPurpose::PluginKv);
        let sealed = sealer.seal(&[&plugin_kv_aad(plugin_id, key)], value)?;
        let now = now_ms();
        let mut transaction = self.begin_transaction(WriteScope::inbound("plugins"))?;
        let outcome = (|| -> Result<()> {
            let entries: i64 = transaction
                .transaction
                .query_row(
                    "SELECT COUNT(*) FROM plugin_kv WHERE plugin_id = ?1",
                    [plugin_id],
                    |row| row.get(0),
                )
                .map_err(StorageError::from)?;
            let existing: Option<Vec<u8>> = transaction
                .transaction
                .query_row(
                    "SELECT value FROM plugin_kv WHERE plugin_id = ?1 AND key = ?2",
                    params![plugin_id, key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(StorageError::from)?;
            if existing.is_none() && entries as usize >= MAX_PLUGIN_KV_ENTRIES {
                return Err(StorageError::InvalidInput(
                    "plugin storage entry limit reached".to_owned(),
                ));
            }
            let mut total: usize = sealed.len();
            {
                let mut statement = transaction
                    .transaction
                    .prepare("SELECT value FROM plugin_kv WHERE plugin_id = ?1")
                    .map_err(StorageError::from)?;
                let rows = statement
                    .query_map([plugin_id], |row| row.get::<usize, Vec<u8>>(0))
                    .map_err(StorageError::from)?;
                for row in rows {
                    let other = row.map_err(StorageError::from)?;
                    total = total.saturating_add(other.len());
                }
            }
            if total > MAX_PLUGIN_KV_TOTAL_BYTES {
                return Err(StorageError::InvalidInput(
                    "plugin storage total byte limit reached".to_owned(),
                ));
            }
            transaction
                .transaction
                .execute(
                    "INSERT INTO plugin_kv (plugin_id, key, value, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(plugin_id, key) DO UPDATE SET
                         value = excluded.value,
                         updated_at_ms = excluded.updated_at_ms",
                    params![plugin_id, key, sealed, now],
                )
                .map_err(StorageError::from)?;
            Ok(())
        })();
        match outcome {
            Ok(()) => {
                commit_transaction(transaction)?;
                Ok(())
            }
            Err(error) => {
                transaction.poison();
                let _ = rollback_transaction(transaction);
                Err(error)
            }
        }
    }

    /// 删除某插件的全部键值（delete_data=true 的卸载语义）。
    pub fn plugin_kv_delete_all(&mut self, plugin_id: &str) -> Result<u64> {
        self.ensure_ready()?;
        validate_plugin_id(plugin_id)?;
        let mut transaction = self.begin_transaction(WriteScope::inbound("plugins"))?;
        let outcome = (|| -> Result<u64> {
            let rows = transaction
                .transaction
                .execute("DELETE FROM plugin_kv WHERE plugin_id = ?1", [plugin_id])
                .map_err(StorageError::from)?;
            Ok(rows as u64)
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
}


/// KV 迁移策略（插件版本升级时如何对待旧命名空间的键值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KvMigrationPolicy {
    /// **默认**：旧键原样保留（升级后仍可直接读）。
    Keep,
    /// 把旧键**复制**到 `v<from>/` 前缀下（新命名空间），**原键保留**（绝不静默删除）。
    Namespace,
}

/// KV 迁移结果（纯数据）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KvMigrationReport {
    /// 复制到新前缀的键数（`Keep` 恒为 0）。
    pub copied: u64,
    /// 原样保留的键数。
    pub kept: u64,
    /// 新前缀（`Keep` 为 `None`）。
    pub prefix: Option<String>,
    /// 插件 ID（诊断用）。
    pub plugin_id: String,
    /// 旧版本文本（诊断用）。
    pub from: String,
    /// 新版本文本（诊断用）。
    pub to: String,
}

/// 单个插件最多一次清理多少行（与 `MAX_PLUGIN_ROWS` 一致）。
pub const MAX_PLUGIN_PURGE: usize = 256;
/// `uninstalled` 行状态文本（卸载时上层写入，清理只认这个值）。
pub const STATE_UNINSTALLED: &str = "uninstalled";

impl Store {
    /// 升级时的 KV 处理：`Keep`（默认，原样保留）或 `Namespace`（复制到 `v<from>/` 前缀）。
    ///
    /// - **绝不删除**旧键；`Namespace` 只增不删，且对"已经复制过"的键幂等跳过；
    /// - 同前缀下已存在且值不同 → 显式 `INVALID_INPUT`（不覆盖别人的数据）；
    /// - 复制会突破条目/字节配额 → 显式报错（不做半迁移）。
    pub fn plugin_kv_rename_or_keep(
        &mut self,
        plugin_id: &str,
        from: &str,
        to: &str,
        policy: KvMigrationPolicy,
    ) -> Result<KvMigrationReport> {
        self.ensure_ready()?;
        validate_plugin_id(plugin_id)?;
        validate_key(from)?;
        validate_key(to)?;
        let existing = self.plugin_kv_keys(plugin_id)?;
        let kept = existing.len() as u64;
        let Some(prefix) = namespace_prefix(policy, from)? else {
            return Ok(KvMigrationReport {
                copied: 0,
                kept,
                prefix: None,
                plugin_id: plugin_id.to_owned(),
                from: from.to_owned(),
                to: to.to_owned(),
            });
        };
        let mut plan: Vec<(String, Vec<u8>)> = Vec::new();
        let mut plain_bytes: u64 = 0;
        for key in &existing {
            // 已经在新命名空间里的键不再迁移（否则每次重跑都会叠加一层前缀）。
            if key.starts_with(&prefix) {
                continue;
            }
            let Some(value) = self.plugin_kv_get(plugin_id, key)? else {
                continue;
            };
            plain_bytes = plain_bytes.saturating_add(value.len() as u64);
            let target = format!("{prefix}{key}");
            if target.len() > MAX_PLUGIN_KEY_BYTES {
                return Err(StorageError::InvalidInput(
                    "migrated key exceeds the key length limit".to_owned(),
                ));
            }
            if let Some(current) = self.plugin_kv_get(plugin_id, &target)? {
                if current == value {
                    continue; // 已复制过（幂等重跑）
                }
                return Err(StorageError::InvalidInput(
                    "the migration prefix already holds a different value".to_owned(),
                ));
            }
            plan.push((target, value));
        }
        let copies = plan.len() as u64;
        if copies > 0 {
            if existing.len().saturating_add(plan.len()) > MAX_PLUGIN_KV_ENTRIES {
                return Err(StorageError::InvalidInput(
                    "migrating the plugin namespace would exceed the entry limit".to_owned(),
                ));
            }
            let copy_bytes: u64 = plan.iter().map(|(_, value)| value.len() as u64).sum();
            if plain_bytes.saturating_add(copy_bytes) > MAX_PLUGIN_KV_TOTAL_BYTES as u64 {
                return Err(StorageError::InvalidInput(
                    "migrating the plugin namespace would exceed the byte limit".to_owned(),
                ));
            }
            for (key, value) in &plan {
                self.plugin_kv_set(plugin_id, key, value)?;
            }
        }
        Ok(KvMigrationReport {
            copied: copies,
            kept,
            prefix: Some(prefix),
            plugin_id: plugin_id.to_owned(),
            from: from.to_owned(),
            to: to.to_owned(),
        })
    }

    /// 该插件命名空间里的全部键（升序）。
    pub fn plugin_kv_keys(&self, plugin_id: &str) -> Result<Vec<String>> {
        self.ensure_ready()?;
        validate_plugin_id(plugin_id)?;
        let connection = self.connection();
        let mut statement = connection
            .prepare("SELECT key FROM plugin_kv WHERE plugin_id = ?1 ORDER BY key ASC")
            .map_err(StorageError::from)?;
        let rows = statement
            .query_map([plugin_id], |row| row.get::<usize, String>(0))
            .map_err(StorageError::from)?;
        let mut keys = Vec::new();
        for row in rows {
            keys.push(row.map_err(StorageError::from)?);
        }
        Ok(keys)
    }

    /// 清理 `uninstalled` 且最后更新早于阈值（**严格小于**）的插件行（同事务级联清 KV）。
    ///
    /// 只删 `state = 'uninstalled'` 的行；单次最多 [`MAX_PLUGIN_PURGE`] 行（updated_at 升序）。
    pub fn purge_uninstalled_plugins(&mut self, older_than_ms: i64, limit: usize) -> Result<u64> {
        self.ensure_ready()?;
        if older_than_ms < 0 {
            return Err(StorageError::InvalidInput(
                "purge threshold must be a non-negative timestamp".to_owned(),
            ));
        }
        if limit == 0 || limit > MAX_PLUGIN_PURGE {
            return Err(StorageError::InvalidInput(
                "purge limit must be within 1..=256".to_owned(),
            ));
        }
        let mut transaction = self.begin_transaction(WriteScope::inbound("plugins"))?;
        let outcome = (|| -> Result<u64> {
            let mut ids = Vec::new();
            {
                let mut statement = transaction
                    .transaction
                    .prepare(
                        "SELECT id FROM plugins
                          WHERE state = ?1 AND updated_at_ms < ?2
                          ORDER BY updated_at_ms ASC, id ASC LIMIT ?3",
                    )
                    .map_err(StorageError::from)?;
                let rows = statement
                    .query_map(
                        params![STATE_UNINSTALLED, older_than_ms, limit as i64],
                        |row| row.get::<usize, String>(0),
                    )
                    .map_err(StorageError::from)?;
                for row in rows {
                    ids.push(row.map_err(StorageError::from)?);
                }
            }
            let mut removed = 0_u64;
            for id in ids {
                removed += transaction
                    .transaction
                    .execute("DELETE FROM plugins WHERE id = ?1", [id])
                    .map_err(StorageError::from)? as u64;
            }
            Ok(removed)
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
}

/// 迁移目标前缀（`Namespace` → `v<from>/`；`Keep` → `None`）。
fn namespace_prefix(policy: KvMigrationPolicy, from: &str) -> Result<Option<String>> {
    match policy {
        KvMigrationPolicy::Keep => Ok(None),
        KvMigrationPolicy::Namespace => {
            let prefix = format!("v{from}/");
            if prefix.len() > MAX_PLUGIN_KEY_BYTES {
                return Err(StorageError::InvalidInput(
                    "the migration prefix is too long".to_owned(),
                ));
            }
            Ok(Some(prefix))
        }
    }
}
/// 插件 KV 的 AAD（绑定 plugin_id 与 key，跨插件/跨键不可互换）。
fn plugin_kv_aad(plugin_id: &str, key: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(plugin_id.len() + key.len() + 32);
    aad.extend_from_slice(b"plugin-kv-v1");
    aad.extend_from_slice(plugin_id.as_bytes());
    aad.push(0);
    aad.extend_from_slice(key.as_bytes());
    aad
}
