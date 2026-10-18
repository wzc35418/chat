-- 附件记录的正式加密持久化（P1 增量；不改写 0001~0004）。
--
-- 0001 已定义 attachments 主表与密封列 protected_descriptor，本迁移只补查询/排序
-- 需要的明文列（chunk_size/created_at/updated_at）与索引，不重建表。
--
-- 隐私边界：
--   1) 敏感元数据（明文内容摘要、密文摘要、文件名展示值）一律放进
--      protected_descriptor，用用途 attachment-record、AAD 绑定 attachment_id 密封；
--   2) 明文列只有不透明 attachment_id、state（受白名单约束的状态码）、大小/块数
--      与时间戳——这些与 TYPES.md 的公开清单同一级别，不含密钥；
--   3) 文件内容密钥（FileKey）不落盘：当前只在进程内，重启后导出必须显式失败，
--      不得伪造“可导出”。
ALTER TABLE attachments ADD COLUMN chunk_size INTEGER NOT NULL DEFAULT 262144 CHECK (chunk_size > 0);
ALTER TABLE attachments ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0 CHECK (created_at >= 0);
ALTER TABLE attachments ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0 CHECK (updated_at >= 0);
CREATE INDEX attachments_state_updated ON attachments(state, updated_at DESC);
CREATE INDEX attachments_created ON attachments(created_at DESC, attachment_id);
