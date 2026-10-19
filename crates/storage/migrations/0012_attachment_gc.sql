-- 附件两阶段回收（ATTACHMENTS-008 的存储侧；P1 增量，不改写 0001~0011）。
--
-- 语义：
-- 1) plan_attachment_gc 只**选**候选：无消息引用（message_attachments）、无 outbox 引用
--    （outbox_jobs.payload_ref）、且 created_at 早于 now - grace_ms；
-- 2) execute_attachment_gc 先**标记**（gc_pending = 1）再删除：标记与删除在同一张表上，
--    任何一步失败都能重入（已标记未删除的行会被下一次 plan 再次选中）；
-- 3) 删除 attachments 行时，attachment_secrets / attachment_chunks / attachment_progress /
--    message_attachments 通过既有 ON DELETE CASCADE 一并清除（PRAGMA foreign_keys=ON）。
ALTER TABLE attachments ADD COLUMN gc_pending INTEGER NOT NULL DEFAULT 0 CHECK (gc_pending IN (0, 1));
ALTER TABLE attachments ADD COLUMN gc_pending_at INTEGER NOT NULL DEFAULT 0 CHECK (gc_pending_at >= 0);
CREATE INDEX attachments_gc_pending ON attachments(gc_pending, created_at, attachment_id);
