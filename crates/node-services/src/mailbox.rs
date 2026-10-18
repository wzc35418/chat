//! NODE-009~011：密文信箱投递、分页读取与幂等 ACK。
//!
//! 节点只存客户端密文（Welcome/应用密文），不持任何用户解密钥；读页只读不删，
//! ACK 才把条目转成 consumed 并释放配额。事务边界（TRANSACTIONS.md T4）：
//! 条目/dedup 回执/配额核算在同一 IMMEDIATE 事务；ACK 的消费状态与配额释放同事务。
use chat_protocol::{
    AccountId, ConversationId, CursorPosition, CursorResource, CursorScope, DeviceId, GroupId,
    OpaqueCursor, decode_page_cursor, encode_page_cursor,
};
use ed25519_dalek::{Signature, Signer};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::canonical::{
    MAILBOX_RECEIPT_DOMAIN, NODE_ACCOUNT_DERIVE_DOMAIN, NODE_DEVICE_DERIVE_DOMAIN, bytes32,
    decode_hex_bytes, hex_decode, hex_encode, push_field, sha256, sha256_concat,
    validate_bounded_id, validate_timestamp, verify_ed25519,
};
use crate::error::{NodeError, NodeResult};
use crate::group::{load_group, load_membership};
use crate::limits::{MAX_DEVICE_ID_BYTES, MAX_GROUP_ID_BYTES, MAX_MAILBOX_ITEM_ID_BYTES};
use crate::services::{AuthenticatedDevice, NodeService, to_i64};

/// 信箱游标排序版本；旧版本游标必须要求重查。
pub const MAILBOX_SORT_VERSION: u16 = 1;
/// 信箱游标稳定排序键（按节点分配的全局单调 ordinal）。
pub const MAILBOX_SORT_KEY: &[u8] = b"mailbox-ordinal";
/// 信箱游标会话维度域（信箱不按会话划分，固定绑定一个派生值）。
pub const MAILBOX_SCOPE_DOMAIN: &[u8] = b"chat/p1/node-mailbox-scope/v1";

/// 一条待投递的密文信箱条目（授权关联到已接受的群日志条目）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxItem {
    pub item_id: String,
    pub group_id: String,
    pub target_device_id: String,
    /// 已接受日志条目序号：该条目作者必须是发送设备本人。
    pub source_seq: u64,
    /// 绝对过期时间（Unix 秒），必须晚于投递时间且不超过节点 TTL 上限。
    pub expires_at: u64,
    /// 客户端密文（小写十六进制）；节点不解析明文、不保存文件密钥。
    pub ciphertext_envelope: String,
}

impl MailboxItem {
    /// 形状校验；不检查授权（那是 service 的职责）。
    pub fn validate_shape(&self, max_item_bytes: usize) -> NodeResult<Vec<u8>> {
        validate_bounded_id("mailbox item id", &self.item_id, MAX_MAILBOX_ITEM_ID_BYTES)?;
        validate_bounded_id("mailbox group id", &self.group_id, MAX_GROUP_ID_BYTES)?;
        GroupId::parse(&self.group_id)
            .map_err(|_| NodeError::InvalidRequest("mailbox group id"))?;
        validate_bounded_id(
            "mailbox target device id",
            &self.target_device_id,
            MAX_DEVICE_ID_BYTES,
        )?;
        DeviceId::parse(&self.target_device_id)
            .map_err(|_| NodeError::InvalidRequest("mailbox target device id"))?;
        if self.source_seq == 0 {
            return Err(NodeError::InvalidRequest("mailbox source seq"));
        }
        validate_timestamp("mailbox source seq", self.source_seq)?;
        validate_timestamp("mailbox expiry", self.expires_at)?;
        let payload =
            decode_hex_bytes("mailbox payload", &self.ciphertext_envelope, max_item_bytes)?;
        if payload.is_empty() {
            return Err(NodeError::InvalidRequest("mailbox payload"));
        }
        Ok(payload)
    }
}

/// 节点签发的信箱存储回执（幂等重试返回同一份）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxReceipt {
    pub item_id: String,
    pub group_id: String,
    pub target_device_id: String,
    pub sender_device_id: String,
    pub source_seq: u64,
    pub payload_hash: String,
    pub byte_size: u64,
    pub ordinal: u64,
    pub stored_at: u64,
    pub expires_at: u64,
    pub node_id: String,
    pub node_public_key: String,
    pub signature: String,
}

