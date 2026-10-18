//! PLUGINS-004 的纯逻辑子集：每插件命名空间键值存储。
//!
//! 隔离由类型与 API 保证：所有读写都以 `PluginId` 作为分区键，宿主只把绑定自身 ID
//! 的 [`crate::host::PluginRuntime`] 交给插件；跨插件读写需要伪造 ID，宿主路径不会这样做。
//! 每个分区独立计算条目数与字节数，任一分区超限只影响该插件。

use std::collections::BTreeMap;

use crate::error::{PluginError, Result};
use crate::limits::PluginLimits;
use crate::manifest::PluginId;

/// 单插件存储用量。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageUsage {
    /// 条目数。
    pub entries: usize,
    /// 键+值的总字节数（含键开销）。
    pub bytes: usize,
}

impl StorageUsage {
    /// 空用量。
    pub const EMPTY: StorageUsage = StorageUsage {
        entries: 0,
        bytes: 0,
    };
}

/// 每插件命名空间键值存储。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginStorage {
    partitions: BTreeMap<PluginId, BTreeMap<String, Vec<u8>>>,
}

fn validate_key(key: &str, limits: &PluginLimits) -> Result<()> {
    let bytes = key.as_bytes();
    if bytes.is_empty() {
        return Err(PluginError::InvalidStorageKey {
            detail: "key is empty",
        });
    }
    if bytes.len() > limits.max_storage_key_bytes {
        return Err(PluginError::InvalidStorageKey {
            detail: "key exceeds maxStorageKeyBytes",
        });
    }
    if !key.is_ascii() {
        return Err(PluginError::InvalidStorageKey {
            detail: "key must be ASCII",
        });
    }
    if !bytes.iter().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/')
    }) {
        return Err(PluginError::InvalidStorageKey {
            detail: "key contains unsupported characters",
        });
    }
    if bytes[0] == b'/' || bytes[bytes.len() - 1] == b'/' || key.contains("//") {
        return Err(PluginError::InvalidStorageKey {
            detail: "key must not contain empty path segments",
        });
    }
    Ok(())
}

impl PluginStorage {
    /// 写入/覆盖键值。
    ///
    /// 失败不修改任何分区：先完成全部配额检查，再插入。
    pub fn put(
        &mut self,
        plugin: &PluginId,
        key: &str,
        value: &[u8],
        limits: &PluginLimits,
    ) -> Result<()> {
        validate_key(key, limits)?;
        if value.len() > limits.max_storage_value_bytes {
            return Err(PluginError::ContentTooLarge {
                size: value.len(),
                limit: limits.max_storage_value_bytes,
            });
        }
        // 先做纯检查（不修改分区），失败时不留下空分区或部分写入。
        let (entries_after, bytes_after) = match self.partitions.get(plugin) {
            Some(partition) => {
                let previous = partition
                    .get(key)
                    .map_or(0usize, |value| key.len() + value.len());
                let total: usize = partition
                    .iter()
                    .map(|(stored_key, value)| stored_key.len() + value.len())
                    .sum();
                let entries_after = partition.len() + usize::from(!partition.contains_key(key));
                (entries_after, total - previous + key.len() + value.len())
            }
            None => (1, key.len() + value.len()),
        };
        if entries_after > limits.max_storage_entries {
            return Err(PluginError::StorageQuotaExceeded {
                detail: "entry limit",
            });
        }
        if bytes_after > limits.max_storage_bytes {
            return Err(PluginError::StorageQuotaExceeded {
                detail: "byte limit",
            });
        }
        self.partitions
            .entry(plugin.clone())
            .or_default()
            .insert(key.to_owned(), value.to_vec());
        Ok(())
    }

    /// 读取键值；不存在返回 `None`。
    pub fn get(&self, plugin: &PluginId, key: &str) -> Option<&[u8]> {
        self.partitions
            .get(plugin)
            .and_then(|partition| partition.get(key))
            .map(Vec::as_slice)
    }

    /// 删除键；返回是否删除。
    pub fn remove(&mut self, plugin: &PluginId, key: &str) -> bool {
        let removed = self
            .partitions
            .get_mut(plugin)
            .is_some_and(|partition| partition.remove(key).is_some());
        if removed && self.partitions.get(plugin).is_some_and(BTreeMap::is_empty) {
            self.partitions.remove(plugin);
        }
        removed
    }

