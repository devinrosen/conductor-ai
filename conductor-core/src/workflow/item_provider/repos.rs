use std::collections::HashMap;

use crate::error::Result;
use runkon_flow::dsl::ForeachScope;

use super::{collect_fan_out_items, FanOutItem, ItemProvider, ProviderContext};

pub struct ReposProvider;

impl ItemProvider for ReposProvider {
    fn items(
        &self,
        ctx: &ProviderContext<'_>,
        _scope: Option<&ForeachScope>,
        _filter: &HashMap<String, String>,
    ) -> Result<Vec<FanOutItem>> {
        use crate::repo::RepoManager;

        let mgr = RepoManager::new(ctx.conn, ctx.config);
        let repos = mgr.list()?;
        Ok(collect_fan_out_items(repos, |r| FanOutItem {
            item_type: "repo".to_string(),
            item_id: r.id,
            item_ref: r.slug,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers;

    #[test]
    fn test_repos_items_returns_registered_repos() {
        let conn = test_helpers::setup_db();
        let config = crate::config::Config::default();
        let ctx = test_helpers::make_provider_ctx(&conn, &config);
        let items = ReposProvider.items(&ctx, None, &HashMap::new()).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item_id, "r1");
        assert_eq!(items[0].item_type, "repo");
    }

    #[test]
    fn test_repos_items_returns_all_without_dedup() {
        let conn = test_helpers::setup_db();
        let config = crate::config::Config::default();
        let ctx = test_helpers::make_provider_ctx(&conn, &config);
        // Providers return ALL items; dedup is done by the foreach executor.
        let items = ReposProvider.items(&ctx, None, &HashMap::new()).unwrap();
        assert_eq!(
            items.len(),
            1,
            "all repos returned regardless of prior state"
        );
    }
}
