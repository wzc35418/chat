-- 附件传输进度（断点续传）的正式持久化（P1 增量；不改写 0001~0007）。
--
-- 1) 主键 (attachment_id, direction)：一个附件每个方向只有一份进度快照；
--    direction 只允许 upload/download（应用层白名单 + 列 CHECK 兜底）；
-- 2) 外键到 attachments 并 ON DELETE CASCADE：删除附件记录时进度一并清除；
-- 3) 密封列保存 已确认块位图 + 节点上传会话 ID + blob 标识 + 下载前缀摘要
--    （用途 attachment-transfer-progress，AAD 绑定 attachment_id + direction，
--    换行/换方向都会认证失败）；明文列只有 attachment_id、direction、
--    chunk_count 与 updated_at——它们都是公开级元数据。
-- 4) chunk_count 保存声明值，读取时与 attachments.chunk_count 比对，不一致即
--    STATE_CORRUPTED（不接受“位图长度和当前记录不符”的进度）。
CREATE TABLE attachment_transfer_progress (
    attachment_id TEXT NOT NULL
        REFERENCES attachments(attachment_id) ON DELETE CASCADE,
    direction TEXT NOT NULL CHECK (direction IN ('upload', 'download')),
    chunk_count INTEGER NOT NULL CHECK (chunk_count > 0),
    protected_progress BLOB NOT NULL,
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0),
    PRIMARY KEY (attachment_id, direction)
);
CREATE INDEX attachment_transfer_progress_updated
    ON attachment_transfer_progress(updated_at DESC);
