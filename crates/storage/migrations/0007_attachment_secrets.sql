-- 附件 FileKey 的正式加密持久化（P1 增量；不改写 0001~0005）。
--
-- 只新增表，不改既有表/迁移：
--   1) attachment_id 外键到 attachments 并 ON DELETE CASCADE：删除附件记录时密钥记录
--      一并清除（PRAGMA foreign_keys=ON 由 open_store 保证）；
--   2) 密封列保存 file_id(16) + FileKey(32) 的封存记录（用途 attachment-secret，
--      AAD 绑定 attachment_id + 记录版本）；明文列只有 attachment_id、版本号与时间戳；
--   3) 当前版本只允许单条记录（PK=attachment_id），重复保存同密钥幂等、异密钥拒绝。
CREATE TABLE attachment_secrets (
    attachment_id TEXT PRIMARY KEY
        REFERENCES attachments(attachment_id) ON DELETE CASCADE,
    version INTEGER NOT NULL CHECK (version > 0),
    protected_secret BLOB NOT NULL,
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0)
);
