-- 凭据目录（IDENTITY 凭据 → 真实账户/设备 ID 的映射；STORAGE-016）。
--
-- 背景：同步 apply 与应用消息落库需要把 MLS/协议层的**凭据文本**映射成真实 AccountId。
-- 之前 core 用确定性占位派生（chat/sync/applied-author/v1），既不可查询也无法被身份层覆盖。
--
-- 语义：
-- 1) credential 是群内公开凭据文本（1..=128 字节），主键保证同凭据幂等 upsert；
-- 2) account_id / device_id 是 32 字节协议 ID（BLOB，长度由 CHECK 约束），不是秘密；
-- 3) updated_at_ms 由存储层给出，重放 upsert 只刷新时间戳；
-- 4) 条目数上限 1024（由写路径强制，见 formal/directory.rs），越界显式 INVALID_INPUT。
CREATE TABLE credential_directory (
    credential TEXT PRIMARY KEY NOT NULL,
    account_id BLOB NOT NULL CHECK (length(account_id) = 32),
    device_id BLOB NOT NULL CHECK (length(device_id) = 32),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0)
);
CREATE INDEX credential_directory_account ON credential_directory(account_id);
