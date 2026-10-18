//! 正式存储的版本化迁移（STORAGE-002 的存储侧实现）。
//!
//! 迁移 SQL 以 `include_str!` 编译进二进制，运行时逐版本在单笔 `BEGIN IMMEDIATE`
//! 事务内执行，并在 `schema_migrations` 记录“规范化 SQL 的 SHA-256 校验和”。
//! 已记录的校验和与当前脚本不一致、或库内版本高于本程序支持的版本时一律显式失败，
//! 不做自动降级，也不跳过迁移继续写。
//!
//! 规范化只做一件确定性的事：去掉脚本外层的 `BEGIN IMMEDIATE`/`COMMIT` 包装与
//! `PRAGMA` 行（事务由迁移器自己控制，PRAGMA 由 `open_store` 统一设置），
//! 其余字节（含注释）原样进入校验和。

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use super::error::StorageError;

/// 单个迁移定义。
pub(crate) struct MigrationDef {
    pub(crate) version: u32,
    pub(crate) name: &'static str,
    pub(crate) sql: &'static str,
}

/// 全部迁移按版本升序排列。
pub(crate) const MIGRATIONS: &[MigrationDef] = &[
    MigrationDef {
        version: 1,
        name: "0001_initial",
        sql: include_str!("../../migrations/0001_initial.sql"),
    },
    MigrationDef {
        version: 2,
        name: "0002_formal_store",
        sql: include_str!("../../migrations/0002_formal_store.sql"),
    },
    MigrationDef {
        version: 3,
        name: "0003_identity",
        sql: include_str!("../../migrations/0003_identity.sql"),
    },
    MigrationDef {
        version: 4,
        name: "0004_mls_state",
        sql: include_str!("../../migrations/0004_mls_state.sql"),
    },
    MigrationDef {
        version: 5,
        name: "0005_attachments",
        sql: include_str!("../../migrations/0005_attachments.sql"),
    },
    MigrationDef {
        version: 6,
        name: "0006_device_credentials",
        sql: include_str!("../../migrations/0006_device_credentials.sql"),
    },
    MigrationDef {
        version: 7,
        name: "0007_attachment_secrets",
        sql: include_str!("../../migrations/0007_attachment_secrets.sql"),
    },
    MigrationDef {
        version: 8,
        name: "0008_attachment_transfer_progress",
        sql: include_str!("../../migrations/0008_attachment_transfer_progress.sql"),
    },
    MigrationDef {
        version: 9,
        name: "0009_message_attachments",
        sql: include_str!("../../migrations/0009_message_attachments.sql"),
    },
    MigrationDef {
        version: 10,
        name: "0010_plugins",
        sql: include_str!("../../migrations/0010_plugins.sql"),
    },
];

/// 迁移结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationReport {
    /// 迁移前库内版本（0 表示新库）。
    pub from_version: u32,
    /// 迁移后库内版本。
    pub to_version: u32,
    /// 本次实际执行的版本列表。
    pub applied: Vec<u32>,
}

/// 本程序支持的最高 schema 版本。
pub(crate) fn target_version() -> u32 {
    MIGRATIONS.last().map(|entry| entry.version).unwrap_or(0)
}

/// 计算某个迁移脚本的规范化校验和（十六进制小写）。
pub(crate) fn migration_checksum(sql: &str) -> String {
    let normalized = normalized_body(sql);
    let digest = Sha256::digest(normalized.as_bytes());
    super::hex(&digest)
}

/// 去掉外层事务包装与 PRAGMA 行后的可执行 SQL。
pub(crate) fn normalized_body(sql: &str) -> String {
    let mut kept = Vec::new();
    for line in sql.lines() {
        let trimmed = line.trim();
        let upper = trimmed.to_ascii_uppercase();
        let wrapper = matches!(
            upper.as_str(),
            "BEGIN IMMEDIATE;" | "BEGIN;" | "COMMIT;" | "END;" | "COMMIT TRANSACTION;"
        );
        if wrapper || upper.starts_with("PRAGMA ") {
            continue;
        }
        kept.push(line);
    }
    let body = kept.join("\n");
    body.trim().to_owned()
}

