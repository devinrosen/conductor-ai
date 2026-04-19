use std::collections::{HashMap, HashSet};

#[cfg(test)]
thread_local! {
    static BETWEEN_CYCLE_HOOK: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
fn set_between_cycle_hook(hook: impl FnMut() + 'static) {
    BETWEEN_CYCLE_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
fn clear_between_cycle_hook() {
    BETWEEN_CYCLE_HOOK.with(|h| *h.borrow_mut() = None);
}

#[cfg(test)]
fn call_between_cycle_hook() {
    BETWEEN_CYCLE_HOOK.with(|h| {
        if let Some(hook) = h.borrow_mut().as_mut() {
            hook();
        }
    });
}

#[cfg(not(test))]
#[inline(always)]
fn call_between_cycle_hook() {}

use crate::error::{ConductorError, Result};
use crate::workflow::engine::{
    record_step_failure, record_step_success, restore_step, should_skip, ExecutionState,
};
use crate::workflow::prompt_builder::build_variable_map;
use crate::workflow::run_context::RunContext;
use crate::workflow::status::WorkflowStepStatus;
use crate::workflow::types::WorkflowExecStandalone;
use crate::workflow_dsl::{ForEachNode, ForeachOver, ForeachScope, OnChildFail};
use crate::worktree::WorktreeManager;

/// Execute a `foreach` step: fan out a child workflow over a collection of items.
///
/// Three-phase execution:
/// 1. **Item collection** — query tickets/repos/workflow_runs from DB, write fan_out_items rows.
/// 2. **Dispatch loop** — dispatch child workflows up to max_parallel, poll completions.
/// 3. **Step completion** — record success/failure with context summary.
pub fn execute_foreach(
    state: &mut ExecutionState<'_>,
    node: &ForEachNode,
    iteration: u32,
) -> Result<()> {
    let pos = state.position;
    state.position += 1;

    let step_key = format!("foreach:{}", node.name);

    // Skip on resume if already completed.
    if should_skip(state, &step_key, iteration) {
        tracing::info!("foreach '{}': skipping completed step", node.name);
        restore_step(state, &step_key, iteration);
        return Ok(());
    }

    // Find or create the step record.
    // On resume, reuse the existing non-completed step row so that fan_out_items
    // inserted under the old step_id remain visible and max_parallel is respected.
    let existing_step = if state.resume_ctx.is_some() {
        state.wf_mgr.find_step_by_name_and_iteration(
            &state.workflow_run_id,
            &step_key,
            iteration as i64,
        )?
    } else {
        None
    };

    let step_id = if let Some(existing) = existing_step {
        state.wf_mgr.update_step_status(
            &existing.id,
            WorkflowStepStatus::Running,
            None,
            None,
            None,
            None,
            Some(0),
        )?;
        // Reset orphaned running items (no child_run_id) so they are re-dispatched.
        state
            .wf_mgr
            .reset_running_items_without_child_run(&existing.id)?;
        existing.id
    } else {
        let id = state.wf_mgr.insert_step(
            &state.workflow_run_id,
            &step_key,
            "foreach",
            false,
            pos,
            iteration as i64,
        )?;
        state.wf_mgr.update_step_status(
            &id,
            WorkflowStepStatus::Running,
            None,
            None,
            None,
            None,
            Some(0),
        )?;
        id
    };

    let (working_dir, repo_path) = {
        let ctx = crate::workflow::run_context::WorktreeRunContext::new(state);
        (
            ctx.working_dir().to_path_buf(),
            ctx.repo_path().to_path_buf(),
        )
    };

    // Load the child workflow definition (needed for input resolution).
    let child_def = crate::workflow_dsl::load_workflow_by_name(
        working_dir.to_str().unwrap_or(""),
        repo_path.to_str().unwrap_or(""),
        &node.workflow,
    )
    .map_err(|e| {
        ConductorError::Workflow(format!(
            "foreach '{}': failed to load child workflow '{}': {e}",
            node.name, node.workflow
        ))
    })?;

    // --- Phase 1: Item collection ---
    // Check for existing fan_out_items rows (resume case).
    let existing_ids = state.wf_mgr.get_existing_fan_out_item_ids(&step_id)?;
    let existing_set: HashSet<String> = existing_ids.into_iter().collect();

    let items = collect_items(state, node, &existing_set)?;

    // Write pending rows for newly discovered items.
    let total_items = {
        let mut count = 0i64;
        for (item_type, item_id, item_ref) in &items {
            if !existing_set.contains(item_id) {
                state
                    .wf_mgr
                    .insert_fan_out_item(&step_id, item_type, item_id, item_ref)?;
                count += 1;
            }
        }
        // Total = newly inserted + already existing
        count + existing_set.len() as i64
    };

    if total_items > 0 {
        // Update or set the fan_out_total (always recalculate from all items).
        let actual_total = state.wf_mgr.get_fan_out_items(&step_id, None)?.len() as i64;
        state.wf_mgr.set_fan_out_total(&step_id, actual_total)?;
    }

    tracing::info!(
        "foreach '{}': {} items to process (over={:?}, max_parallel={})",
        node.name,
        total_items,
        node.over,
        node.max_parallel,
    );

    if total_items == 0 {
        let context = format!("foreach {}: no items to process", node.name);
        state.wf_mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Completed,
            None,
            Some(&context),
            Some(&context),
            None,
            Some(0),
        )?;
        record_step_success(
            state,
            step_key,
            &node.name,
            Some(context.clone()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            vec![],
            context,
            None,
            iteration,
            None,
            None,
        );
        return Ok(());
    }

    // --- Phase 2: Dispatch loop ---
    let result = run_dispatch_loop(state, node, &step_id, &child_def, iteration);

    // --- Phase 3: Step completion ---
    // Reload counters from DB for accurate summary.
    let fan_out_items = state.wf_mgr.get_fan_out_items(&step_id, None)?;
    let completed_count = fan_out_items
        .iter()
        .filter(|i| i.status == "completed")
        .count();
    let failed_count = fan_out_items
        .iter()
        .filter(|i| i.status == "failed")
        .count();
    let skipped_count = fan_out_items
        .iter()
        .filter(|i| i.status == "skipped")
        .count();
    let total = fan_out_items.len();

    let context = format!(
        "foreach {}: {completed_count} completed, {failed_count} failed, {skipped_count} skipped of {total} {}",
        node.name,
        match node.over {
            ForeachOver::Tickets => "tickets",
            ForeachOver::Repos => "repos",
            ForeachOver::WorkflowRuns => "workflow_runs",
            ForeachOver::Worktrees => "worktrees",
        },
    );

    let step_succeeded = match result {
        Ok(stalled) => {
            if stalled {
                // Stall = completed with warning marker (RFC decision 10)
                true
            } else {
                failed_count == 0
            }
        }
        Err(ref e) => {
            tracing::warn!("foreach '{}': dispatch loop error: {e}", node.name);
            false
        }
    };

    if step_succeeded {
        let stall_marker = if matches!(&result, Ok(true)) {
            vec!["foreach_stalled".to_string()]
        } else {
            vec![]
        };
        let markers_json = serde_json::to_string(&stall_marker).unwrap_or_default();

        state.wf_mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Completed,
            None,
            Some(&context),
            Some(&context),
            Some(&markers_json),
            Some(0),
        )?;

        record_step_success(
            state,
            step_key,
            &node.name,
            Some(context.clone()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            stall_marker,
            context,
            None,
            iteration,
            None,
            None,
        );
    } else {
        let error_msg = match result {
            Err(ref e) => format!("foreach '{}' failed: {e}", node.name),
            Ok(_) => format!(
                "foreach '{}': {failed_count} of {total} items failed",
                node.name
            ),
        };

        state.wf_mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Failed,
            None,
            Some(&error_msg),
            Some(&context),
            None,
            Some(0),
        )?;

        return record_step_failure(state, step_key, &node.name, error_msg, 1, true);
    }

    Ok(())
}

/// Collect fan-out items from the DB, excluding any already in `existing_set`.
/// Returns a Vec of (item_type, item_id, item_ref).
fn collect_items(
    state: &mut ExecutionState<'_>,
    node: &ForEachNode,
    existing_set: &HashSet<String>,
) -> Result<Vec<(String, String, String)>> {
    match node.over {
        ForeachOver::Tickets => collect_ticket_items(state, node, existing_set),
        ForeachOver::Repos => collect_repo_items(state, existing_set),
        ForeachOver::WorkflowRuns => collect_workflow_run_items(state, node, existing_set),
        ForeachOver::Worktrees => collect_worktree_items(state, node, existing_set),
    }
}

fn collect_ticket_items(
    state: &mut ExecutionState<'_>,
    node: &ForEachNode,
    existing_set: &HashSet<String>,
) -> Result<Vec<(String, String, String)>> {
    use crate::tickets::{TicketFilter, TicketSyncer};

    let syncer = TicketSyncer::new(state.conn);

    // Determine repo scope: require a repo_id for ticket fan-outs.
    let repo_id_owned = {
        let ctx = crate::workflow::run_context::WorktreeRunContext::new(state);
        ctx.repo_id().map(String::from)
    };
    let repo_id = repo_id_owned.as_deref().ok_or_else(|| {
        ConductorError::Workflow(
            "foreach over tickets requires a repo_id in the execution context".to_string(),
        )
    })?;

    let mut items = Vec::new();

    match &node.scope {
        Some(ForeachScope::Ticket(ts)) => match ts {
            crate::workflow_dsl::TicketScope::TicketId(ticket_id) => {
                // Single ticket — just look it up directly.
                match syncer.get_by_id(ticket_id) {
                    Ok(t) if !existing_set.contains(&t.id) => {
                        items.push(("ticket".to_string(), t.id.clone(), t.source_id.clone()));
                    }
                    Ok(_) => {} // Already in existing_set
                    Err(crate::error::ConductorError::TicketNotFound { .. }) => {
                        return Err(ConductorError::Workflow(format!(
                            "foreach: ticket '{}' not found",
                            ticket_id
                        )));
                    }
                    Err(e) => return Err(e),
                }
            }
            crate::workflow_dsl::TicketScope::Label(label) => {
                let filter = TicketFilter {
                    labels: vec![label.clone()],
                    search: None,
                    include_closed: false,
                    unlabeled_only: false,
                };
                let tickets = syncer.list_filtered(Some(repo_id), &filter)?;
                for t in tickets {
                    if !existing_set.contains(&t.id) {
                        items.push(("ticket".to_string(), t.id.clone(), t.source_id.clone()));
                    }
                }
            }
            crate::workflow_dsl::TicketScope::Unlabeled => {
                let filter = TicketFilter {
                    labels: vec![],
                    search: None,
                    include_closed: false,
                    unlabeled_only: true,
                };
                let tickets = syncer.list_filtered(Some(repo_id), &filter)?;
                for t in tickets {
                    if !existing_set.contains(&t.id) {
                        items.push(("ticket".to_string(), t.id.clone(), t.source_id.clone()));
                    }
                }
            }
        },
        Some(ForeachScope::Worktree(_)) => {
            return Err(ConductorError::Workflow(
                "foreach over = tickets does not accept a worktree scope; use over = worktrees instead".to_string(),
            ));
        }
        None => {
            // No ticket scope — collect all open tickets for the repo.
            let filter = TicketFilter {
                labels: vec![],
                search: None,
                include_closed: false,
                unlabeled_only: false,
            };
            let tickets = syncer.list_filtered(Some(repo_id), &filter)?;
            for t in tickets {
                if !existing_set.contains(&t.id) {
                    items.push(("ticket".to_string(), t.id.clone(), t.source_id.clone()));
                }
            }
        }
    }

    Ok(items)
}

fn collect_repo_items(
    state: &mut ExecutionState<'_>,
    existing_set: &HashSet<String>,
) -> Result<Vec<(String, String, String)>> {
    use crate::repo::RepoManager;

    let mgr = RepoManager::new(state.conn, state.config);
    let repos = mgr.list()?;
    let mut items = Vec::new();
    for r in repos {
        if !existing_set.contains(&r.id) {
            items.push(("repo".to_string(), r.id.clone(), r.slug.clone()));
        }
    }
    Ok(items)
}

fn collect_workflow_run_items(
    state: &mut ExecutionState<'_>,
    node: &ForEachNode,
    existing_set: &HashSet<String>,
) -> Result<Vec<(String, String, String)>> {
    // Build filter from node.filter map.
    let status_filter = node.filter.get("status").map(|s| s.as_str()).unwrap_or("");
    let workflow_name_filter = node
        .filter
        .get("workflow_name")
        .map(|s| s.as_str())
        .unwrap_or("");

    let terminal_statuses = ["completed", "failed", "cancelled"];
    let statuses: Vec<&str> = if status_filter.is_empty() {
        terminal_statuses.to_vec()
    } else {
        // Support comma-separated list of statuses
        status_filter
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect()
    };

    // Build SQL query for terminal workflow runs.
    let mut conditions: Vec<String> = Vec::new();
    let placeholder_list: String = statuses
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(", ");
    conditions.push(format!("status IN ({placeholder_list})"));

    let mut param_values: Vec<String> = statuses.iter().map(|s| s.to_string()).collect();

    if !workflow_name_filter.is_empty() {
        param_values.push(workflow_name_filter.to_string());
        conditions.push(format!("workflow_name = ?{}", param_values.len()));
    }

    // Query only the columns we actually need (id, workflow_name).
    let sql_id_name = format!(
        "SELECT id, workflow_name FROM workflow_runs WHERE {} ORDER BY started_at ASC",
        conditions.join(" AND ")
    );
    let items: Vec<(String, String)> = crate::db::query_collect(
        state.conn,
        &sql_id_name,
        rusqlite::params_from_iter(param_values.iter()),
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;

    Ok(items
        .into_iter()
        .filter(|(id, _)| !existing_set.contains(id))
        .map(|(id, wf_name)| ("workflow_run".to_string(), id, wf_name))
        .collect())
}

pub(super) fn filter_worktrees_by_open_pr(
    mut candidates: Vec<crate::worktree::Worktree>,
    want_open_pr: bool,
    open_prs: Vec<crate::github::GithubPr>,
) -> Vec<crate::worktree::Worktree> {
    let open_branches: HashSet<String> = open_prs.into_iter().map(|pr| pr.head_ref_name).collect();
    candidates.retain(|wt| open_branches.contains(&wt.branch) == want_open_pr);
    candidates
}

fn collect_worktree_items(
    state: &mut ExecutionState<'_>,
    node: &ForEachNode,
    existing_set: &HashSet<String>,
) -> Result<Vec<(String, String, String)>> {
    // Require a repo_id for worktree fan-outs.
    let repo_id_owned = {
        let ctx = crate::workflow::run_context::WorktreeRunContext::new(state);
        ctx.repo_id().map(String::from)
    };
    let repo_id = repo_id_owned.as_deref().ok_or_else(|| {
        ConductorError::Workflow(
            "foreach over worktrees requires a repo_id in the execution context".to_string(),
        )
    })?;

    // Extract base_branch from scope, or infer from the execution context worktree.
    let base_branch_owned: String;
    let wt_scope_opt = match &node.scope {
        Some(ForeachScope::Worktree(s)) => Some(s),
        _ => None,
    };
    let base_branch: &str = match wt_scope_opt.and_then(|s| s.base_branch.as_deref()) {
        Some(b) => b,
        None => {
            let worktree_id_owned = {
                let ctx = crate::workflow::run_context::WorktreeRunContext::new(state);
                ctx.worktree_id().map(String::from)
            };
            let wt_id = worktree_id_owned.as_deref().ok_or_else(|| {
                ConductorError::Workflow(format!(
                    "foreach '{}': over = worktrees requires either \
                     scope = {{ base_branch = \"...\" }} or a worktree_id in the execution context",
                    node.name
                ))
            })?;
            let wt = WorktreeManager::new(state.conn, state.config).get_by_id(wt_id)?;
            base_branch_owned = wt.branch.clone();
            &base_branch_owned
        }
    };

    let wt_mgr = WorktreeManager::new(state.conn, state.config);
    let active_worktrees = wt_mgr.list_by_repo_id_and_base_branch(repo_id, base_branch)?;

    let mut candidates: Vec<_> = active_worktrees
        .into_iter()
        .filter(|wt| !existing_set.contains(&wt.id))
        .collect();

    if let Some(want_open_pr) = wt_scope_opt.and_then(|s| s.has_open_pr) {
        let repo = crate::repo::RepoManager::new(state.conn, state.config).get_by_id(repo_id)?;
        let open_prs = crate::github::list_open_prs(&repo.remote_url)?;
        candidates = filter_worktrees_by_open_pr(candidates, want_open_pr, open_prs);
    }

    Ok(candidates
        .into_iter()
        .map(|wt| ("worktree".to_string(), wt.id, wt.slug))
        .collect())
}

/// Run the dispatch loop. Returns Ok(true) on stall, Ok(false) on clean finish,
/// Err on executor error.
fn run_dispatch_loop(
    state: &mut ExecutionState<'_>,
    node: &ForEachNode,
    step_id: &str,
    child_def: &crate::workflow_dsl::WorkflowDef,
    iteration: u32,
) -> Result<bool> {
    // Effective on_child_fail: default to SkipDependents for ordered tickets/worktrees.
    let ordered_dep_type =
        node.ordered && (node.over == ForeachOver::Tickets || node.over == ForeachOver::Worktrees);
    let on_child_fail = if node.on_child_fail == OnChildFail::Continue && ordered_dep_type {
        OnChildFail::SkipDependents
    } else {
        node.on_child_fail.clone()
    };

    // Load dependency edges once upfront (for ordered ticket/worktree fan-outs).
    let dep_edges: Vec<(String, String)> = if ordered_dep_type {
        if node.over == ForeachOver::Tickets {
            load_ticket_dep_edges(state, step_id)?
        } else {
            load_worktree_dep_edges(state, step_id)?
        }
    } else {
        vec![]
    };

    // Detect cycles if ordered (tickets or worktrees).
    if ordered_dep_type {
        let all_items = state.wf_mgr.get_fan_out_items(step_id, None)?;
        let item_ids: Vec<String> = all_items.iter().map(|i| i.item_id.clone()).collect();
        if let Some(cycle) = crate::graph::detect_cycles(&item_ids, &dep_edges) {
            match node.on_cycle {
                crate::workflow_dsl::OnCycle::Fail => {
                    return Err(ConductorError::Workflow(format!(
                        "foreach '{}': cycle detected: {}",
                        node.name,
                        cycle.join(" → ")
                    )));
                }
                crate::workflow_dsl::OnCycle::Warn => {
                    tracing::warn!(
                        "foreach '{}': cycle detected (continuing): {}",
                        node.name,
                        cycle.join(" → ")
                    );
                }
            }
        }
    }

    // Tracks item IDs that were observed in a non-completed terminal state on the previous cycle.
    // A failure is only committed once the same item appears failed/cancelled on two consecutive
    // cycles, guarding against transient DB-write races (#2269).
    let mut pending_terminal_failed: HashSet<String> = HashSet::new();

    loop {
        let all_items = state.wf_mgr.get_fan_out_items(step_id, None)?;
        let _pending: Vec<_> = all_items
            .iter()
            .filter(|i| i.status == "pending")
            .cloned()
            .collect();
        let running: Vec<_> = all_items
            .iter()
            .filter(|i| i.status == "running")
            .cloned()
            .collect();

        // Poll currently running child workflows for completion.
        let mut newly_failed: Vec<String> = vec![];
        for item in &running {
            if let Some(ref child_run_id) = item.child_run_id {
                match state.wf_mgr.get_workflow_run_status(child_run_id)? {
                    Some(ref s) if s == "completed" => {
                        state
                            .wf_mgr
                            .update_fan_out_item_terminal(&item.id, "completed")?;
                        state.wf_mgr.refresh_fan_out_counters(step_id)?;
                        pending_terminal_failed.remove(&item.id);

                        tracing::info!(
                            "foreach '{}': item '{}' → completed",
                            node.name,
                            item.item_ref,
                        );
                    }
                    Some(ref s) if is_terminal_status(s) => {
                        // Non-completed terminal (failed/cancelled): require two consecutive
                        // observations before committing, to avoid acting on transient DB state.
                        if pending_terminal_failed.contains(&item.id) {
                            pending_terminal_failed.remove(&item.id);
                            state
                                .wf_mgr
                                .update_fan_out_item_terminal(&item.id, "failed")?;
                            state.wf_mgr.refresh_fan_out_counters(step_id)?;

                            tracing::info!(
                                "foreach '{}': item '{}' → failed (confirmed on second observation)",
                                node.name,
                                item.item_ref,
                            );

                            newly_failed.push(item.id.clone());
                        } else {
                            pending_terminal_failed.insert(item.id.clone());
                            tracing::debug!(
                                "foreach '{}': item '{}' observed {} — deferring one cycle",
                                node.name,
                                item.item_ref,
                                s,
                            );
                        }
                    }
                    _ => {
                        // Still running or DB miss — clear any pending failure flag (status recovered).
                        pending_terminal_failed.remove(&item.id);
                    }
                }
            }
        }

        // Handle newly failed items.
        for failed_item_id in &newly_failed {
            match on_child_fail {
                OnChildFail::Halt => {
                    // Cancel all pending/running items and fail the step.
                    state.wf_mgr.cancel_fan_out_items(step_id)?;
                    state.wf_mgr.refresh_fan_out_counters(step_id)?;
                    tracing::warn!(
                        "foreach '{}': on_child_fail=halt — cancelling remaining items",
                        node.name
                    );
                    return Ok(false);
                }
                OnChildFail::SkipDependents if node.ordered => {
                    // Find transitively blocked items and skip them.
                    let dependents =
                        find_transitive_dependents(failed_item_id, &dep_edges, &all_items);
                    if !dependents.is_empty() {
                        tracing::info!(
                            "foreach '{}': skipping {} dependents of failed item",
                            node.name,
                            dependents.len()
                        );
                        state
                            .wf_mgr
                            .skip_fan_out_items_by_item_ids(step_id, &dependents)?;
                        state.wf_mgr.refresh_fan_out_counters(step_id)?;
                    }
                }
                OnChildFail::Continue | OnChildFail::SkipDependents => {
                    // Continue processing remaining items.
                }
            }
        }

        // Re-read state after updates.
        let all_items = state.wf_mgr.get_fan_out_items(step_id, None)?;
        let pending: Vec<_> = all_items
            .iter()
            .filter(|i| i.status == "pending")
            .cloned()
            .collect();
        let running: Vec<_> = all_items
            .iter()
            .filter(|i| i.status == "running")
            .cloned()
            .collect();

        // Determine eligible items to dispatch.
        let eligible: Vec<_> = if ordered_dep_type {
            // Only dispatch items whose blockers are all completed.
            pending
                .iter()
                .filter(|item| blockers_all_completed(&item.item_id, &dep_edges, &all_items))
                .cloned()
                .collect()
        } else {
            pending.clone()
        };

        if eligible.is_empty() && running.is_empty() {
            if !pending.is_empty() {
                // Stall: items remain but none are eligible and nothing is running.
                tracing::warn!(
                    "foreach '{}': stalled — {} pending items but none eligible",
                    node.name,
                    pending.len()
                );
                return Ok(true);
            }
            // All done.
            return Ok(false);
        }

        // Dispatch up to max_parallel slots.
        let slots = (node.max_parallel as usize).saturating_sub(running.len());
        for item in eligible.iter().take(slots) {
            dispatch_child_workflow(state, node, step_id, item, child_def, iteration)?;
        }

        // Sleep poll interval before next tick.
        call_between_cycle_hook();
        std::thread::sleep(state.exec_config.poll_interval);
    }
}

/// Resolve `(ticket_id, repo_id, worktree_id)` for a child dispatch based on fan-out type.
///
/// Tickets and Repos fan-outs clear `worktree_id` so each child workflow starts with
/// an independent context instead of colliding with the parent's active-run guard.
/// WorkflowRuns fan-outs pass the parent context through unchanged.
fn resolve_child_context_ids(
    over: ForeachOver,
    item_id: &str,
    parent_ticket_id: &Option<String>,
    parent_repo_id: &Option<String>,
    parent_worktree_id: &Option<String>,
) -> (Option<String>, Option<String>, Option<String>) {
    match over {
        ForeachOver::Tickets => (Some(item_id.to_string()), parent_repo_id.clone(), None),
        ForeachOver::Repos => (None, Some(item_id.to_string()), None),
        ForeachOver::WorkflowRuns => (
            parent_ticket_id.clone(),
            parent_repo_id.clone(),
            parent_worktree_id.clone(),
        ),
        ForeachOver::Worktrees => (None, parent_repo_id.clone(), Some(item_id.to_string())),
    }
}

/// Build the `WorkflowExecStandalone` params for a single fan-out item dispatch.
///
/// Extracted from `dispatch_child_workflow` so the param-construction logic can be
/// unit-tested without spawning threads or running actual workflows.
fn build_child_dispatch_params(
    state: &mut ExecutionState<'_>,
    node: &ForEachNode,
    item: &crate::workflow::manager::FanOutItemRow,
    child_def: &crate::workflow_dsl::WorkflowDef,
) -> Result<WorkflowExecStandalone> {
    let working_dir = state.worktree_ctx.working_dir.clone();
    let repo_path = state.worktree_ctx.repo_path.clone();
    let ticket_id = state.worktree_ctx.ticket_id.clone();
    let repo_id = state.worktree_ctx.repo_id.clone();
    let worktree_id = state.worktree_ctx.worktree_id.clone();
    let conductor_bin_dir = state.worktree_ctx.conductor_bin_dir.clone();
    let extra_plugin_dirs = state.worktree_ctx.extra_plugin_dirs.clone();

    // Build item-specific variable map for {{item.*}} substitution.
    let item_vars = build_item_vars(state, node, item)?;

    // Merge base workflow variables with item-specific vars.
    let base_vars = build_variable_map(state);
    let mut merged_vars: HashMap<String, String> = base_vars
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    merged_vars.extend(item_vars);

    // Build string-keyed ref map for substitute_variables_keep_literal.
    let vars_ref: HashMap<&str, String> = merged_vars
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();

    // Resolve child inputs with substitution.
    let mut child_inputs = HashMap::new();
    for (k, v) in &node.inputs {
        child_inputs.insert(
            k.clone(),
            crate::workflow::prompt_builder::substitute_variables_keep_literal(v, &vars_ref),
        );
    }
    // Apply defaults for missing optional inputs.
    for decl in &child_def.inputs {
        if !child_inputs.contains_key(&decl.name) {
            if let Some(ref default) = decl.default {
                child_inputs.insert(decl.name.clone(), default.clone());
            }
        }
    }

    // Determine target_label from item context.
    let target_label = match node.over {
        ForeachOver::Tickets => Some(item.item_ref.clone()),
        ForeachOver::Repos => Some(item.item_ref.clone()),
        ForeachOver::WorkflowRuns => Some(format!("run:{}", item.item_ref)),
        ForeachOver::Worktrees => Some(item.item_ref.clone()),
    };

    // ticket_id, repo_id, and worktree_id: pass through based on item type.
    // For ticket/repo fan-outs, clear worktree_id so each child gets its own
    // independent context instead of colliding with the parent's active run.
    let (item_ticket_id, item_repo_id, item_worktree_id) = resolve_child_context_ids(
        node.over.clone(),
        &item.item_id,
        &ticket_id,
        &repo_id,
        &worktree_id,
    );

    // For worktree fan-outs, run the child in the child worktree's directory,
    // not the parent's CWD (which may be a different worktree entirely).
    let child_working_dir = if node.over == ForeachOver::Worktrees {
        match WorktreeManager::new(state.conn, state.config).get_by_id(&item.item_id) {
            Ok(wt) => wt.path,
            Err(e) => {
                tracing::warn!(
                    "foreach: failed to look up worktree '{}', falling back to parent working dir: {e}",
                    item.item_id
                );
                working_dir.clone()
            }
        }
    } else {
        working_dir
    };

    Ok(WorkflowExecStandalone {
        config: state.config.clone(),
        workflow: child_def.clone(),
        worktree_id: item_worktree_id,
        working_dir: child_working_dir,
        repo_path,
        ticket_id: item_ticket_id,
        repo_id: item_repo_id,
        model: state.model.clone(),
        exec_config: crate::workflow::types::WorkflowExecConfig {
            poll_interval: state.exec_config.poll_interval,
            step_timeout: state.exec_config.step_timeout,
            fail_fast: state.exec_config.fail_fast,
            dry_run: state.exec_config.dry_run,
            shutdown: state.exec_config.shutdown.clone(),
        },
        inputs: child_inputs,
        target_label,
        run_id_notify: None,
        triggered_by_hook: state.triggered_by_hook,
        conductor_bin_dir: conductor_bin_dir.clone(),
        force: false,
        extra_plugin_dirs: extra_plugin_dirs.clone(),
        db_path: None,
        parent_workflow_run_id: Some(state.workflow_run_id.clone()),
    })
}

/// Dispatch one fan-out item: resolve inputs, execute child workflow in a thread,
/// and update fan_out_items to 'running'.
fn dispatch_child_workflow(
    state: &mut ExecutionState<'_>,
    node: &ForEachNode,
    step_id: &str,
    item: &crate::workflow::manager::FanOutItemRow,
    child_def: &crate::workflow_dsl::WorkflowDef,
    _iteration: u32,
) -> Result<()> {
    let params = build_child_dispatch_params(state, node, item, child_def)?;

    // Capture values needed to update DB after thread completes.
    let item_id = item.id.clone();
    let step_id_clone = step_id.to_string();
    let workflow_name = node.workflow.clone();

    // Spawn the child workflow in a background thread.
    std::thread::spawn(move || {
        match crate::workflow::engine::execute_workflow_standalone(&params) {
            Ok(result) => {
                let conn = match crate::db::open_database(&crate::config::db_path()) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("foreach dispatch thread: failed to open DB after run: {e}");
                        return;
                    }
                };
                let mgr = crate::workflow::manager::WorkflowManager::new(&conn);
                let terminal = if result.all_succeeded {
                    "completed"
                } else {
                    "failed"
                };
                if let Err(e) = mgr.update_fan_out_item_terminal(&item_id, terminal) {
                    tracing::warn!("foreach: failed to update fan_out_item terminal: {e}");
                }
                if let Err(e) = mgr.refresh_fan_out_counters(&step_id_clone) {
                    tracing::warn!("foreach: failed to refresh fan_out counters: {e}");
                }
            }
            Err(e) => {
                tracing::warn!("foreach '{}' child run failed: {e}", workflow_name);
                let conn = match crate::db::open_database(&crate::config::db_path()) {
                    Ok(c) => c,
                    Err(db_err) => {
                        tracing::warn!(
                            "foreach dispatch thread: failed to open DB for error update: {db_err}"
                        );
                        return;
                    }
                };
                let mgr = crate::workflow::manager::WorkflowManager::new(&conn);
                if let Err(update_err) = mgr.update_fan_out_item_terminal(&item_id, "failed") {
                    tracing::warn!("foreach: failed to mark item failed: {update_err}");
                }
                if let Err(update_err) = mgr.refresh_fan_out_counters(&step_id_clone) {
                    tracing::warn!("foreach: failed to refresh fan_out counters: {update_err}");
                }
            }
        }
    });

    // We don't have the child run ID yet (it's set inside the spawned thread).
    // Mark as running with a placeholder; the thread updates terminal status.
    // Use update_fan_out_item_running with a synthetic placeholder run ID.
    // Actually, since execute_workflow_standalone sets run_id_notify=None and we
    // don't await the thread, we can't get the run ID synchronously.
    // Instead, set status to 'running' without a child_run_id — the thread
    // will update terminal status when it completes.
    let now = chrono::Utc::now().to_rfc3339();
    state.conn.execute(
        "UPDATE workflow_run_step_fan_out_items \
         SET status = 'running', dispatched_at = :now \
         WHERE id = :id",
        rusqlite::named_params![":now": now, ":id": item.id],
    )?;

    tracing::info!(
        "foreach '{}': dispatched item '{}' ({})",
        node.workflow,
        item.item_ref,
        item.item_id,
    );

    Ok(())
}

