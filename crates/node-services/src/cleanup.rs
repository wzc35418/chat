//! NODE-016：过期资源回收（每轮有上限、幂等、不删未过期对象）。
//!
//! 清理项（每类每轮最多 [CleanupPolicy::max_rows_per_kind] 行）：
//!
//! 1. **过期上传会话**：@blob_uploads.state='uploading' AND expires_at <= now@ → 删掉它的临时块
//!    （@blob_chunks@ + @blob_chunk_content@），会话行置 @expired@（保留行做幂等/审计，不删）；
//! 2. **过期已发布对象**：@blob_objects.state='published' AND expires_at > 0 AND expires_at <= now@ →
//!    删回执、删块内容、删对象行（**未过期对象永远不动**；@expires_at = 0@ 表示“未设置保留期”，也不动）；
//! 3. **过期信箱条目**：未 ACK（@state='stored'@）与已 ACK（@state='consumed'@）分别计数后删除；
//! 4. **过期挑战**与**过期公告记录**删除、**过期未领取密钥包**置 @expired@；
//! 5. **限流桶**不在本表里（进程内），由 [crate::rate_limit::RateLimiter::collect_expired] 处理，
//!    node-api 的 @AppState::collect_expired@ 把两边统计合并。
//!
//! 全部写操作在**同一个 IMMEDIATE 事务**里完成：要么整轮生效，要么整轮回滚，不留半清理状态。
use rusqlite::{TransactionBehavior, params};

use crate::error::NodeResult;
use crate::limits::CleanupPolicy;
use crate::services::{NodeService, to_i64};

/// 一轮回收的统计（全部是“本**轮**实际处理的条数”，不是累计值）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CleanupStats {
    /// 被置为 expired 的过期上传会话数。
    pub expired_upload_sessions: u64,
    /// 随过期会话删掉的临时密文块数（含块内容行）。
    pub expired_upload_chunks: u64,
    /// 被回收的已发布对象数。
    pub expired_blob_objects: u64,
    /// 随过期对象删掉的密文块数。
    pub expired_blob_chunks: u64,
    /// 过期未 ACK 的信箱条目数。
    pub expired_mailbox_unacked: u64,
    /// 过期已 ACK 的信箱条目数。
    pub expired_mailbox_acked: u64,
    /// 过期挑战数。
    pub expired_challenges: u64,
    /// 被置为 expired 的未领取密钥包数。
    pub expired_key_packages: u64,
    /// 被删除的过期公告记录数。
    pub expired_discovery_records: u64,
    /// 被清理的进程内限流桶数（由 node-api 合并进来）。
    pub expired_rate_limit_buckets: u64,
}

impl CleanupStats {
    /// 全零统计（策略关闭或无事可做时返回）。
    pub const ZERO: Self = Self {
        expired_upload_sessions: 0,
        expired_upload_chunks: 0,
        expired_blob_objects: 0,
        expired_blob_chunks: 0,
        expired_mailbox_unacked: 0,
        expired_mailbox_acked: 0,
        expired_challenges: 0,
        expired_key_packages: 0,
        expired_discovery_records: 0,
        expired_rate_limit_buckets: 0,
    };

    /// 本轮处理的总条数。
    pub fn total(&self) -> u64 {
        self.expired_upload_sessions
            .saturating_add(self.expired_upload_chunks)
            .saturating_add(self.expired_blob_objects)
            .saturating_add(self.expired_blob_chunks)
            .saturating_add(self.expired_mailbox_unacked)
            .saturating_add(self.expired_mailbox_acked)
            .saturating_add(self.expired_challenges)
            .saturating_add(self.expired_key_packages)
            .saturating_add(self.expired_discovery_records)
            .saturating_add(self.expired_rate_limit_buckets)
    }

