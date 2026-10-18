-- MLS 群状态与 provider 记录的正式加密持久化（P1 增量；不改写 0001/0002/0003）。
--
-- 设计约束：
--   1) 只新增表，不改既有表/迁移；
--   2) 所有状态与秘密都以 AEAD 密封记录落盘（用途 mls-group-state / mls-record，
--      AAD 绑定 group_id + epoch + 记录标识），明文列只有 group_id（不透明 ID）、
--      epoch、版本与时间戳；
--   3) epoch 单调：旧 epoch 不得覆盖新 epoch（应用层比较 + 事务内校验）；
--   4) provider 记录按 (group_id, record_key) 唯一，epoch 前进时整组替换，
--      旧 epoch 的秘密不会残留可读。

CREATE TABLE mls_group_state (
    group_id TEXT PRIMARY KEY CHECK (length(group_id) BETWEEN 1 AND 128),
    epoch INTEGER NOT NULL CHECK (epoch >= 0),
    state_version INTEGER NOT NULL CHECK (state_version >= 0),
    protected_state BLOB NOT NULL,
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0)
);

CREATE TABLE mls_record (
    group_id TEXT NOT NULL CHECK (length(group_id) BETWEEN 1 AND 128),
    record_key BLOB NOT NULL CHECK (length(record_key) BETWEEN 1 AND 512),
    record_epoch INTEGER NOT NULL CHECK (record_epoch >= 0),
    protected_value BLOB NOT NULL,
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0),
    PRIMARY KEY (group_id, record_key)
);

CREATE INDEX mls_record_epoch ON mls_record(group_id, record_epoch);
