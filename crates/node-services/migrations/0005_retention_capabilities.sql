-- NG 包：NODE-016 过期回收所需的保留期字段与清理索引（API-004 能力令牌是无状态签名，不需要表）。
-- 已发布附件此前没有保留期：本轮补 expires_at（0 表示“未设置”，永不被回收），
-- 并为按 (state, expires_at) 的回收查询补索引。
-- 本脚本自带事务并记录自己的 schema 版本；校验和在 crate 内由 :checksum 占位符替换（见 services::migrate）。
BEGIN IMMEDIATE;
INSERT INTO schema_migrations (version, checksum, applied_at)
    VALUES (5, ':checksum', CAST(strftime('%s','now') AS INTEGER));

-- 已发布对象的保留期（发布时写入 = 完成时刻 + BlobPolicy.published_ttl_seconds）。
ALTER TABLE blob_objects ADD COLUMN expires_at INTEGER NOT NULL DEFAULT 0;

-- NODE-016 的回收查询都是 (state, expires_at) 过滤 + LIMIT，逐类走索引而不是全表扫。
CREATE INDEX IF NOT EXISTS blob_objects_expiry ON blob_objects(state, expires_at);
CREATE INDEX IF NOT EXISTS blob_uploads_expiry ON blob_uploads(state, expires_at);
CREATE INDEX IF NOT EXISTS mailbox_items_expiry ON mailbox_items(state, expires_at);
CREATE INDEX IF NOT EXISTS auth_challenges_expiry ON auth_challenges(consumed_at, expires_at);
COMMIT;