/// Build item-specific variables for `{{item.*}}` substitution.
fn build_item_vars(
    state: &mut ExecutionState<'_>,
    node: &ForEachNode,
    item: &crate::workflow::manager::FanOutItemRow,
) -> Result<HashMap<String, String>> {
    let mut vars = HashMap::new();

    match node.over {
        ForeachOver::Tickets => {
            // Load ticket fields.
            let syncer = crate::tickets::TicketSyncer::new(state.conn);
            match syncer.get_by_id(&item.item_id) {
                Ok(t) => {
                    vars.insert("item.id".to_string(), t.id.clone());
                    vars.insert("item.title".to_string(), t.title.clone());
                    vars.insert("item.url".to_string(), t.url.clone());
                    vars.insert("item.source_id".to_string(), t.source_id.clone());
                    vars.insert("item.state".to_string(), t.state.clone());
                    vars.insert("item.labels".to_string(), t.labels.clone());
                }
                Err(e) => {
                    tracing::warn!("foreach: could not load ticket '{}': {e}", item.item_id);
                    vars.insert("item.id".to_string(), item.item_id.clone());
                    vars.insert("item.source_id".to_string(), item.item_ref.clone());
                }
            }
        }
        ForeachOver::Repos => {
            // Load repo fields.
            let mgr = crate::repo::RepoManager::new(state.conn, state.config);
            match mgr.get_by_id(&item.item_id) {
                Ok(r) => {
                    vars.insert("item.id".to_string(), r.id.clone());
                    vars.insert("item.slug".to_string(), r.slug.clone());
                    vars.insert("item.local_path".to_string(), r.local_path.clone());
                    vars.insert("item.remote_url".to_string(), r.remote_url.clone());
                }
                Err(e) => {
                    tracing::warn!("foreach: could not load repo '{}': {e}", item.item_id);
                    vars.insert("item.id".to_string(), item.item_id.clone());
                    vars.insert("item.slug".to_string(), item.item_ref.clone());
                }
            }
        }
        ForeachOver::WorkflowRuns => {
            vars.insert("item.id".to_string(), item.item_id.clone());
            vars.insert("item.workflow_name".to_string(), item.item_ref.clone());
        }
        ForeachOver::Worktrees => {
            let wt_mgr = WorktreeManager::new(state.conn, state.config);
            match wt_mgr.get_by_id(&item.item_id) {
                Ok(wt) => {
                    vars.insert("item.id".to_string(), wt.id);
                    vars.insert("item.slug".to_string(), wt.slug);
                    vars.insert("item.branch".to_string(), wt.branch);
                    vars.insert("item.path".to_string(), wt.path);
                    vars.insert(
                        "item.base_branch".to_string(),
                        wt.base_branch.unwrap_or_default(),
                    );
                    vars.insert(
                        "item.ticket_id".to_string(),
                        wt.ticket_id.unwrap_or_default(),
                    );
                }
                Err(e) => {
                    tracing::warn!("foreach: could not load worktree '{}': {e}", item.item_id);
                    vars.insert("item.id".to_string(), item.item_id.clone());
                    vars.insert("item.slug".to_string(), item.item_ref.clone());
                }
            }
        }
    }

    Ok(vars)
}

