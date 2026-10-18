//! PLUGINS-002/003 的纯逻辑子集：扩展点注册表。
//!
//! 注册表只记录「哪个插件注册了哪些扩展点」，不执行插件代码。真正的调用分发由
//! [`crate::host::PluginHost::dispatch`] 生成受预算约束的分发计划，执行仍由宿主负责。
//!
//! 语义：
//!
//! - 同一插件重复注册**相同**扩展点集合是幂等的（返回 `AlreadyRegistered`，不产生重复项）；
//! - 同一插件注册**不同**集合是冲突（`PLUGIN_STATE_CONFLICT`），避免静默覆盖；
//! - 注销幂等；
//! - 多个插件可以同时注册同一个扩展点，查询按插件 ID 排序返回。

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{PluginError, Result};
use crate::manifest::{ExtensionPoint, PluginId};

/// 注册结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationOutcome {
    /// 首次注册。
    Registered,
    /// 相同集合重复注册（幂等）。
    AlreadyRegistered,
}

/// 扩展点注册表。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtensionPointRegistry {
    registrations: BTreeMap<PluginId, BTreeSet<ExtensionPoint>>,
}

impl ExtensionPointRegistry {
    /// 注册（幂等）插件声明的扩展点集合。
    pub fn register(
        &mut self,
        plugin: &PluginId,
        points: &BTreeSet<ExtensionPoint>,
    ) -> Result<RegistrationOutcome> {
        match self.registrations.get(plugin) {
            Some(existing) if existing == points => Ok(RegistrationOutcome::AlreadyRegistered),
            Some(_) => Err(PluginError::RegistrationConflict),
            None => {
                self.registrations.insert(plugin.clone(), points.clone());
                Ok(RegistrationOutcome::Registered)
            }
        }
    }

    /// 注销插件全部注册项；返回是否有注册项被移除（幂等）。
    pub fn unregister(&mut self, plugin: &PluginId) -> bool {
        self.registrations.remove(plugin).is_some()
    }

    /// 插件是否已注册。
    pub fn is_registered(&self, plugin: &PluginId) -> bool {
        self.registrations.contains_key(plugin)
    }

    /// 插件是否注册了指定扩展点。
    pub fn supports(&self, plugin: &PluginId, point: ExtensionPoint) -> bool {
        self.registrations
            .get(plugin)
            .is_some_and(|points| points.contains(&point))
    }

    /// 插件的注册集合。
    pub fn points_of(&self, plugin: &PluginId) -> Option<&BTreeSet<ExtensionPoint>> {
        self.registrations.get(plugin)
    }

    /// 按扩展点查询已注册插件（按 ID 升序）。
    pub fn query(&self, point: ExtensionPoint) -> Vec<PluginId> {
        self.registrations
            .iter()
            .filter(|(_, points)| points.contains(&point))
            .map(|(plugin, _)| plugin.clone())
            .collect()
    }

    /// 已注册插件数量。
    pub fn registration_count(&self) -> usize {
        self.registrations.len()
    }

    /// 已注册扩展点总数（跨插件累计）。
    pub fn extension_count(&self) -> usize {
        self.registrations.values().map(BTreeSet::len).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(id: &str) -> PluginId {
        PluginId::parse(id).unwrap()
    }

    fn points(points: &[ExtensionPoint]) -> BTreeSet<ExtensionPoint> {
        points.iter().copied().collect()
    }

    #[test]
    fn repeated_registration_is_idempotent() {
        let mut registry = ExtensionPointRegistry::default();
        let a = plugin("a.notes");
        let set = points(&[ExtensionPoint::Command]);
        assert_eq!(
            registry.register(&a, &set).unwrap(),
            RegistrationOutcome::Registered
        );
        assert_eq!(
            registry.register(&a, &set).unwrap(),
            RegistrationOutcome::AlreadyRegistered
        );
        assert_eq!(registry.registration_count(), 1);
        assert_eq!(registry.extension_count(), 1);
        assert!(registry.supports(&a, ExtensionPoint::Command));
    }

    #[test]
    fn conflicting_registration_set_is_rejected() {
        let mut registry = ExtensionPointRegistry::default();
        let a = plugin("a.notes");
        registry
            .register(&a, &points(&[ExtensionPoint::Command]))
            .unwrap();
        assert_eq!(
            registry
                .register(&a, &points(&[ExtensionPoint::MessageAction]))
                .unwrap_err()
                .code(),
            "PLUGIN_STATE_CONFLICT"
        );
        // 冲突不改变既有集合。
        assert!(registry.supports(&a, ExtensionPoint::Command));
        assert!(!registry.supports(&a, ExtensionPoint::MessageAction));
    }

    #[test]
    fn unregister_is_idempotent_and_removes_all_points() {
        let mut registry = ExtensionPointRegistry::default();
        let a = plugin("a.notes");
        registry
            .register(
                &a,
                &points(&[ExtensionPoint::Command, ExtensionPoint::SettingsPanel]),
            )
            .unwrap();
        assert!(registry.unregister(&a));
        assert!(!registry.unregister(&a));
        assert!(!registry.is_registered(&a));
        assert!(registry.query(ExtensionPoint::Command).is_empty());
        assert_eq!(registry.registration_count(), 0);
    }

    #[test]
    fn query_returns_sorted_plugins_per_point() {
        let mut registry = ExtensionPointRegistry::default();
        registry
            .register(&plugin("b.notes"), &points(&[ExtensionPoint::Command]))
            .unwrap();
        registry
            .register(&plugin("a.notes"), &points(&[ExtensionPoint::Command]))
            .unwrap();
        registry
            .register(
                &plugin("c.notes"),
                &points(&[ExtensionPoint::MessageAction]),
            )
            .unwrap();
        let commands: Vec<String> = registry
            .query(ExtensionPoint::Command)
            .iter()
            .map(|plugin| plugin.as_str().to_owned())
            .collect();
        assert_eq!(commands, vec!["a.notes".to_owned(), "b.notes".to_owned()]);
        let actions: Vec<String> = registry
            .query(ExtensionPoint::MessageAction)
            .iter()
            .map(|plugin| plugin.as_str().to_owned())
            .collect();
        assert_eq!(actions, vec!["c.notes".to_owned()]);
    }
}
