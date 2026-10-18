-- P1-03：节点鉴权与设备注册。复用 0001 的 devices / auth_challenges / capability_revocations，
-- 只补充挑战绑定字段与节点固定身份、设备公钥、注册能力消费与注册回执表。
-- 本脚本自带事务并记录自己的 schema 版本；校验和在 crate 内由 :checksum 占位符替换（见 services::migrate）。
BEGIN IMMEDIATE;
INSERT INTO schema_migrations (version, checksum, applied_at)
    VALUES (2, ':checksum', CAST(strftime('%s','now') AS INTEGER));
ALTER TABLE auth_challenges ADD COLUMN node_id TEXT NOT NULL DEFAULT '';
ALTER TABLE auth_challenges ADD COLUMN purpose TEXT NOT NULL DEFAULT 'request_auth';
ALTER TABLE auth_challenges ADD COLUMN issued_at INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS auth_challenges_expiry ON auth_challenges(expires_at, consumed_at);
CREATE TABLE IF NOT EXISTS node_identity (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    node_id TEXT NOT NULL CHECK(length(node_id) > 0),
    public_key BLOB NOT NULL CHECK(length(public_key) = 32));
CREATE TABLE IF NOT EXISTS account_supervisors (
    account_id TEXT PRIMARY KEY,
    supervisor_public_key BLOB NOT NULL CHECK(length(supervisor_public_key) = 32),
    pinned_at INTEGER NOT NULL CHECK(pinned_at >= 0));
CREATE TABLE IF NOT EXISTS device_public_keys (
    device_id TEXT PRIMARY KEY REFERENCES devices(device_id),
    public_key BLOB NOT NULL CHECK(length(public_key) = 32));
CREATE TABLE IF NOT EXISTS consumed_registration_capabilities (
    capability_id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    consumed_at INTEGER NOT NULL CHECK(consumed_at >= 0));
CREATE TABLE IF NOT EXISTS device_registrations (
    device_id TEXT PRIMARY KEY REFERENCES devices(device_id),
    account_id TEXT NOT NULL,
    credential_hash BLOB NOT NULL CHECK(length(credential_hash) = 32),
    registered_at INTEGER NOT NULL CHECK(registered_at >= 0),
    receipt BLOB NOT NULL);
COMMIT;