impl MailboxReceipt {
    /// 回执签名原文。
    pub fn signing_bytes(&self) -> NodeResult<Vec<u8>> {
        let node_public_key = hex_decode::<32>("node public key", &self.node_public_key)?;
        let payload_hash = hex_decode::<32>("payload hash", &self.payload_hash)?;
        validate_bounded_id("item id", &self.item_id, MAX_MAILBOX_ITEM_ID_BYTES)?;
        validate_bounded_id("group id", &self.group_id, MAX_GROUP_ID_BYTES)?;
        validate_bounded_id(
            "target device id",
            &self.target_device_id,
            MAX_DEVICE_ID_BYTES,
        )?;
        validate_bounded_id(
            "sender device id",
            &self.sender_device_id,
            MAX_DEVICE_ID_BYTES,
        )?;
        validate_timestamp("stored at", self.stored_at)?;
        validate_timestamp("expires at", self.expires_at)?;
        validate_timestamp("source seq", self.source_seq)?;
        validate_timestamp("ordinal", self.ordinal)?;
        let mut bytes = Vec::with_capacity(256);
        bytes.extend_from_slice(MAILBOX_RECEIPT_DOMAIN);
        push_field(&mut bytes, self.node_id.as_bytes());
        bytes.extend_from_slice(&node_public_key);
        push_field(&mut bytes, self.item_id.as_bytes());
        push_field(&mut bytes, self.group_id.as_bytes());
        push_field(&mut bytes, self.target_device_id.as_bytes());
        push_field(&mut bytes, self.sender_device_id.as_bytes());
        bytes.extend_from_slice(&self.source_seq.to_be_bytes());
        bytes.extend_from_slice(&payload_hash);
        bytes.extend_from_slice(&self.byte_size.to_be_bytes());
        bytes.extend_from_slice(&self.ordinal.to_be_bytes());
        bytes.extend_from_slice(&self.stored_at.to_be_bytes());
        bytes.extend_from_slice(&self.expires_at.to_be_bytes());
        Ok(bytes)
    }

    /// 用固定节点公钥验证回执。
    pub fn verify(&self, expected_node_public_key: &[u8; 32]) -> NodeResult<()> {
        let node_public_key = hex_decode::<32>("node public key", &self.node_public_key)?;
        if &node_public_key != expected_node_public_key {
            return Err(NodeError::NodeIdentityMismatch);
        }
        let signature = hex_decode::<64>("mailbox receipt signature", &self.signature)?;
        verify_ed25519(
            expected_node_public_key,
            &self.signing_bytes()?,
            &signature,
            || NodeError::InvalidSignature,
        )
    }

    /// 复核回执确实绑定到这份条目（客户端用它确认幂等结果）。
    pub fn verify_item(&self, item: &MailboxItem, payload: &[u8]) -> NodeResult<()> {
        if self.item_id != item.item_id
            || self.group_id != item.group_id
            || self.target_device_id != item.target_device_id
            || self.source_seq != item.source_seq
            || self.expires_at != item.expires_at
            || self.payload_hash != hex_encode(&sha256(payload))
            || self.byte_size != payload.len() as u64
        {
            return Err(NodeError::InvalidSignature);
        }
        Ok(())
    }
}

/// 一页信箱条目（只含密文与公开元数据）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxEntry {
    pub item_id: String,
    pub group_id: String,
    pub sender_device_id: String,
    pub source_seq: u64,
    pub payload_hash: String,
    pub byte_size: u64,
    pub ordinal: u64,
    pub expires_at: u64,
    pub ciphertext_envelope: String,
}

/// 一页信箱数据 + 下一页游标 + 过期缺口提示。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxPage {
    pub items: Vec<MailboxEntry>,
    pub next_cursor: Option<String>,
    /// 本设备当前已过期但未消费的条目数（客户端据此向用户提示历史缺口）。
    pub expired_items: u64,
}

/// ACK 结果：逐项状态与精确释放的字节数。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AckSummary {
    /// 本次真正转为 consumed 的条目（按请求顺序）。
    pub acknowledged: Vec<String>,
    /// 之前已经 consumed 的条目（重复 ACK 幂等，不再释放配额）。
    pub already_consumed: Vec<String>,
    /// 本次释放的未消费字节数。
    pub released_bytes: u64,
}

fn derive_account(account_id: &str) -> AccountId {
    AccountId::from_bytes(sha256_concat(&[NODE_ACCOUNT_DERIVE_DOMAIN, account_id.as_bytes()]))
}

fn derive_device(device_id: &str) -> DeviceId {
    DeviceId::from_bytes(sha256_concat(&[NODE_DEVICE_DERIVE_DOMAIN, device_id.as_bytes()]))
}

