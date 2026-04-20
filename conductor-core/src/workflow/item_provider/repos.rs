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
