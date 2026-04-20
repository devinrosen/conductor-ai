use std::collections::{HashMap, HashSet};

use crate::error::Result;
use crate::workflow_dsl::ForeachScope;

use super::{FanOutItem, ItemProvider, ProviderContext};

pub struct ReposProvider;

impl ItemProvider for ReposProvider {
    fn name(&self) -> &str {
        "repos"
    }

    fn items(
        &self,
        ctx: &ProviderContext<'_>,
        _scope: Option<&ForeachScope>,
        _filter: &HashMap<String, String>,
        existing_set: &HashSet<String>,
    ) -> Result<Vec<FanOutItem>> {
        use crate::repo::RepoManager;

        let mgr = RepoManager::new(ctx.conn, ctx.config);
        let repos = mgr.list()?;
        let mut items = Vec::new();
        for r in repos {
            if !existing_set.contains(&r.id) {
                items.push(FanOutItem {
                    item_type: "repo".to_string(),
                    item_id: r.id.clone(),
                    item_ref: r.slug.clone(),
                });
            }
        }
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers;

    fn make_ctx<'a>(
        conn: &'a rusqlite::Connection,
        config: &'a crate::config::Config,
    ) -> ProviderContext<'a> {
        ProviderContext {
            conn,
            config,
            repo_id: None,
            worktree_id: None,
        }
    }

    #[test]
    fn test_repos_items_returns_registered_repos() {
        let conn = test_helpers::setup_db();
        let config = crate::config::Config::default();
        let ctx = make_ctx(&conn, &config);
        let items = ReposProvider
            .items(&ctx, None, &HashMap::new(), &HashSet::new())
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item_id, "r1");
        assert_eq!(items[0].item_type, "repo");
    }

    #[test]
    fn test_repos_items_skips_existing_set() {
        let conn = test_helpers::setup_db();
        let config = crate::config::Config::default();
        let ctx = make_ctx(&conn, &config);
        let mut existing = HashSet::new();
        existing.insert("r1".to_string());
        let items = ReposProvider
            .items(&ctx, None, &HashMap::new(), &existing)
            .unwrap();
        assert!(
            items.is_empty(),
            "repo already in existing_set should be skipped"
        );
    }
}
