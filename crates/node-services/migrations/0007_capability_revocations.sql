-- CAP-REV 包：能力令牌撤销表（API-004 无状态签名令牌 + 可撤销名单）。
--
-- token_id 由令牌文本确定性派生（capability_token_id），因此不必落库令牌本身；
-- revoked_at_ms 保留撤销时刻，用于按保留期清理（purge_expired_revocations）。
-- reason 是节点侧诊断短语（有界文本，不含用户内容）。
-- 本脚本自带事务并记录自己的 schema 版本；校验和在 crate 内由 :checksum 占位符替换（见 services::migrate）。
BEGIN IMMEDIATE;
INSERT INTO schema_migrations (version, checksum, applied_at)
    VALUES (7, ':checksum', CAST(strftime('%s','now') AS INTEGER));

-- 注意：0001 里的 capability_revocations 是**注册能力版本**的撤销表（issuer_id/capability_id），
-- 与这里的对象级能力令牌不是一回事，因此本表用独立名字避免语义混淆。
CREATE TABLE capability_token_revocations (
    token_id TEXT PRIMARY KEY NOT NULL,
    revoked_at_ms INTEGER NOT NULL CHECK (revoked_at_ms >= 0),
    reason TEXT NOT NULL
);
CREATE INDEX capability_token_revocations_revoked_at
    ON capability_token_revocations(revoked_at_ms);
COMMIT;
