-- 身份账户、联系人与邀请一次性消费（P1 增量；不改写 0001/0002 的任何内容）。
--
-- 设计约束：
--   1) 只新增表，不修改既有表/迁移；
--   2) 敏感列一律以 AEAD 密封记录落盘（用途 identity-account / contact-record，
--      AAD 绑定行身份），明文列只有不可逆索引哈希与时间戳：
--      - identity_account.protected_account 内含账户/设备私钥种子、公钥、账户/设备 ID、
--        显示名与核验指纹；种子只在封装后才允许落盘；
--      - contacts.protected_record 内含对端账户 ID、显示名、观测指纹与联系人状态；
--   3) 单账户/单设备由 id = 1 的单行语义保证；contacts 用确定性 contact_id 作幂等键；
--   4) consumed_invites 以 invite_id 主键 + 摘要表达“一次性消费”，重复消费由主键冲突拒绝。

CREATE TABLE identity_account (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    account_index TEXT NOT NULL CHECK (length(account_index) = 64),
    protected_account BLOB NOT NULL,
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0)
);

CREATE TABLE contacts (
    contact_id TEXT PRIMARY KEY CHECK (length(contact_id) BETWEEN 1 AND 128),
    protected_record BLOB NOT NULL,
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0)
);

CREATE INDEX contacts_updated ON contacts(updated_at DESC, contact_id);

CREATE TABLE consumed_invites (
    invite_id TEXT PRIMARY KEY CHECK (length(invite_id) BETWEEN 1 AND 128),
    invite_digest BLOB NOT NULL CHECK (length(invite_digest) = 32),
    consumed_at INTEGER NOT NULL CHECK (consumed_at >= 0)
);
