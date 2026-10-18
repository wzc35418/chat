-- W 包：NODE-012~015 附件密文 blob 上传/读取。
-- 复用 0001 的 blob_uploads / blob_chunks / blob_objects 四张表（不改其约束），只补：
--   1) 上传会话所需的公开字段（blob id / 群 / 关联条目 / 块大小 / 创建时间）；
--   2) 密文块内容表（与 blob_chunks 同事务写入，节点没有独立文件系统路径）；
--   3) 发布回执表（重复 complete 返回同一份节点签名回执）；
--   4) 已发布对象的密文字节数（按设备精确核算配额）。
-- 本脚本自带事务并记录自己的 schema 版本；校验和在 crate 内由 :checksum 占位符替换（见 services::migrate）。
BEGIN IMMEDIATE;
INSERT INTO schema_migrations (version, checksum, applied_at)
    VALUES (4, ':checksum', CAST(strftime('%s','now') AS INTEGER));

ALTER TABLE blob_uploads ADD COLUMN blob_id TEXT NOT NULL DEFAULT '';
ALTER TABLE blob_uploads ADD COLUMN group_id TEXT NOT NULL DEFAULT '';
ALTER TABLE blob_uploads ADD COLUMN source_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE blob_uploads ADD COLUMN chunk_bytes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE blob_uploads ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0;

ALTER TABLE blob_objects ADD COLUMN group_id TEXT NOT NULL DEFAULT '';
ALTER TABLE blob_objects ADD COLUMN source_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE blob_objects ADD COLUMN chunk_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE blob_objects ADD COLUMN cipher_bytes INTEGER NOT NULL DEFAULT 0;

-- 密文块内容：与 blob_chunks 的登记行在同一 IMMEDIATE 事务内写入，
-- 因此不存在“文件已写、DB 未登记”的双系统半写状态。
CREATE TABLE IF NOT EXISTS blob_chunk_content (
    upload_id TEXT NOT NULL,
    chunk_index INTEGER NOT NULL,
    ciphertext BLOB NOT NULL,
    PRIMARY KEY (upload_id, chunk_index),
    FOREIGN KEY (upload_id, chunk_index) REFERENCES blob_chunks(upload_id, chunk_index));

CREATE TABLE IF NOT EXISTS blob_receipts (
    blob_id TEXT PRIMARY KEY REFERENCES blob_objects(blob_id),
    upload_id TEXT NOT NULL,
    completed_at INTEGER NOT NULL CHECK(completed_at >= 0),
    receipt BLOB NOT NULL);

CREATE INDEX IF NOT EXISTS blob_uploads_owner ON blob_uploads(owner_device_id, state, expires_at);
CREATE INDEX IF NOT EXISTS blob_objects_owner ON blob_objects(owner_device_id, state);
COMMIT;
