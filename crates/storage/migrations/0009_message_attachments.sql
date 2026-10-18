-- 附件被消息引用的最小关联表（ATTACHMENTS-008 引用计数；P1 增量，不改写 0001~0008）。
--
-- 1) 主键 (message_id, attachment_id)：同一条消息重复引用同一附件幂等；
-- 2) message_id 外键到 messages(message_id)、attachment_id 外键到 attachments(attachment_id)，
--    两者都 ON DELETE CASCADE：删消息或删附件记录时关联自动消失，不留悬空引用；
-- 3) 只存两个不透明 ID 与创建时间，不含正文/密钥；缓存回收必须据此保留“被消息引用”的附件
--    （哪怕它的附件记录已经不存在）。
CREATE TABLE message_attachments (
    message_id TEXT NOT NULL REFERENCES messages(message_id) ON DELETE CASCADE,
    attachment_id TEXT NOT NULL REFERENCES attachments(attachment_id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    PRIMARY KEY (message_id, attachment_id)
);
CREATE INDEX message_attachments_attachment ON message_attachments(attachment_id);
