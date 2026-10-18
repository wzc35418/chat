-- 正式加密存储路径（P0-04 基础）附加记录。
-- 主密钥本身不落盘：只保存 kdf 来源参数与一段 AEAD 校验密文（store_key_check.verifier）。
-- 恢复标记只记录“应该存在什么”，恢复流程据此核对，绝不据此重建已存在的密文。
CREATE TABLE store_meta (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    store_version INTEGER NOT NULL CHECK(store_version >= 0),
    created_at INTEGER NOT NULL
);
CREATE TABLE store_key_check (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    kdf_version INTEGER NOT NULL CHECK(kdf_version > 0),
    params BLOB NOT NULL,
    verifier BLOB NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE store_recovery_markers (
    marker_id INTEGER PRIMARY KEY AUTOINCREMENT,
    stage TEXT NOT NULL CHECK(stage IN ('draft_prepared','attempt_prepared')),
    draft_id TEXT NOT NULL,
    submission_id TEXT,
    mark_hash BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    CHECK((stage = 'draft_prepared' AND submission_id IS NULL)
       OR (stage = 'attempt_prepared' AND submission_id IS NOT NULL))
);
CREATE UNIQUE INDEX store_recovery_markers_key
    ON store_recovery_markers(stage, draft_id, IFNULL(submission_id, ''));
CREATE TABLE store_quarantine (
    quarantine_id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    ref_id TEXT NOT NULL,
    protected_detail BLOB NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX store_quarantine_key ON store_quarantine(kind, ref_id);
