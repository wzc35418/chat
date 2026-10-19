-- 设备登记与撤销集合（IDENTITY-011/012 的落库形态）。
--
-- 语义：
-- 1) device_id 是 PROTOCOL-001 设备文本（主键，天然幂等 upsert）；
-- 2) device_public_key 必须是 32 字节（表 CHECK + 写路径双重约束）；
-- 3) revoked_at_ms 为 NULL 表示**未撤销**；撤销是**单向**的（绝不因重登记而复用）；
-- 4) 撤销不删行：历史设备留在集合里（默认列表可隐藏，include_revoked 时可见）；
-- 5) label / revoked_reason 是本地诊断文本（有界、可打印，不含密钥）。
CREATE TABLE identity_devices (
    device_id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL,
    device_public_key BLOB NOT NULL CHECK (length(device_public_key) = 32),
    label TEXT NOT NULL,
    added_at_ms INTEGER NOT NULL CHECK (added_at_ms >= 0),
    revoked_at_ms INTEGER,
    revoked_reason TEXT,
    CHECK ((revoked_at_ms IS NULL) = (revoked_reason IS NULL))
);
CREATE INDEX identity_devices_account ON identity_devices(account_id);
