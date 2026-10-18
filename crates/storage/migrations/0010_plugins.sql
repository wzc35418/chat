-- 插件宿主持久化（PLUGINS P4 增量的最小落库；不改写 0001~0009）。
--
-- 1) plugins：已安装插件的**清单原文**（JSON）+ 宿主状态（installed/stopped…）+ 时间戳；
--    清单原文是公开材料（不含密钥），因此明文存；状态变化只改 state/updated_at_ms。
-- 2) plugin_kv：每插件私有键值（值**密封**存储，用途 SealPurpose::PluginKv），
--    主键 (plugin_id, key) 保证同键幂等；plugin_id 外键到 plugins(id) 且 ON DELETE CASCADE，
--    卸载删除插件行时 KV 一起消失（delete_data=true 的语义）。
CREATE TABLE plugins (
    id TEXT PRIMARY KEY NOT NULL,
    manifest_json TEXT NOT NULL,
    state TEXT NOT NULL,
    installed_at_ms INTEGER NOT NULL CHECK (installed_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0)
);
CREATE TABLE plugin_kv (
    plugin_id TEXT NOT NULL REFERENCES plugins(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    value BLOB NOT NULL,
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (plugin_id, key)
);
CREATE INDEX plugin_kv_plugin ON plugin_kv(plugin_id);
