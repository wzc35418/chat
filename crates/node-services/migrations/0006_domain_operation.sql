-- DM 包：节点日志条目的领域操作元数据投影（PROTOCOL-004 字段 55/56）。
-- 旧客户端不声明 → 两列为 NULL（向后兼容，旧条目读出也是 None）。
-- 本脚本自带事务并记录自己的 schema 版本；校验和在 crate 内由 :checksum 占位符替换（见 services::migrate）。
BEGIN IMMEDIATE;
INSERT INTO schema_migrations (version, checksum, applied_at)
    VALUES (6, ':checksum', CAST(strftime('%s','now') AS INTEGER));

ALTER TABLE group_log ADD COLUMN domain_operation TEXT;
ALTER TABLE group_log ADD COLUMN domain_target_digest TEXT;
COMMIT;
