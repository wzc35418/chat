-- CAP-AUTHZ 包：撤销授权策略（谁可以撤销哪个令牌）。
--
-- 1) capability_token_issuers：签发时记录 token_id → 签发设备，撤销时据此判断调用方是否
--    就是签发设备（self_only 默认策略）。token 本身仍不落库（token_id 由令牌文本确定性派生）。
-- 2) capability_token_revocations 扩展两列：谁执行的撤销（revoked_by_device）与授权依据
--    （authorization = self | policy_relaxed）——放宽策略下的跨设备撤销必须留下审计依据。
-- 本脚本自带事务并记录自己的 schema 版本；校验和在 crate 内由 :checksum 占位符替换（见 services::migrate）。
BEGIN IMMEDIATE;
INSERT INTO schema_migrations (version, checksum, applied_at)
    VALUES (8, ':checksum', CAST(strftime('%s','now') AS INTEGER));

CREATE TABLE capability_token_issuers (
    token_id TEXT PRIMARY KEY NOT NULL,
    issuer_device_id TEXT NOT NULL,
    issued_at_ms INTEGER NOT NULL CHECK (issued_at_ms >= 0)
);
CREATE INDEX capability_token_issuers_device
    ON capability_token_issuers(issuer_device_id);

ALTER TABLE capability_token_revocations
    ADD COLUMN revoked_by_device TEXT NOT NULL DEFAULT '';
ALTER TABLE capability_token_revocations
    ADD COLUMN authorization TEXT NOT NULL DEFAULT 'self';
COMMIT;
