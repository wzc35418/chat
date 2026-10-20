-- MLSDEV（TASK-130）：**设备级**的 MLS provider 记录（无群设备的 KeyPackage 私钥也在此）。
--
-- 背景：KeyPackage 的 init 私钥只存在于 OpenMlsRustCrypto 的内存 provider 里。为了让
-- "进程 A 导出 KeyPackage → 进程 B 用同一库入群" 成立，必须把设备的 provider 记录加密落库。
--
-- 语义：
-- 1) 一台设备一行（device_key 是调用方给的稳定标识），state 是设备头（凭据名/签名公钥等，
--    同样密封），记录值按 (device_key, record_key) 绑定的 AAD 密封；
-- 2) 整组写入是**单笔事务**：头与记录要么一起提交、要么一起回滚；
-- 3) 记录随设备行级联删除（ON DELETE CASCADE）；
-- 4) 上限由写路径强制（条目数与单条字节），越界显式 INVALID_INPUT。
CREATE TABLE mls_device_state (
    device_key TEXT PRIMARY KEY NOT NULL,
    state_version INTEGER NOT NULL CHECK (state_version >= 1),
    protected_state BLOB NOT NULL,
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0)
);
CREATE TABLE mls_device_record (
    device_key TEXT NOT NULL,
    record_key TEXT NOT NULL,
    protected_value BLOB NOT NULL,
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0),
    PRIMARY KEY (device_key, record_key),
    FOREIGN KEY (device_key) REFERENCES mls_device_state(device_key) ON DELETE CASCADE
);
