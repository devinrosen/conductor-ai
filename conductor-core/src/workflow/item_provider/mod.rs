use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use crate::config::Config;
use crate::error::Result;
use runkon_flow::dsl::ForeachScope;

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
}

/// Trait for a foreach item source registered with the engine.
pub trait ItemProvider: Send + Sync {
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

/// Collect `FanOutItem`s from an iterator, skipping ids already in `existing_set`.
///
/// Centralises the deduplication loop that every `ItemProvider::items()` needs:
/// `for item in list { if !existing_set.contains(&item.id) { items.push(...) } }`.
pub(super) fn collect_fan_out_items<T>(
    items: impl IntoIterator<Item = T>,
    existing_set: &HashSet<String>,
    get_id: impl Fn(&T) -> &str,
    to_item: impl Fn(T) -> FanOutItem,
) -> Vec<FanOutItem> {
    items
        .into_iter()
        .filter(|t| !existing_set.contains(get_id(t)))
        .map(to_item)
        .collect()
}

/// Fetch item IDs for a foreach step from the DB and return them, or `None` if the
/// step has no items yet (caller should return `Ok(vec![])`).
///
/// Eliminates the repeated open-connection+query+early-exit boilerplate that appeared
/// at the top of every `ItemProvider::dependencies()` impl.
pub(super) fn fetch_dep_item_ids(
    conn: &rusqlite::Connection,
    step_id: &str,
) -> crate::error::Result<Option<Vec<String>>> {
    let mgr = crate::workflow::manager::WorkflowManager::new(conn);
    let items = mgr.get_fan_out_items(step_id, None)?;
    let ids: Vec<String> = items.into_iter().map(|i| i.item_id).collect();
    if ids.is_empty() {
        Ok(None)
    } else {
        Ok(Some(ids))
    }
}

/// Extract a required `repo_id` from an `Option<String>`, returning a typed
/// `ConductorError::Workflow` when absent.  Used by providers that scope items
/// to a single repo (tickets, worktrees).
pub(super) fn require_repo_id<'a>(
    repo_id: &'a Option<String>,
    entity: &str,
) -> crate::error::Result<&'a str> {
    repo_id.as_deref().ok_or_else(|| {
        crate::error::ConductorError::Workflow(format!(
            "foreach over {entity} requires a repo_id in the execution context"
        ))
    })
}
