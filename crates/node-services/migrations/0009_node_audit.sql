-- NODE-018 包：审计持久化（只存元数据，不含正文/令牌/密钥）。
--
-- 1) node_audit：一条事件一行；device_id 为 NULL 表示节点级事件（所有设备可见）；
-- 2) 两个索引：按时间倒序扫描（at_ms）与按设备+时间过滤（device_id, at_ms）；
--    查询按 kind 过滤时复用 at_ms 索引即可（事件种类基数很小）。
-- 保留期由 purge_audit 按阈值批量删除（默认 7 天），迁移本身不设 TTL。
-- 本脚本自带事务并记录自己的 schema 版本；校验和在 crate 内由 :checksum 占位符替换（见 services::migrate）。
BEGIN IMMEDIATE;
INSERT INTO schema_migrations (version, checksum, applied_at)
    VALUES (9, ':checksum', CAST(strftime('%s','now') AS INTEGER));

CREATE TABLE node_audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL CHECK (length(kind) > 0),
    at_ms INTEGER NOT NULL CHECK (at_ms >= 0),
    device_id TEXT NULL,
    group_id TEXT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('ok','denied','failed')),
    code TEXT NULL,
    request_id TEXT NULL
);

CREATE INDEX node_audit_at_ms ON node_audit(at_ms);
CREATE INDEX node_audit_device_at_ms ON node_audit(device_id, at_ms);

COMMIT;