fn mailbox_scope(account_id: &str, device_id: &str) -> CursorScope {
    CursorScope {
        account: derive_account(account_id),
        device: derive_device(device_id),
        conversation: ConversationId::from_bytes(sha256_concat(&[MAILBOX_SCOPE_DOMAIN])),
        resource: CursorResource::Mailbox,
        sort_version: MAILBOX_SORT_VERSION,
    }
}

/// 该设备的当前持久占用（未过期且未消费的密文字节合计）。
fn used_bytes(tx: &rusqlite::Transaction<'_>, device_id: &str, now: i64) -> NodeResult<u64> {
    let used: i64 = tx.query_row(
        "SELECT COALESCE(SUM(byte_size), 0) FROM mailbox_items
         WHERE target_device_id = ?1 AND state = 'stored' AND expires_at > ?2",
        params![device_id, now],
        |row| row.get(0),
    )?;
    u64::try_from(used).map_err(|_| NodeError::StorageUnavailable)
}

/// 演员必须是本节点已登记的有效设备，且账户一致（跨账户直接拒绝）。
fn require_active_device(service: &NodeService, actor: &AuthenticatedDevice) -> NodeResult<()> {
    let (account_id, state, _version) = service
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

impl NodeService {
    /// NODE-009：配额内持久保存密文条目并返回节点签名存储回执。
    ///
    /// 授权关联：发送设备与目标设备都必须是该群当前有效成员，且 `source_seq` 指向
    /// 一条作者正是发送设备的已接受日志条目；匿名或无关联投递一律显式拒绝。
    pub fn store_mailbox_item(
        &mut self,
        item: &MailboxItem,
        actor: &AuthenticatedDevice,
        now: u64,
    ) -> NodeResult<MailboxReceipt> {
        validate_timestamp("now", now)?;
        let policy = self.policy().mailbox.clone();
        let payload = item.validate_shape(policy.max_item_bytes)?;
        let byte_size = u64::try_from(payload.len()).map_err(|_| NodeError::PayloadTooLarge)?;
        if item.expires_at <= now {
            return Err(NodeError::InvalidRequest("mailbox expiry"));
        }
        let max_expiry = now
            .checked_add(policy.default_ttl_seconds)
            .ok_or(NodeError::InvalidRequest("mailbox expiry"))?;
        if item.expires_at > max_expiry {
            return Err(NodeError::InvalidRequest("mailbox expiry"));
        }
        require_active_device(self, actor)?;
        let payload_hash = sha256(&payload);
        let node_id = self.node_id().to_owned();
        let node_public_key = self.node_public_key();
        let signing_key = self.signing_key().clone();
        let now_sql = to_i64("now", now)?;
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let group_row = load_group(&tx, &item.group_id)?.ok_or(NodeError::GroupNotFound)?;
        if group_row.state != "active" {
            return Err(NodeError::PermissionDenied);
        }
        // 目标设备必须是该群的有效成员（已加入且未退出）。
        let target =
            load_membership(&tx, &item.group_id, &item.target_device_id, group_row.last_seq)?
                .ok_or(NodeError::PermissionDenied)?;
        if target.1.is_some() {
            return Err(NodeError::PermissionDenied);
        }
        // 授权关联：source_seq 必须指向作者是本设备的一条已接受条目。
        let source_author: Option<String> = tx
            .query_row(
                "SELECT author_device_id FROM group_log WHERE group_id = ?1 AND group_seq = ?2",
                params![item.group_id, to_i64("source seq", item.source_seq)?],
                |row| row.get(0),
            )
            .optional()?;
        match source_author {
            Some(author) if author == actor.device_id => {}
            _ => return Err(NodeError::PermissionDenied),
        }
        // 幂等：同 item_id 同内容返回旧回执，相异内容显式拒绝。
        let existing: Option<(Vec<u8>, Vec<u8>)> = tx
            .query_row(
                "SELECT m.payload_hash, r.receipt
                 FROM mailbox_items m LEFT JOIN mailbox_item_receipts r ON r.item_id = m.item_id
                 WHERE m.item_id = ?1",
                [&item.item_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((stored_hash, stored_receipt)) = existing {
            if stored_hash.as_slice() != payload_hash.as_slice() {
                return Err(NodeError::ItemIdConflict);
            }
            let receipt: MailboxReceipt = serde_json::from_slice(&stored_receipt)?;
            receipt.verify(&node_public_key)?;
            receipt.verify_item(item, &payload)?;
            tx.commit()?;
            return Ok(receipt);
        }
        // 配额：只统计未过期未消费条目，与插入同一事务原子核算。
        let used = used_bytes(&tx, &item.target_device_id, now_sql)?;
        if used.saturating_add(byte_size) > policy.max_bytes_per_device {
            return Err(NodeError::QuotaExceeded);
        }
        let ordinal: i64 = tx.query_row(
            "SELECT COALESCE(MAX(ordinal), 0) + 1 FROM mailbox_items",
            [],
            |row| row.get(0),
        )?;
        let ordinal = u64::try_from(ordinal).map_err(|_| NodeError::StorageUnavailable)?;
        tx.execute(
            "INSERT INTO mailbox_items
                 (item_id, target_device_id, sender_device_id, payload_hash, ciphertext_envelope,
                  byte_size, expires_at, state, ordinal)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'stored', ?8)",
            params![
                item.item_id,
                item.target_device_id,
                actor.device_id,
                payload_hash.as_slice(),
                payload,
                to_i64("byte size", byte_size)?,
                to_i64("expires at", item.expires_at)?,
                to_i64("ordinal", ordinal)?
            ],
        )?;
        let mut receipt = MailboxReceipt {
            item_id: item.item_id.clone(),
            group_id: item.group_id.clone(),
            target_device_id: item.target_device_id.clone(),
            sender_device_id: actor.device_id.clone(),
            source_seq: item.source_seq,
            payload_hash: hex_encode(&payload_hash),
            byte_size,
            ordinal,
            stored_at: now,
            expires_at: item.expires_at,
            node_id,
            node_public_key: hex_encode(&node_public_key),
            signature: String::new(),
        };
        let signature: Signature = signing_key.sign(&receipt.signing_bytes()?);
        receipt.signature = hex_encode(&signature.to_bytes());
        tx.execute(
            "INSERT INTO mailbox_item_receipts (item_id, payload_hash, stored_at, receipt)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                receipt.item_id,
                payload_hash.as_slice(),
                now_sql,
                serde_json::to_vec(&receipt)?
            ],
        )?;
        tx.commit()?;
        Ok(receipt)
    }

    /// NODE-010：仅返回当前设备自己的有界页面（读不删除、可重复读）。
    pub fn read_mailbox_page(
        &mut self,
        actor: &AuthenticatedDevice,
        cursor: Option<&str>,
        limit: u32,
        now: u64,
    ) -> NodeResult<MailboxPage> {
        validate_timestamp("now", now)?;
        let policy = self.policy().mailbox.clone();
        if limit == 0 || limit > policy.max_page_items {
            return Err(NodeError::InvalidRequest("mailbox page limit"));
        }
        require_active_device(self, actor)?;
        let scope = mailbox_scope(&actor.account_id, &actor.device_id);
        let key = self.cursor_key();
        let now_ms = now.saturating_mul(1000);
        let after_ordinal = match cursor {
            Some(text) => {
                let token = OpaqueCursor::from_text(text);
                let position = decode_page_cursor(&token, &scope, &key, now_ms)
                    .map_err(|_| NodeError::InvalidCursor)?;
                position.seq
            }
            None => 0,
        };
        let now_sql = to_i64("now", now)?;
        let after_sql = to_i64("cursor", after_ordinal)?;
        let mut entries: Vec<MailboxEntry> = Vec::new();
        {
            let mut statement = self.connection().prepare(
                "SELECT m.item_id, m.sender_device_id, m.payload_hash, m.ciphertext_envelope,
                        m.byte_size, m.expires_at, m.ordinal, r.receipt
                 FROM mailbox_items m
                 JOIN mailbox_item_receipts r ON r.item_id = m.item_id
                 WHERE m.target_device_id = ?1 AND m.state = 'stored' AND m.expires_at > ?2
                   AND m.ordinal > ?3
                 ORDER BY m.ordinal LIMIT ?4",
            )?;
            let mut rows = statement.query(params![
                actor.device_id,
                now_sql,
                after_sql,
                i64::from(limit)
            ])?;
            let mut budget = 0_u64;
            while let Some(row) = rows.next()? {
                let byte_size =
                    u64::try_from(row.get::<_, i64>(4)?).map_err(|_| NodeError::StorageUnavailable)?;
                // 响应预算按密文字节核算；单条密文自身必须能放进一页（策略已校验）。
                if !entries.is_empty()
                    && budget.saturating_add(byte_size) > policy.max_page_bytes as u64
                {
                    break;
                }
                budget = budget.saturating_add(byte_size);
                let receipt: MailboxReceipt = serde_json::from_slice(&row.get::<_, Vec<u8>>(7)?)?;
                let envelope: Vec<u8> = row.get(3)?;
                entries.push(MailboxEntry {
                    item_id: row.get(0)?,
                    group_id: receipt.group_id,
                    sender_device_id: row.get(1)?,
                    source_seq: receipt.source_seq,
                    payload_hash: hex_encode(&bytes32("payload hash", row.get(2)?)?),
                    byte_size,
                    ordinal: u64::try_from(row.get::<_, i64>(6)?)
                        .map_err(|_| NodeError::StorageUnavailable)?,
                    expires_at: u64::try_from(row.get::<_, i64>(5)?)
                        .map_err(|_| NodeError::StorageUnavailable)?,
                    ciphertext_envelope: hex_encode(&envelope),
                });
            }
        }
        let expired: i64 = self.connection().query_row(
            "SELECT COUNT(*) FROM mailbox_items
             WHERE target_device_id = ?1 AND state = 'stored' AND expires_at <= ?2",
            params![actor.device_id, now_sql],
            |row| row.get(0),
        )?;
        let next_cursor = match entries.last() {
            Some(last) => {
                let more: Option<i64> = self
                    .connection()
                    .query_row(
                        "SELECT 1 FROM mailbox_items
                         WHERE target_device_id = ?1 AND state = 'stored' AND expires_at > ?2
                           AND ordinal > ?3 LIMIT 1",
                        params![
                            actor.device_id,
                            now_sql,
                            to_i64("cursor", last.ordinal)?
                        ],
                        |row| row.get(0),
                    )
                    .optional()?;
                if more.is_some() {
                    let position = CursorPosition {
                        sort_key: MAILBOX_SORT_KEY.to_vec(),
                        seq: last.ordinal,
                        tie: [0_u8; chat_protocol::CURSOR_TIE_BYTES],
                    };
                    Some(
                        encode_page_cursor(&scope, &position, &key, now_ms)
                            .map_err(|_| NodeError::StorageUnavailable)?
                            .as_str()
                            .to_owned(),
                    )
                } else {
                    None
                }
            }
            None => None,
        };
        Ok(MailboxPage {
            items: entries,
            next_cursor,
            expired_items: u64::try_from(expired).map_err(|_| NodeError::StorageUnavailable)?,
        })
    }

    /// NODE-011：幂等确认当前设备消费并释放条目配额。
    ///
    /// 批量原子：任一 id 未知或属于其他设备，整批拒绝且不写任何消费状态。
    pub fn ack_mailbox_items(
        &mut self,
        actor: &AuthenticatedDevice,
        ids: &[String],
        now: u64,
    ) -> NodeResult<AckSummary> {
        validate_timestamp("now", now)?;
        let policy = self.policy().mailbox.clone();
        if ids.is_empty() || ids.len() > policy.max_ack_batch {
            return Err(NodeError::InvalidAckBatch);
        }
        require_active_device(self, actor)?;
        for id in ids {
            validate_bounded_id("mailbox item id", id, MAX_MAILBOX_ITEM_ID_BYTES)?;
        }
        // 去重但保留请求顺序。
        let mut unique: Vec<&String> = Vec::with_capacity(ids.len());
        for id in ids {
            if !unique.iter().any(|existing| *existing == id) {
                unique.push(id);
            }
        }
        let tx = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut owned: Vec<(String, String, u64)> = Vec::with_capacity(unique.len());
        for id in &unique {
            let row: Option<(String, String, i64)> = tx
                .query_row(
                    "SELECT target_device_id, state, byte_size FROM mailbox_items WHERE item_id = ?1",
                    [id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((target, state, byte_size)) = row else {
                // 未知条目与他设备条目都按同一拒绝返回，不泄漏条目是否存在。
                return Err(NodeError::PermissionDenied);
            };
            if target != actor.device_id {
                return Err(NodeError::PermissionDenied);
            }
            owned.push((
                (*id).clone(),
                state,
                u64::try_from(byte_size).map_err(|_| NodeError::StorageUnavailable)?,
            ));
        }
        let mut acknowledged = Vec::with_capacity(owned.len());
        let mut already_consumed = Vec::with_capacity(owned.len());
        let mut released_bytes = 0_u64;
        for (item_id, state, byte_size) in &owned {
            if state != "stored" {
                already_consumed.push(item_id.clone());
                continue;
            }
            let updated = tx.execute(
                "UPDATE mailbox_items SET state = 'consumed'
                 WHERE item_id = ?1 AND state = 'stored'",
                [item_id.as_str()],
            )?;
            if updated != 1 {
                return Err(NodeError::StorageUnavailable);
            }
            released_bytes = released_bytes.saturating_add(*byte_size);
            acknowledged.push(item_id.clone());
        }
        tx.commit()?;
        Ok(AckSummary {
            acknowledged,
            already_consumed,
            released_bytes,
        })
    }
}
