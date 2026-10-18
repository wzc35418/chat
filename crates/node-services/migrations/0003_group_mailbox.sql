-- P1 群日志与密文信箱：只补“持久幂等结果”两表，不改动 0001/0002 的既有结构。
-- 群/成员/控制头/日志复用 0001 的 groups / group_members / group_control_heads / group_log；
-- 信箱条目复用 0001 的 mailbox_items（item_id 主键 + 全局 ordinal 稳定序）。
-- 本脚本自带事务并记录自己的 schema 版本；校验和在 crate 内由 :checksum 占位符替换（见 services::migrate）。
BEGIN IMMEDIATE;
INSERT INTO schema_migrations (version, checksum, applied_at)
    VALUES (3, ':checksum', CAST(strftime('%s','now') AS INTEGER));
CREATE TABLE IF NOT EXISTS group_registrations (
    group_id TEXT PRIMARY KEY REFERENCES groups(group_id),
    creation_hash BLOB NOT NULL CHECK(length(creation_hash) = 32),
    registered_at INTEGER NOT NULL CHECK(registered_at >= 0),
    receipt BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS mailbox_item_receipts (
    item_id TEXT PRIMARY KEY REFERENCES mailbox_items(item_id),
    payload_hash BLOB NOT NULL CHECK(length(payload_hash) = 32),
    stored_at INTEGER NOT NULL CHECK(stored_at >= 0),
    receipt BLOB NOT NULL);
CREATE INDEX IF NOT EXISTS group_members_device ON group_members(device_id, group_id);
COMMIT;