/// Load dependency edges (blocker_id → dependent_id) for tickets currently in the fan_out.
fn load_ticket_dep_edges(
    state: &mut ExecutionState<'_>,
    step_id: &str,
) -> Result<Vec<(String, String)>> {
    let items = state.wf_mgr.get_fan_out_items(step_id, None)?;
    let item_ids: Vec<String> = items.iter().map(|i| i.item_id.clone()).collect();
    if item_ids.is_empty() {
        return Ok(vec![]);
    }

    let syncer = crate::tickets::TicketSyncer::new(state.conn);
    let id_set: HashSet<&String> = item_ids.iter().collect();
    let ticket_id_refs: Vec<&str> = item_ids.iter().map(String::as_str).collect();
    let raw_edges = syncer
        .get_blocking_edges_for_tickets(&ticket_id_refs)
        .map_err(|e| ConductorError::Workflow(format!("foreach: dependency query failed: {e}")))?;

    let edges: Vec<(String, String)> = raw_edges
        .into_iter()
        .filter(|(blocker_id, dependent_id)| {
            id_set.contains(blocker_id) && id_set.contains(dependent_id)
        })
        .collect();
    Ok(edges)
}

/// Load dependency edges (blocker_worktree_id → dependent_worktree_id) for worktrees in the fan_out.
/// Pivots through worktree.ticket_id → ticket_dependencies.
fn load_worktree_dep_edges(
    state: &mut ExecutionState<'_>,
    step_id: &str,
) -> Result<Vec<(String, String)>> {
    let items = state.wf_mgr.get_fan_out_items(step_id, None)?;
    let item_ids: Vec<String> = items.iter().map(|i| i.item_id.clone()).collect();
    if item_ids.is_empty() {
        return Ok(vec![]);
    }

    let id_set: HashSet<&String> = item_ids.iter().collect();

    // Build worktree_id → ticket_id map for items that have a linked ticket.
    // Use WorktreeManager.get_by_ids() to avoid raw SQL against the worktrees table.
    let id_refs: Vec<&str> = item_ids.iter().map(String::as_str).collect();
    let wt_mgr = WorktreeManager::new(state.conn, state.config);
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

    // Build reverse map: ticket_id → worktree_id.
    let ticket_wt_map: HashMap<String, String> = wt_ticket_map
        .iter()
        .map(|(wt_id, tid)| (tid.clone(), wt_id.clone()))
        .collect();

    // Batch-query all 'blocks' edges for our ticket set via TicketSyncer.
    let ticket_ids: Vec<String> = wt_ticket_map.values().cloned().collect();
    if ticket_ids.is_empty() {
        return Ok(vec![]);
    }
    let ticket_id_refs: Vec<&str> = ticket_ids.iter().map(String::as_str).collect();
    let syncer = crate::tickets::TicketSyncer::new(state.conn);
    let dep_edges = syncer
        .get_blocking_edges_for_tickets(&ticket_id_refs)
        .map_err(|e| ConductorError::Workflow(format!("foreach: dependency query failed: {e}")))?;

    // Translate ticket-level edges into worktree-to-worktree edges.
    // Deduplicate with a HashSet to handle multiple worktrees sharing a ticket_id.
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

/// Check if all blockers of `item_id` are in 'completed' status.
fn blockers_all_completed(
    item_id: &str,
    edges: &[(String, String)],
    all_items: &[crate::workflow::manager::FanOutItemRow],
) -> bool {
    let status_map: HashMap<&str, &str> = all_items
        .iter()
        .map(|i| (i.item_id.as_str(), i.status.as_str()))
        .collect();

    for (blocker, dependent) in edges {
        if dependent.as_str() == item_id {
            let blocker_status = status_map
                .get(blocker.as_str())
                .copied()
                .unwrap_or("pending");
            if blocker_status != "completed" {
                return false;
            }
        }
    }
    true
}

/// Find all items that are transitively blocked by `failed_item_id`.
fn find_transitive_dependents(
    failed_item_id: &str,
    edges: &[(String, String)],
    all_items: &[crate::workflow::manager::FanOutItemRow],
) -> Vec<String> {
    // Build forward adjacency: blocker → dependents
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for (blocker, dependent) in edges {
        adj.entry(blocker.as_str())
            .or_default()
            .push(dependent.as_str());
    }

    let pending_ids: HashSet<&str> = all_items
        .iter()
        .filter(|i| i.status == "pending")
        .map(|i| i.item_id.as_str())
        .collect();

    let mut result = Vec::new();
    let mut queue = vec![failed_item_id];
    let mut seen: HashSet<&str> = HashSet::new();
    seen.insert(failed_item_id);

    while let Some(current) = queue.pop() {
        if let Some(deps) = adj.get(current) {
            for &dep in deps {
                if !seen.contains(dep) && pending_ids.contains(dep) {
                    seen.insert(dep);
                    result.push(dep.to_string());
                    queue.push(dep);
                }
            }
        }
    }

    result
}

fn is_terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled")
}

#[cfg(test)]
mod tests;