    /// 是否什么都没做（幂等路径的判据）。
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

impl NodeService {
    /// NODE-016：回收所有过期资源，返回本轮统计。
    ///
    /// - **幂等**：同一时刻重复调用只会在第一轮有产出（第二轮 0），因为过期行已被删除/置态；
    /// - **有上限**：每类资源每轮最多 @policy.max_rows_per_kind@ 行，大库需要多轮收敛（不会长时间持写锁）；
    /// - **不越界**：未过期对象、未过期会话、未过期条目一律不动；@expires_at = 0@ 的已发布对象也被视为
    ///   “未设置保留期”，永不被本函数删除。
    pub fn collect_expired(&mut self, now: u64) -> NodeResult<CleanupStats> {
        crate::canonical::validate_timestamp("now", now)?;
        let policy: CleanupPolicy = self.policy().cleanup;
        if !policy.enabled {
            return Ok(CleanupStats::ZERO);
        }
        let limit = i64::from(policy.max_rows_per_kind);
        let now_sql = to_i64("now", now)?;
        let mut stats = CleanupStats::ZERO;
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        // 1) 过期上传会话（含临时块）。
        {
            let mut statement = tx.prepare(
                "SELECT upload_id FROM blob_uploads
                  WHERE state = 'uploading' AND expires_at <= ?1
                  ORDER BY expires_at LIMIT ?2",
            )?;
            let ids: Vec<String> = statement
                .query_map(params![now_sql, limit], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            drop(statement);
            for upload_id in ids {
                stats.expired_upload_chunks = stats
                    .expired_upload_chunks
                    .saturating_add(delete_upload_chunks(&tx, &upload_id)?);
                let updated = tx.execute(
                    "UPDATE blob_uploads SET state = 'expired'
                      WHERE upload_id = ?1 AND state = 'uploading'",
                    [upload_id.as_str()],
                )?;
                if updated == 1 {
                    stats.expired_upload_sessions = stats.expired_upload_sessions.saturating_add(1);
                }
            }
        }

        // 2) 过期已发布对象（未过期 / 未设置保留期的对象不动）。
        {
            let mut statement = tx.prepare(
                "SELECT blob_id, upload_id FROM blob_objects
                  WHERE state = 'published' AND expires_at > 0 AND expires_at <= ?1
                  ORDER BY expires_at LIMIT ?2",
            )?;
            let rows: Vec<(String, String)> = statement
                .query_map(params![now_sql, limit], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(statement);
            for (blob_id, upload_id) in rows {
                stats.expired_blob_chunks = stats
                    .expired_blob_chunks
                    .saturating_add(delete_upload_chunks(&tx, &upload_id)?);
                tx.execute("DELETE FROM blob_receipts WHERE blob_id = ?1", [blob_id.as_str()])?;
                let deleted = tx.execute("DELETE FROM blob_objects WHERE blob_id = ?1", [blob_id.as_str()])?;
                if deleted == 1 {
                    stats.expired_blob_objects = stats.expired_blob_objects.saturating_add(1);
                }
            }
        }

        // 3) 过期信箱条目：未 ACK 与已 ACK 分开计数（两者都要回收）。
        //    先删依赖行（投递回执），否则外键会拦住条目删除。
        tx.execute(
            "DELETE FROM mailbox_item_receipts WHERE item_id IN (
                 SELECT item_id FROM mailbox_items
                  WHERE expires_at <= ?1
                  ORDER BY expires_at LIMIT ?2)",
            params![now_sql, limit.saturating_mul(2)],
        )?;
        stats.expired_mailbox_unacked = tx.execute(
            "DELETE FROM mailbox_items WHERE item_id IN (
                 SELECT item_id FROM mailbox_items
                  WHERE state = 'stored' AND expires_at <= ?1
                  ORDER BY expires_at LIMIT ?2)",
            params![now_sql, limit],
        )? as u64;
        stats.expired_mailbox_acked = tx.execute(
            "DELETE FROM mailbox_items WHERE item_id IN (
                 SELECT item_id FROM mailbox_items
                  WHERE state = 'consumed' AND expires_at <= ?1
                  ORDER BY expires_at LIMIT ?2)",
            params![now_sql, limit],
        )? as u64;

        // 4) 过期挑战 / 过期公告 / 过期未领取密钥包。
        stats.expired_challenges = tx.execute(
            "DELETE FROM auth_challenges WHERE challenge_id IN (
                 SELECT challenge_id FROM auth_challenges
                  WHERE expires_at <= ?1 ORDER BY expires_at LIMIT ?2)",
            params![now_sql, limit],
        )? as u64;
        stats.expired_discovery_records = tx.execute(
            "DELETE FROM signed_discovery_records WHERE device_id IN (
                 SELECT device_id FROM signed_discovery_records
                  WHERE expires_at <= ?1 ORDER BY expires_at LIMIT ?2)",
            params![now_sql, limit],
        )? as u64;
        stats.expired_key_packages = tx.execute(
            "UPDATE key_packages SET state = 'expired' WHERE package_hash IN (
                 SELECT package_hash FROM key_packages
                  WHERE state = 'available' AND expires_at <= ?1
                  ORDER BY expires_at LIMIT ?2)",
            params![now_sql, limit],
        )? as u64;

        tx.commit()?;
        Ok(stats)
    }
}

/// 删掉某次上传的全部密文块（登记行 + 内容行），返回删除的块数。
fn delete_upload_chunks(tx: &rusqlite::Transaction<'_>, upload_id: &str) -> NodeResult<u64> {
    tx.execute(
        "DELETE FROM blob_chunk_content WHERE upload_id = ?1",
        [upload_id],
    )?;
    let chunks = tx.execute("DELETE FROM blob_chunks WHERE upload_id = ?1", [upload_id])?;
    Ok(chunks as u64)
}

