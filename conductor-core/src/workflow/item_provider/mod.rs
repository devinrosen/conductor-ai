use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rusqlite::Connection;

use crate::config::Config;
use crate::error::Result;
use crate::workflow_dsl::ForeachScope;

pub mod repos;
pub mod tickets;
pub mod workflow_runs;
pub mod worktrees;

/// An item returned by an `ItemProvider` during fan-out.
pub struct FanOutItem {
    pub item_type: String,
    pub item_id: String,
    pub item_ref: String,
}

/// Context passed to providers during item collection.
pub struct ProviderContext<'a> {
    pub conn: &'a Connection,
    pub config: &'a Config,
    pub repo_id: Option<&'a str>,
    pub worktree_id: Option<&'a str>,
}

/// Trait for a foreach item source registered with the engine.
pub trait ItemProvider: Send + Sync {
    fn name(&self) -> &str;

    fn items(
        &self,
        ctx: &ProviderContext<'_>,
        scope: Option<&ForeachScope>,
        filter: &HashMap<String, String>,
        existing_set: &HashSet<String>,
    ) -> Result<Vec<FanOutItem>>;

    fn dependencies(
        &self,
        conn: &Connection,
        config: &Config,
        step_id: &str,
    ) -> Result<Vec<(String, String)>> {
        let _ = (conn, config, step_id);
        Ok(vec![])
    }

    fn supports_ordered(&self) -> bool {
        false
    }
}

/// Registry mapping provider names to implementations.
pub struct ItemProviderRegistry {
    providers: HashMap<String, Arc<dyn ItemProvider>>,
}

impl ItemProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }

    pub fn register<P: ItemProvider + 'static>(&mut self, provider: P) {
        let name = provider.name().to_string();
        self.providers.insert(name, Arc::new(provider));
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn ItemProvider>> {
        self.providers.get(name).cloned()
    }
}

impl Default for ItemProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the default registry with the four built-in providers.
pub fn build_default_registry() -> ItemProviderRegistry {
    let mut r = ItemProviderRegistry::new();
    r.register(tickets::TicketsProvider);
    r.register(repos::ReposProvider);
    r.register(workflow_runs::WorkflowRunsProvider);
    r.register(worktrees::WorktreesProvider);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyProvider;
    impl ItemProvider for DummyProvider {
        fn name(&self) -> &str {
            "dummy"
        }
        fn items(
            &self,
            _ctx: &ProviderContext<'_>,
            _scope: Option<&crate::workflow_dsl::ForeachScope>,
            _filter: &HashMap<String, String>,
            _existing_set: &HashSet<String>,
        ) -> Result<Vec<FanOutItem>> {
            Ok(vec![FanOutItem {
                item_type: "dummy".to_string(),
                item_id: "d1".to_string(),
                item_ref: "ref1".to_string(),
            }])
        }
    }

    #[test]
    fn test_registry_register_and_get() {
        let mut registry = ItemProviderRegistry::new();
        registry.register(DummyProvider);
        let p = registry.get("dummy");
        assert!(
            p.is_some(),
            "registered provider should be retrievable by name"
        );
        let missing = registry.get("nonexistent");
        assert!(
            missing.is_none(),
            "unregistered provider should return None"
        );
    }

    #[test]
    fn test_registry_get_returns_same_name() {
        let mut registry = ItemProviderRegistry::new();
        registry.register(DummyProvider);
        let p = registry.get("dummy").unwrap();
        assert_eq!(p.name(), "dummy");
    }

    #[test]
    fn test_build_default_registry_has_all_four_providers() {
        let registry = build_default_registry();
        for name in ["tickets", "repos", "workflow_runs", "worktrees"] {
            assert!(
                registry.get(name).is_some(),
                "build_default_registry should register '{name}'"
            );
        }
    }
}
