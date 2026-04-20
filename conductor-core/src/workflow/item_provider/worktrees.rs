use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use crate::config::Config;
use crate::error::{ConductorError, Result};
use crate::workflow_dsl::ForeachScope;
use crate::worktree::{Worktree, WorktreeManager};

use super::{FanOutItem, ItemProvider, ProviderContext};

pub struct WorktreesProvider;

impl ItemProvider for WorktreesProvider {
    fn name(&self) -> &str {
        "worktrees"
    }

    fn items(
        &self,
        ctx: &ProviderContext<'_>,
        scope: Option<&ForeachScope>,
        _filter: &HashMap<String, String>,
        existing_set: &HashSet<String>,
    ) -> Result<Vec<FanOutItem>> {
        let repo_id = ctx.repo_id.ok_or_else(|| {
            ConductorError::Workflow(
                "foreach over worktrees requires a repo_id in the execution context".to_string(),
            )
        })?;

        let wt_scope_opt = match scope {
            Some(ForeachScope::Worktree(s)) => Some(s),
            _ => None,
        };

        let base_branch_owned: String;
        let base_branch: &str = match wt_scope_opt.and_then(|s| s.base_branch.as_deref()) {
            Some(b) => b,
            None => {
                let wt_id = ctx.worktree_id.ok_or_else(|| {
                    ConductorError::Workflow(
                        "foreach over worktrees requires either scope = { base_branch = \"...\" } \
                         or a worktree_id in the execution context"
                            .to_string(),
                    )
                })?;
                let wt = WorktreeManager::new(ctx.conn, ctx.config).get_by_id(wt_id)?;
                base_branch_owned = wt.branch.clone();
                &base_branch_owned
            }
        };

        let wt_mgr = WorktreeManager::new(ctx.conn, ctx.config);
        let active_worktrees = wt_mgr.list_by_repo_id_and_base_branch(repo_id, base_branch)?;

        let mut candidates: Vec<_> = active_worktrees
            .into_iter()
            .filter(|wt| !existing_set.contains(&wt.id))
            .collect();

        if let Some(want_open_pr) = wt_scope_opt.and_then(|s| s.has_open_pr) {
            let repo = crate::repo::RepoManager::new(ctx.conn, ctx.config).get_by_id(repo_id)?;
            let open_prs = crate::github::list_open_prs(&repo.remote_url)?;
            candidates = filter_by_open_pr(candidates, want_open_pr, open_prs);
        }

        Ok(candidates
            .into_iter()
            .map(|wt| FanOutItem {
                item_type: "worktree".to_string(),
                item_id: wt.id,
                item_ref: wt.slug,
            })
            .collect())
    }

    fn supports_ordered(&self) -> bool {
        true
    }

    fn dependencies(&self, conn: &Connection, step_id: &str) -> Result<Vec<(String, String)>> {
        let config = crate::config::load_config().unwrap_or_default();
        dependencies_impl(conn, &config, step_id)
    }
}

fn filter_by_open_pr(
    mut candidates: Vec<Worktree>,
    want_open_pr: bool,
    open_prs: Vec<crate::github::GithubPr>,
) -> Vec<Worktree> {
    let open_branches: HashSet<String> = open_prs.into_iter().map(|pr| pr.head_ref_name).collect();
    candidates.retain(|wt| open_branches.contains(&wt.branch) == want_open_pr);
    candidates
}

fn dependencies_impl(
    conn: &Connection,
    config: &Config,
    step_id: &str,
) -> Result<Vec<(String, String)>> {
    let mgr = crate::workflow::manager::WorkflowManager::new(conn);
    let items = mgr.get_fan_out_items(step_id, None)?;
    let item_ids: Vec<String> = items.iter().map(|i| i.item_id.clone()).collect();
    if item_ids.is_empty() {
        return Ok(vec![]);
    }

    let id_set: HashSet<&String> = item_ids.iter().collect();
    let id_refs: Vec<&str> = item_ids.iter().map(String::as_str).collect();
    let wt_mgr = WorktreeManager::new(conn, config);
    let worktrees = match wt_mgr.get_by_ids(&id_refs) {
        Ok(wts) => wts,
        Err(e) => {
            tracing::warn!(
                "foreach: could not fetch worktrees for dep edges: {e}; skipping ordering"
            );
            return Ok(vec![]);
        }
    };
    let mut wt_ticket_map: HashMap<String, String> = HashMap::new();
    for wt in worktrees {
        if let Some(tid) = wt.ticket_id {
            wt_ticket_map.insert(wt.id, tid);
        }
    }

    let ticket_wt_map: HashMap<String, String> = wt_ticket_map
        .iter()
        .map(|(wt_id, tid)| (tid.clone(), wt_id.clone()))
        .collect();

    let ticket_ids: Vec<String> = wt_ticket_map.values().cloned().collect();
    if ticket_ids.is_empty() {
        return Ok(vec![]);
    }
    let ticket_id_refs: Vec<&str> = ticket_ids.iter().map(String::as_str).collect();
    let syncer = crate::tickets::TicketSyncer::new(conn);
    let dep_edges = syncer
        .get_blocking_edges_for_tickets(&ticket_id_refs)
        .map_err(|e| ConductorError::Workflow(format!("foreach: dependency query failed: {e}")))?;

    let mut edges: HashSet<(String, String)> = HashSet::new();
    for (blocker_ticket_id, dependent_ticket_id) in dep_edges {
        if let (Some(blocker_wt_id), Some(dependent_wt_id)) = (
            ticket_wt_map.get(&blocker_ticket_id),
            ticket_wt_map.get(&dependent_ticket_id),
        ) {
            if id_set.contains(blocker_wt_id) && id_set.contains(dependent_wt_id) {
                edges.insert((blocker_wt_id.clone(), dependent_wt_id.clone()));
            }
        }
    }

    Ok(edges.into_iter().collect())
}
