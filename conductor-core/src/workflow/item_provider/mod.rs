use std::any::Any;
use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use crate::config::Config;
use crate::error::Result;

pub mod repos;
pub mod tickets;
pub mod workflow_runs;
pub mod worktrees;

// ---------------------------------------------------------------------------
// Scope types (moved from runkon-flow)
// ---------------------------------------------------------------------------

/// Scope selector for ticket fan-outs.
#[derive(Debug, Clone)]
pub enum TicketScope {
    /// Ticket with the given internal ID (and its children via parent_of edges).
    TicketId(String),
    /// All open tickets with the given label in the repo.
    Label(String),
    /// All open tickets with no entries in ticket_labels.
    Unlabeled,
}

/// Scope selector for worktree fan-outs.
#[derive(Debug, Clone, Default)]
pub struct WorktreeScope {
    pub base_branch: Option<String>,
    pub has_open_pr: Option<bool>,
}

/// An item returned by an `ItemProvider` during fan-out.
pub struct FanOutItem {
    pub item_type: String,
    pub item_id: String,
    pub item_ref: String,
    /// Per-item data that will be injected into the child workflow as `item.<key>`.
    pub context: std::collections::HashMap<String, String>,
}

/// Context passed to providers during item collection.
pub struct ProviderContext<'a> {
    pub conn: &'a Connection,
    pub config: &'a Config,
}

/// Trait for a foreach item source registered with the engine.
pub trait ItemProvider: Send + Sync {
    fn name(&self) -> &str;

    /// Parse a raw scope KV map into a provider-specific opaque value.
    fn parse_scope(
        &self,
        raw: Option<&HashMap<String, String>>,
    ) -> crate::error::Result<Option<Box<dyn Any>>> {
        match raw {
            None => Ok(None),
            Some(_) => Err(crate::error::ConductorError::Workflow(format!(
                "provider '{}' does not support scope",
                self.name()
            ))),
        }
    }

    /// Return warnings about the scope (e.g. "no scope; falling back to context").
    fn scope_warnings(&self, _raw: Option<&HashMap<String, String>>) -> Vec<String> {
        vec![]
    }

    /// Whether a `filter` block is required.
    fn requires_filter(&self) -> bool {
        false
    }

    /// Validate filter key/value pairs.
    fn validate_filter(&self, _filter: &HashMap<String, String>) -> crate::error::Result<()> {
        Ok(())
    }

    fn items(
        &self,
        ctx: &ProviderContext<'_>,
        scope: Option<&dyn Any>,
        filter: &HashMap<String, String>,
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

/// Collect `FanOutItem`s from an iterator into the provider's return type.
///
/// Providers return all items unconditionally; the foreach executor owns the dedup.
pub(super) fn collect_fan_out_items<T>(
    items: impl IntoIterator<Item = T>,
    to_item: impl Fn(T) -> FanOutItem,
) -> Vec<FanOutItem> {
    items.into_iter().map(to_item).collect()
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
    let items = crate::workflow::get_fan_out_items(conn, step_id, None)?;
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

/// Build the `HashSet<&String>` and `Vec<&str>` pair used for dependency filtering.
pub(super) fn ids_to_set_and_refs(item_ids: &[String]) -> (HashSet<&String>, Vec<&str>) {
    let id_set: HashSet<&String> = item_ids.iter().collect();
    let id_refs: Vec<&str> = item_ids.iter().map(String::as_str).collect();
    (id_set, id_refs)
}

/// Standard error for a failed dependency query in a foreach step.
pub(super) fn dep_query_err(e: impl std::fmt::Display) -> crate::error::ConductorError {
    crate::error::ConductorError::Workflow(format!("foreach: dependency query failed: {e}"))
}