/// `schema_migrations` 是否已存在。
pub(crate) fn has_schema_table(connection: &Connection) -> Result<bool, StorageError> {
    table_exists(connection, "schema_migrations")
}

/// 通用表存在性检查（只读 `sqlite_master`）。
pub(crate) fn table_exists(
    connection: &Connection,
    name: &str,
) -> Result<bool, StorageError> {
    let found: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

/// 读取已应用的 (version, checksum) 列表（升序）。
pub(crate) fn applied_migrations(
    connection: &Connection,
) -> Result<Vec<(u32, String)>, StorageError> {
    if !has_schema_table(connection)? {
        return Ok(Vec::new());
    }
    let mut statement = connection
        .prepare("SELECT version, checksum FROM schema_migrations ORDER BY version ASC")?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?)))?;
    let mut applied = Vec::new();
    for row in rows {
        applied.push(row?);
    }
    Ok(applied)
}

/// 执行迁移到 `target`（不超过本程序支持的最高版本）。
///
/// 每个版本单独一笔 `BEGIN IMMEDIATE` 事务：执行 DDL → 校验外键 → 写入版本与校验和
/// → COMMIT。任一步失败都会 ROLLBACK 并返回 `MIGRATION_FAILED`，库停留在上一个完整版本。
pub(crate) fn migrate(
    connection: &mut Connection,
    target: u32,
) -> Result<MigrationReport, StorageError> {
    let supported = target_version();
    if target > supported {
        return Err(StorageError::SchemaTooNew {
            found: target,
            supported,
        });
    }
    let applied = applied_migrations(connection)?;
    for (version, checksum) in &applied {
        let Some(definition) = MIGRATIONS.iter().find(|entry| entry.version == *version) else {
            return Err(StorageError::SchemaTooNew {
                found: *version,
                supported,
            });
        };
        if migration_checksum(definition.sql) != *checksum {
            return Err(StorageError::MigrationFailed(format!(
                "checksum mismatch for schema version {version} ({})",
                definition.name
            )));
        }
    }
    let from_version = applied.last().map(|(version, _)| *version).unwrap_or(0);
    if from_version > supported {
        return Err(StorageError::SchemaTooNew {
            found: from_version,
            supported,
        });
    }

    let mut applied_now = Vec::new();
    for definition in MIGRATIONS
        .iter()
        .filter(|entry| entry.version > from_version && entry.version <= target)
    {
        let body = normalized_body(definition.sql);
        let checksum = migration_checksum(definition.sql);
        connection.execute_batch("BEGIN IMMEDIATE")?;
        let outcome = apply_one(connection, definition, &body, &checksum);
        match outcome {
            Ok(()) => {
                connection.execute_batch("COMMIT")?;
                applied_now.push(definition.version);
            }
            Err(error) => {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error);
            }
        }
    }

    Ok(MigrationReport {
        from_version,
        to_version: applied_now.last().copied().unwrap_or(from_version),
        applied: applied_now,
    })
}

fn apply_one(
    connection: &Connection,
    definition: &MigrationDef,
    body: &str,
    checksum: &str,
) -> Result<(), StorageError> {
    if body.is_empty() {
        return Err(StorageError::MigrationFailed(format!(
            "migration {} is empty after normalization",
            definition.name
        )));
    }
    connection.execute_batch(body).map_err(|error| {
        StorageError::MigrationFailed(format!("{}: {error}", definition.name))
    })?;
    connection.execute(
        "INSERT INTO schema_migrations (version, checksum, applied_at) VALUES (?1, ?2, ?3)",
        params![definition.version, checksum, super::now_ms()],
    )?;
    // 迁移后立刻检查外键，避免把互相矛盾的 schema 记为“已迁移”。
    let mut statement = connection.prepare("PRAGMA foreign_key_check")?;
    let mut rows = statement.query([])?;
    if rows.next()?.is_some() {
        return Err(StorageError::MigrationFailed(format!(
            "{} leaves foreign key violations",
            definition.name
        )));
    }
    Ok(())
}

