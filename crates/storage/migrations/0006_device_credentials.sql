-- 身份设备凭据与撤销集合（P1 增量；不改写 0001~0005 的任何内容）。
--
-- 设计约束：
--   1) 只新增表。表名统一加 identity_ 前缀，避免与 0001 的 P0 表
--      （device_credentials / accounts / trusted_contacts）冲突——那些表属于 P0 实验路径，
--      本迁移不复用、不改写；
--   2) 敏感列一律以 AEAD 密封记录落盘：复用 identity-account 用途子密钥，但使用
--      独立 AAD 版本标签（device-credential-v1 / device-revocation-v1），并把
--      设备索引与账户索引都放进 AAD，因此密文挪行、换账户、换设备或换版本都会
--      认证失败（STATE_CORRUPTED）；
--   3) 明文列只有不可逆索引哈希、时间戳与严格约束：device_index 是
--      （账户 ID, 设备 ID）的域分隔哈希，用作幂等键并保证跨账户隔离；
--   4) 撤销是终态：identity_device_revocations 只增不减，账户撤销版本单调不减
--      （identity_device_revocation_epoch），任何再签发都不得清除撤销状态（防止静默复活）。

CREATE TABLE identity_device_credentials (
    device_index TEXT PRIMARY KEY CHECK (length(device_index) = 64),
    account_index TEXT NOT NULL CHECK (length(account_index) = 64),
    protected_credential BLOB NOT NULL,
    issued_at INTEGER NOT NULL CHECK (issued_at > 0),
    expires_at INTEGER NOT NULL CHECK (expires_at > issued_at),
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0)
);

CREATE INDEX identity_device_credentials_account
    ON identity_device_credentials(account_index, issued_at DESC);

CREATE TABLE identity_device_revocations (
    device_index TEXT PRIMARY KEY CHECK (length(device_index) = 64),
    account_index TEXT NOT NULL CHECK (length(account_index) = 64),
    protected_revocation BLOB NOT NULL,
    revocation_generation INTEGER NOT NULL CHECK (revocation_generation >= 1),
    revoked_at INTEGER NOT NULL CHECK (revoked_at >= 0)
);

CREATE INDEX identity_device_revocations_account
    ON identity_device_revocations(account_index, revoked_at);

CREATE TABLE identity_device_revocation_epoch (
    account_index TEXT PRIMARY KEY CHECK (length(account_index) = 64),
    generation INTEGER NOT NULL CHECK (generation >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0)
);