    /// 列出该插件的全部键（升序）。
    pub fn keys(&self, plugin: &PluginId) -> Vec<String> {
        self.partitions
            .get(plugin)
            .map(|partition| partition.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// 该分区用量。
    pub fn usage(&self, plugin: &PluginId) -> StorageUsage {
        self.partitions
            .get(plugin)
            .map_or(StorageUsage::EMPTY, |partition| StorageUsage {
                entries: partition.len(),
                bytes: partition
                    .iter()
                    .map(|(key, value)| key.len() + value.len())
                    .sum(),
            })
    }

    /// 清空单个插件的全部数据；返回是否存在数据。
    pub fn clear(&mut self, plugin: &PluginId) -> bool {
        self.partitions.remove(plugin).is_some()
    }

    /// 当前存在数据的分区数（诊断/测试用）。
    pub fn partition_count(&self) -> usize {
        self.partitions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(id: &str) -> PluginId {
        PluginId::parse(id).unwrap()
    }

    #[test]
    fn partitions_are_isolated_and_overwrites_are_exact() {
        let mut storage = PluginStorage::default();
        let a = plugin("a.notes");
        let b = plugin("b.notes");
        let limits = PluginLimits::DEFAULT;
        storage.put(&a, "draft", b"alpha", &limits).unwrap();
        storage.put(&b, "draft", b"beta", &limits).unwrap();
        assert_eq!(storage.get(&a, "draft"), Some(&b"alpha"[..]));
        assert_eq!(storage.get(&b, "draft"), Some(&b"beta"[..]));
        assert_eq!(storage.keys(&a), vec!["draft".to_owned()]);
        // 覆盖不增加条目数。
        storage.put(&a, "draft", b"alpha2", &limits).unwrap();
        assert_eq!(storage.usage(&a).entries, 1);
        assert_eq!(storage.get(&a, "draft"), Some(&b"alpha2"[..]));
        // 删除未知键与空分区清理。
        assert!(!storage.remove(&a, "missing"));
        assert!(storage.remove(&a, "draft"));
        assert_eq!(storage.partition_count(), 1);
    }

    #[test]
    fn keys_and_values_are_bounded() {
        let mut storage = PluginStorage::default();
        let a = plugin("a.notes");
        let limits = PluginLimits::DEFAULT;
        for key in [
            "",
            "with space",
            "hash#",
            "trailing/",
            "/leading",
            "double//slash",
        ] {
            assert_eq!(
                storage.put(&a, key, b"v", &limits).unwrap_err().code(),
                "INVALID_STORAGE_KEY",
                "key {key:?}"
            );
        }
        let long_key = "k".repeat(limits.max_storage_key_bytes + 1);
        assert!(storage.put(&a, &long_key, b"v", &limits).is_err());
        let big_value = vec![0u8; limits.max_storage_value_bytes + 1];
        assert_eq!(
            storage
                .put(&a, "big", &big_value, &limits)
                .unwrap_err()
                .code(),
            "CONTENT_TOO_LARGE"
        );
        // 条目配额。
        let mut limits = PluginLimits::DEFAULT;
        limits.max_storage_entries = 2;
        storage.put(&a, "k1", b"v", &limits).unwrap();
        storage.put(&a, "k2", b"v", &limits).unwrap();
        assert_eq!(
            storage.put(&a, "k3", b"v", &limits).unwrap_err().code(),
            "PLUGIN_STORAGE_QUOTA_EXCEEDED"
        );
        // 已存在键的覆盖不受条目配额影响。
        storage.put(&a, "k1", b"v2", &limits).unwrap();
        // 字节配额。
        let mut limits = PluginLimits::DEFAULT;
        limits.max_storage_value_bytes = 32;
        limits.max_storage_bytes = 40;
        storage
            .put(&a, "a", b"12345678901234567890", &limits)
            .unwrap();
        assert_eq!(
            storage
                .put(&a, "b", b"12345678901234567890", &limits)
                .unwrap_err()
                .code(),
            "PLUGIN_STORAGE_QUOTA_EXCEEDED"
        );
    }

    #[test]
    fn quota_failure_does_not_create_partial_state() {
        let mut storage = PluginStorage::default();
        let a = plugin("a.notes");
        let mut limits = PluginLimits::DEFAULT;
        limits.max_storage_entries = 1;
        storage.put(&a, "k1", b"v", &limits).unwrap();
        assert!(storage.put(&a, "k2", b"v", &limits).is_err());
        assert_eq!(storage.usage(&a).entries, 1);
        assert_eq!(storage.keys(&a), vec!["k1".to_owned()]);
        // 空分区不会因首次写入失败而残留。
        let b = plugin("b.notes");
        limits.max_storage_bytes = 1;
        assert!(storage.put(&b, "k", b"vv", &limits).is_err());
        assert_eq!(storage.partition_count(), 1);
    }
}
