use std::collections::{HashMap, HashSet};

use crate::error::{ConductorError, Result};
use crate::workflow::engine::{
    record_step_failure, record_step_success, restore_step, should_skip, ExecutionState,
};
use crate::workflow::prompt_builder::build_variable_map;
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

    // Insert the step record.
    let step_id = state.wf_mgr.insert_step(
        &state.workflow_run_id,
        &step_key,
        "foreach",
        false,
        pos,
        iteration as i64,
    )?;

    state.wf_mgr.update_step_status(
        &step_id,
        WorkflowStepStatus::Running,
        None,
        None,
        None,
        None,
        Some(0),
    )?;

    // Load the child workflow definition (needed for input resolution).
    let child_def = crate::workflow_dsl::load_workflow_by_name(
        &state.working_dir,
        &state.repo_path,
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
    let repo_id = state.repo_id.as_deref().ok_or_else(|| {
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

fn collect_worktree_items(
    state: &mut ExecutionState<'_>,
    node: &ForEachNode,
    existing_set: &HashSet<String>,
) -> Result<Vec<(String, String, String)>> {
    // Require a repo_id for worktree fan-outs.
    let repo_id = state.repo_id.as_deref().ok_or_else(|| {
        ConductorError::Workflow(
            "foreach over worktrees requires a repo_id in the execution context".to_string(),
        )
    })?;

    // Extract base_branch from scope.
    let base_branch = match &node.scope {
        Some(ForeachScope::Worktree(wt_scope)) => &wt_scope.base_branch,
        _ => {
            return Err(ConductorError::Workflow(format!(
                "foreach '{}': over = worktrees requires scope = {{ base_branch = \"...\" }}",
                node.name
            )));
        }
    };

    let wt_mgr = WorktreeManager::new(state.conn, state.config);
    let active_worktrees = wt_mgr.list_by_repo_id_and_base_branch(repo_id, base_branch)?;

    Ok(active_worktrees
        .into_iter()
        .filter(|wt| !existing_set.contains(&wt.id))
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
        if let Some(cycle) = detect_ticket_cycles(&item_ids, &dep_edges) {
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
                    Some(ref s) if is_terminal_status(s) => {
                        let item_succeeded = s == "completed";
                        let terminal_status = if item_succeeded {
                            "completed"
                        } else {
                            "failed"
                        };
                        state
                            .wf_mgr
                            .update_fan_out_item_terminal(&item.id, terminal_status)?;
                        state.wf_mgr.refresh_fan_out_counters(step_id)?;

                        tracing::info!(
                            "foreach '{}': item '{}' → {terminal_status}",
                            node.name,
                            item.item_ref,
                        );

                        if !item_succeeded {
                            newly_failed.push(item.id.clone());
                        }
                    }
                    _ => {
                        // Still running or DB miss — continue polling.
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
        &state.ticket_id,
        &state.repo_id,
        &state.worktree_id,
    );

    Ok(WorkflowExecStandalone {
        config: state.config.clone(),
        workflow: child_def.clone(),
        worktree_id: item_worktree_id,
        working_dir: state.working_dir.clone(),
        repo_path: state.repo_path.clone(),
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
        feature_id: state.feature_id.clone(),
        run_id_notify: None,
        triggered_by_hook: state.triggered_by_hook,
        conductor_bin_dir: state.conductor_bin_dir.clone(),
        force: false,
        extra_plugin_dirs: state.extra_plugin_dirs.clone(),
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
         SET status = 'running', dispatched_at = ?1 \
         WHERE id = ?2",
        rusqlite::params![now, item.id],
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
    let mut edges: Vec<(String, String)> = Vec::new();

    // For each ticket in the set, load its dependencies and collect edges
    // where both endpoints are in the set.
    let id_set: HashSet<&String> = item_ids.iter().collect();
    for ticket_id in &item_ids {
        match syncer.get_dependencies(ticket_id) {
            Ok(deps) => {
                for blocker in &deps.blocked_by {
                    if id_set.contains(&blocker.id) {
                        // blocker.id → ticket_id (blocker must complete before ticket)
                        edges.push((blocker.id.clone(), ticket_id.clone()));
                    }
                }
            }
            Err(e) => {
                tracing::warn!("foreach: could not load deps for ticket '{ticket_id}': {e}");
            }
        }
    }

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
            tracing::warn!("foreach: could not fetch worktrees for dep edges: {e}; skipping ordering");
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
    for (dependent_ticket_id, blocker_ticket_id) in dep_edges {
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

/// DFS cycle detection on the dependency graph.
/// Returns Some(cycle_path) if a cycle is found, None otherwise.
fn detect_ticket_cycles(item_ids: &[String], edges: &[(String, String)]) -> Option<Vec<String>> {
    // Build adjacency list: ticket_id → Vec<dependent_ids>
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for id in item_ids {
        adj.entry(id.as_str()).or_default();
    }
    for (blocker, dependent) in edges {
        adj.entry(blocker.as_str())
            .or_default()
            .push(dependent.as_str());
    }

    let mut visited: HashSet<&str> = HashSet::new();
    let mut stack: HashSet<&str> = HashSet::new();
    let mut path: Vec<&str> = Vec::new();

    for id in item_ids {
        if !visited.contains(id.as_str()) {
            if let Some(cycle) = dfs_cycle(id.as_str(), &adj, &mut visited, &mut stack, &mut path) {
                return Some(cycle.into_iter().map(str::to_string).collect());
            }
        }
    }

    None
}

fn dfs_cycle<'a>(
    node: &'a str,
    adj: &HashMap<&'a str, Vec<&'a str>>,
    visited: &mut HashSet<&'a str>,
    stack: &mut HashSet<&'a str>,
    path: &mut Vec<&'a str>,
) -> Option<Vec<&'a str>> {
    visited.insert(node);
    stack.insert(node);
    path.push(node);

    if let Some(neighbors) = adj.get(node) {
        for &neighbor in neighbors {
            if !visited.contains(neighbor) {
                if let Some(cycle) = dfs_cycle(neighbor, adj, visited, stack, path) {
                    return Some(cycle);
                }
            } else if stack.contains(neighbor) {
                // Found a back-edge: cycle starts at neighbor
                let cycle_start = path.iter().position(|&n| n == neighbor).unwrap_or(0);
                let mut cycle: Vec<&'a str> = path[cycle_start..].to_vec();
                cycle.push(neighbor); // close the cycle
                return Some(cycle);
            }
        }
    }

    stack.remove(node);
    path.pop();
    None
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
mod tests {
    use super::*;
    use crate::tickets::{TicketInput, TicketLabelInput, TicketSyncer};
    use crate::workflow_dsl::{ForeachOver, ForeachScope, OnChildFail, OnCycle, TicketScope};

    fn setup_db() -> rusqlite::Connection {
        crate::test_helpers::setup_db()
    }

    fn make_ticket(source_id: &str, title: &str) -> TicketInput {
        TicketInput {
            source_type: "github".to_string(),
            source_id: source_id.to_string(),
            title: title.to_string(),
            body: String::new(),
            state: "open".to_string(),
            labels: vec![],
            assignee: None,
            priority: None,
            url: String::new(),
            raw_json: None,
            label_details: vec![],
            blocked_by: vec![],
            children: vec![],
            parent: None,
        }
    }

    fn make_foreach_node_unlabeled() -> ForEachNode {
        ForEachNode {
            name: "test-foreach".to_string(),
            over: ForeachOver::Tickets,
            scope: Some(ForeachScope::Ticket(TicketScope::Unlabeled)),
            filter: std::collections::HashMap::new(),
            ordered: false,
            on_cycle: OnCycle::Fail,
            max_parallel: 3,
            workflow: "label-ticket".to_string(),
            inputs: std::collections::HashMap::new(),
            on_child_fail: OnChildFail::Continue,
        }
    }

    #[test]
    fn test_collect_ticket_items_unlabeled_scope() {
        let conn = setup_db();
        let config: &'static crate::config::Config =
            Box::leak(Box::new(crate::config::Config::default()));

        // setup_db() inserts repo "r1" / worktree "w1" — reuse them.
        let syncer = TicketSyncer::new(&conn);

        // t1 is labeled, t2 and t3 are unlabeled.
        let mut t1 = make_ticket("1", "Labeled issue");
        t1.label_details = vec![TicketLabelInput {
            name: "bug".to_string(),
            color: None,
        }];
        let t2 = make_ticket("2", "Unlabeled A");
        let t3 = make_ticket("3", "Unlabeled B");
        syncer.upsert_tickets("r1", &[t1, t2, t3]).unwrap();

        // Build a minimal ExecutionState.
        let agent_mgr = crate::agent::AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        let mut state = ExecutionState {
            conn: &conn,
            config,
            workflow_run_id: run.id,
            workflow_name: "test".to_string(),
            worktree_id: Some("w1".to_string()),
            working_dir: "/tmp/test".to_string(),
            worktree_slug: "test".to_string(),
            repo_path: "/tmp/repo".to_string(),
            ticket_id: None,
            repo_id: Some("r1".to_string()),
            model: None,
            exec_config: crate::workflow::types::WorkflowExecConfig::default(),
            inputs: std::collections::HashMap::new(),
            agent_mgr: crate::agent::AgentManager::new(&conn),
            wf_mgr: crate::workflow::manager::WorkflowManager::new(&conn),
            parent_run_id: parent.id,
            depth: 0,
            target_label: None,
            step_results: std::collections::HashMap::new(),
            contexts: Vec::new(),
            position: 0,
            all_succeeded: true,
            total_cost: 0.0,
            total_turns: 0,
            total_duration_ms: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_input_tokens: 0,
            total_cache_creation_input_tokens: 0,
            last_gate_feedback: None,
            block_output: None,
            block_with: Vec::new(),
            resume_ctx: None,
            default_bot_name: None,
            feature_id: None,
            triggered_by_hook: false,
            conductor_bin_dir: None,
            extra_plugin_dirs: vec![],
            last_heartbeat_at: std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0)),
        };

        let node = make_foreach_node_unlabeled();
        let existing_set = HashSet::new();
        let items = collect_ticket_items(&mut state, &node, &existing_set).unwrap();

        // Should only return t2 and t3 (unlabeled).
        assert_eq!(items.len(), 2);
        let source_refs: Vec<&str> = items.iter().map(|(_, _, r)| r.as_str()).collect();
        assert!(source_refs.contains(&"2"), "source_id 2 should be included");
        assert!(source_refs.contains(&"3"), "source_id 3 should be included");
        // item_type should be "ticket"
        for (item_type, _, _) in &items {
            assert_eq!(item_type, "ticket");
        }
    }

    // Regression tests for #2094: worktree_id must be cleared for Tickets/Repos fan-outs
    // so that child dispatches do not inherit the parent's active-workflow guard.

    #[test]
    fn test_resolve_child_context_ids_tickets_clears_worktree_id() {
        let (ticket_id, repo_id, worktree_id) = resolve_child_context_ids(
            ForeachOver::Tickets,
            "ticket-abc",
            &None,
            &Some("repo-1".to_string()),
            &Some("worktree-parent".to_string()),
        );
        assert_eq!(ticket_id, Some("ticket-abc".to_string()));
        assert_eq!(repo_id, Some("repo-1".to_string()));
        assert!(
            worktree_id.is_none(),
            "Tickets fan-out must clear worktree_id"
        );
    }

    #[test]
    fn test_resolve_child_context_ids_repos_clears_worktree_id() {
        let (ticket_id, repo_id, worktree_id) = resolve_child_context_ids(
            ForeachOver::Repos,
            "repo-xyz",
            &Some("ticket-parent".to_string()),
            &None,
            &Some("worktree-parent".to_string()),
        );
        assert!(ticket_id.is_none(), "Repos fan-out must clear ticket_id");
        assert_eq!(repo_id, Some("repo-xyz".to_string()));
        assert!(
            worktree_id.is_none(),
            "Repos fan-out must clear worktree_id"
        );
    }

    #[test]
    fn test_resolve_child_context_ids_workflow_runs_passes_through_worktree_id() {
        let (ticket_id, repo_id, worktree_id) = resolve_child_context_ids(
            ForeachOver::WorkflowRuns,
            "run-999",
            &Some("ticket-parent".to_string()),
            &Some("repo-parent".to_string()),
            &Some("worktree-parent".to_string()),
        );
        assert_eq!(ticket_id, Some("ticket-parent".to_string()));
        assert_eq!(repo_id, Some("repo-parent".to_string()));
        assert_eq!(
            worktree_id,
            Some("worktree-parent".to_string()),
            "WorkflowRuns fan-out must pass worktree_id through"
        );
    }

    #[test]
    fn test_resolve_child_context_ids_workflow_runs_none_worktree_passthrough() {
        let (_, _, worktree_id) =
            resolve_child_context_ids(ForeachOver::WorkflowRuns, "run-000", &None, &None, &None);
        assert!(worktree_id.is_none());
    }

    // Regression tests for #2097: verify that build_child_dispatch_params wires
    // worktree_id correctly — the clearing logic in resolve_child_context_ids must
    // actually reach the WorkflowExecStandalone struct.

    fn make_minimal_item(
        item_id: &str,
        item_ref: &str,
        item_type: &str,
    ) -> crate::workflow::manager::FanOutItemRow {
        crate::workflow::manager::FanOutItemRow {
            id: "item-id".to_string(),
            step_run_id: "step-run-id".to_string(),
            item_type: item_type.to_string(),
            item_id: item_id.to_string(),
            item_ref: item_ref.to_string(),
            child_run_id: None,
            status: "pending".to_string(),
            dispatched_at: None,
            completed_at: None,
        }
    }

    fn make_minimal_child_def() -> crate::workflow_dsl::WorkflowDef {
        crate::workflow_dsl::WorkflowDef {
            name: "child-workflow".to_string(),
            title: None,
            description: String::new(),
            trigger: crate::workflow_dsl::WorkflowTrigger::Manual,
            targets: vec![],
            group: None,
            inputs: vec![],
            body: vec![],
            always: vec![],
            source_path: String::new(),
        }
    }

    fn make_foreach_node_for(over: ForeachOver) -> ForEachNode {
        ForEachNode {
            name: "test-foreach".to_string(),
            over,
            scope: None,
            filter: std::collections::HashMap::new(),
            ordered: false,
            on_cycle: crate::workflow_dsl::OnCycle::Fail,
            max_parallel: 3,
            workflow: "child-workflow".to_string(),
            inputs: std::collections::HashMap::new(),
            on_child_fail: crate::workflow_dsl::OnChildFail::Continue,
        }
    }

    fn make_execution_state_with_worktree<'a>(
        conn: &'a rusqlite::Connection,
        config: &'static crate::config::Config,
        workflow_run_id: String,
        parent_run_id: String,
        worktree_id: Option<String>,
        repo_id: Option<String>,
        ticket_id: Option<String>,
    ) -> ExecutionState<'a> {
        ExecutionState {
            conn,
            config,
            workflow_run_id,
            workflow_name: "test".to_string(),
            worktree_id,
            working_dir: "/tmp/test".to_string(),
            worktree_slug: "test".to_string(),
            repo_path: "/tmp/repo".to_string(),
            ticket_id,
            repo_id,
            model: None,
            exec_config: crate::workflow::types::WorkflowExecConfig::default(),
            inputs: std::collections::HashMap::new(),
            agent_mgr: crate::agent::AgentManager::new(conn),
            wf_mgr: crate::workflow::manager::WorkflowManager::new(conn),
            parent_run_id,
            depth: 0,
            target_label: None,
            step_results: std::collections::HashMap::new(),
            contexts: Vec::new(),
            position: 0,
            all_succeeded: true,
            total_cost: 0.0,
            total_turns: 0,
            total_duration_ms: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_input_tokens: 0,
            total_cache_creation_input_tokens: 0,
            last_gate_feedback: None,
            block_output: None,
            block_with: Vec::new(),
            resume_ctx: None,
            default_bot_name: None,
            feature_id: None,
            triggered_by_hook: false,
            conductor_bin_dir: None,
            extra_plugin_dirs: vec![],
            last_heartbeat_at: std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0)),
        }
    }

    #[test]
    fn test_dispatch_params_tickets_clears_worktree_id() {
        let conn = setup_db();
        let config: &'static crate::config::Config =
            Box::leak(Box::new(crate::config::Config::default()));
        let agent_mgr = crate::agent::AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        let mut state = make_execution_state_with_worktree(
            &conn,
            config,
            run.id,
            parent.id,
            Some("w1".to_string()),
            Some("r1".to_string()),
            None,
        );

        let node = make_foreach_node_for(ForeachOver::Tickets);
        let item = make_minimal_item("ticket-abc", "42", "ticket");
        let child_def = make_minimal_child_def();

        let params = build_child_dispatch_params(&mut state, &node, &item, &child_def).unwrap();
        assert!(
            params.worktree_id.is_none(),
            "Tickets fan-out must clear worktree_id in dispatch params"
        );
    }

    #[test]
    fn test_dispatch_params_repos_clears_worktree_id() {
        let conn = setup_db();
        let config: &'static crate::config::Config =
            Box::leak(Box::new(crate::config::Config::default()));
        let agent_mgr = crate::agent::AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        let mut state = make_execution_state_with_worktree(
            &conn,
            config,
            run.id,
            parent.id,
            Some("w1".to_string()),
            None,
            None,
        );

        let node = make_foreach_node_for(ForeachOver::Repos);
        let item = make_minimal_item("repo-xyz", "my-repo", "repo");
        let child_def = make_minimal_child_def();

        let params = build_child_dispatch_params(&mut state, &node, &item, &child_def).unwrap();
        assert!(
            params.worktree_id.is_none(),
            "Repos fan-out must clear worktree_id in dispatch params"
        );
    }

    #[test]
    fn test_dispatch_params_workflow_runs_passes_worktree_id_through() {
        let conn = setup_db();
        let config: &'static crate::config::Config =
            Box::leak(Box::new(crate::config::Config::default()));
        let agent_mgr = crate::agent::AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        let mut state = make_execution_state_with_worktree(
            &conn,
            config,
            run.id,
            parent.id,
            Some("w1".to_string()),
            Some("r1".to_string()),
            None,
        );

        let node = make_foreach_node_for(ForeachOver::WorkflowRuns);
        let item = make_minimal_item("run-999", "some-workflow", "workflow_run");
        let child_def = make_minimal_child_def();

        let params = build_child_dispatch_params(&mut state, &node, &item, &child_def).unwrap();
        assert_eq!(
            params.worktree_id,
            Some("w1".to_string()),
            "WorkflowRuns fan-out must pass worktree_id through in dispatch params"
        );
    }

    #[test]
    fn test_resolve_child_context_ids_worktrees_sets_worktree_id() {
        let (ticket_id, repo_id, worktree_id) = resolve_child_context_ids(
            ForeachOver::Worktrees,
            "worktree-abc",
            &Some("ticket-parent".to_string()),
            &Some("repo-1".to_string()),
            &Some("old-worktree".to_string()),
        );
        assert!(
            ticket_id.is_none(),
            "Worktrees fan-out must clear ticket_id"
        );
        assert_eq!(repo_id, Some("repo-1".to_string()));
        assert_eq!(
            worktree_id,
            Some("worktree-abc".to_string()),
            "Worktrees fan-out must set worktree_id to the item_id"
        );
    }

    #[test]
    fn test_collect_worktree_items_matching_base_branch() {
        let conn = setup_db();
        let config: &'static crate::config::Config =
            Box::leak(Box::new(crate::config::Config::default()));

        // Insert two active worktrees with matching base_branch.
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, base_branch) \
             VALUES ('wt-a', 'r1', 'feat-a', 'feat/a', '/tmp/a', 'active', '2024-01-01T00:00:00Z', 'release/1.0')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, base_branch) \
             VALUES ('wt-b', 'r1', 'feat-b', 'feat/b', '/tmp/b', 'active', '2024-01-02T00:00:00Z', 'release/1.0')",
            [],
        ).unwrap();
        // Insert one with different base_branch — should be excluded.
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, base_branch) \
             VALUES ('wt-c', 'r1', 'feat-c', 'feat/c', '/tmp/c', 'active', '2024-01-03T00:00:00Z', 'main')",
            [],
        ).unwrap();
        // Insert one with matching base_branch but non-active status — should be excluded.
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, base_branch) \
             VALUES ('wt-d', 'r1', 'feat-d', 'feat/d', '/tmp/d', 'abandoned', '2024-01-04T00:00:00Z', 'release/1.0')",
            [],
        ).unwrap();

        let agent_mgr = crate::agent::AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        let mut state = make_execution_state_with_worktree(
            &conn,
            config,
            run.id,
            parent.id,
            Some("w1".to_string()),
            Some("r1".to_string()),
            None,
        );

        let node = ForEachNode {
            name: "test-wt-foreach".to_string(),
            over: ForeachOver::Worktrees,
            scope: Some(ForeachScope::Worktree(crate::workflow_dsl::WorktreeScope {
                base_branch: "release/1.0".to_string(),
            })),
            filter: std::collections::HashMap::new(),
            ordered: false,
            on_cycle: OnCycle::Fail,
            max_parallel: 2,
            workflow: "child".to_string(),
            inputs: std::collections::HashMap::new(),
            on_child_fail: OnChildFail::Continue,
        };

        let existing_set = HashSet::new();
        let items = collect_worktree_items(&mut state, &node, &existing_set).unwrap();

        assert_eq!(
            items.len(),
            2,
            "should find only the 2 active worktrees on release/1.0 (wt-c excluded by base_branch, wt-d excluded by non-active status)"
        );
        let ids: Vec<&str> = items.iter().map(|(_, id, _)| id.as_str()).collect();
        assert!(ids.contains(&"wt-a"));
        assert!(ids.contains(&"wt-b"));
        assert!(!ids.contains(&"wt-d"), "completed worktree must not appear");
        for (item_type, _, _) in &items {
            assert_eq!(item_type, "worktree");
        }
    }

    #[test]
    fn test_build_item_vars_worktrees_all_fields() {
        let conn = setup_db();
        let config: &'static crate::config::Config =
            Box::leak(Box::new(crate::config::Config::default()));

        // Insert a worktree with all fields populated.
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, base_branch) \
             VALUES ('wt-x', 'r1', 'feat-x', 'feat/x', '/tmp/x', 'active', '2024-01-01T00:00:00Z', 'release/2.0')",
            [],
        ).unwrap();

        let agent_mgr = crate::agent::AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        let mut state = make_execution_state_with_worktree(
            &conn,
            config,
            run.id,
            parent.id,
            Some("w1".to_string()),
            Some("r1".to_string()),
            None,
        );

        let node = make_foreach_node_for(ForeachOver::Worktrees);
        let item = make_minimal_item("wt-x", "feat-x", "worktree");

        let vars = build_item_vars(&mut state, &node, &item).unwrap();
        assert_eq!(vars.get("item.slug").map(|s| s.as_str()), Some("feat-x"));
        assert_eq!(vars.get("item.branch").map(|s| s.as_str()), Some("feat/x"));
        assert_eq!(vars.get("item.path").map(|s| s.as_str()), Some("/tmp/x"));
        assert_eq!(
            vars.get("item.base_branch").map(|s| s.as_str()),
            Some("release/2.0")
        );
        assert_eq!(vars.get("item.ticket_id").map(|s| s.as_str()), Some(""));
        assert_eq!(vars.get("item.id").map(|s| s.as_str()), Some("wt-x"));
    }

    // -----------------------------------------------------------------------
    // load_worktree_dep_edges tests
    // -----------------------------------------------------------------------

    /// Seed two worktrees each linked to a ticket, where ticket-1 blocks ticket-2.
    /// Verify that load_worktree_dep_edges returns a single edge (wt-blocker → wt-blocked).
    #[test]
    fn test_load_worktree_dep_edges_basic() {
        let conn = setup_db();
        let config: &'static crate::config::Config =
            Box::leak(Box::new(crate::config::Config::default()));

        // Insert two tickets where ticket "1" blocks ticket "2".
        let t1 = make_ticket("1", "Blocker");
        let mut t2 = make_ticket("2", "Blocked");
        t2.blocked_by = vec!["1".to_string()];
        let syncer = TicketSyncer::new(&conn);
        syncer.upsert_tickets("r1", &[t1, t2]).unwrap();

        // Resolve ULIDs (tickets table uses server-generated ULIDs).
        let ticket1_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let ticket2_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '2'", [], |row| {
                row.get(0)
            })
            .unwrap();

        // Insert two worktrees linked to the tickets.
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, ticket_id) \
             VALUES ('wt-1', 'r1', 'feat-1', 'feat/1', '/tmp/1', 'active', '2024-01-01T00:00:00Z', ?1)",
            rusqlite::params![ticket1_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, ticket_id) \
             VALUES ('wt-2', 'r1', 'feat-2', 'feat/2', '/tmp/2', 'active', '2024-01-02T00:00:00Z', ?1)",
            rusqlite::params![ticket2_id],
        )
        .unwrap();

        // Create a workflow run + step + fan_out_items for the two worktrees.
        let agent_mgr = crate::agent::AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        let step_id = wf_mgr
            .insert_step(&run.id, "release-foreach", "foreach", false, 0, 0)
            .unwrap();

        wf_mgr
            .insert_fan_out_item(&step_id, "worktree", "wt-1", "feat-1")
            .unwrap();
        wf_mgr
            .insert_fan_out_item(&step_id, "worktree", "wt-2", "feat-2")
            .unwrap();

        let mut state = make_execution_state_with_worktree(
            &conn,
            config,
            run.id,
            parent.id,
            Some("w1".to_string()),
            Some("r1".to_string()),
            None,
        );

        let edges = load_worktree_dep_edges(&mut state, &step_id).unwrap();

        assert_eq!(edges.len(), 1, "expected exactly one dependency edge");
        assert_eq!(
            edges[0],
            ("wt-1".to_string(), "wt-2".to_string()),
            "wt-1 (blocker) should point to wt-2 (blocked)"
        );
    }

    /// When no worktrees have a linked ticket, load_worktree_dep_edges returns no edges.
    #[test]
    fn test_load_worktree_dep_edges_no_tickets() {
        let conn = setup_db();
        let config: &'static crate::config::Config =
            Box::leak(Box::new(crate::config::Config::default()));

        // Insert worktrees with no ticket_id.
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt-a', 'r1', 'feat-a', 'feat/a', '/tmp/a', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt-b', 'r1', 'feat-b', 'feat/b', '/tmp/b', 'active', '2024-01-02T00:00:00Z')",
            [],
        )
        .unwrap();

        let agent_mgr = crate::agent::AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        let step_id = wf_mgr
            .insert_step(&run.id, "release-foreach", "foreach", false, 0, 0)
            .unwrap();

        wf_mgr
            .insert_fan_out_item(&step_id, "worktree", "wt-a", "feat-a")
            .unwrap();
        wf_mgr
            .insert_fan_out_item(&step_id, "worktree", "wt-b", "feat-b")
            .unwrap();

        let mut state = make_execution_state_with_worktree(
            &conn,
            config,
            run.id,
            parent.id,
            Some("w1".to_string()),
            Some("r1".to_string()),
            None,
        );

        let edges = load_worktree_dep_edges(&mut state, &step_id).unwrap();
        assert!(
            edges.is_empty(),
            "expected no edges when worktrees have no linked tickets"
        );
    }

    /// Mixed-case: wt-1 linked to ticket (which has a blocker in the set), wt-2 linked to
    /// ticket (the blocker), wt-3 has no ticket. Expect one edge (wt-2 → wt-1); wt-3 ignored.
    #[test]
    fn test_load_worktree_dep_edges_mixed_some_with_tickets() {
        let conn = setup_db();
        let config: &'static crate::config::Config =
            Box::leak(Box::new(crate::config::Config::default()));

        // ticket "blocker" blocks ticket "dep"
        let t_blocker = make_ticket("blocker", "Blocker ticket");
        let mut t_dep = make_ticket("dep", "Dependent ticket");
        t_dep.blocked_by = vec!["blocker".to_string()];
        let syncer = TicketSyncer::new(&conn);
        syncer.upsert_tickets("r1", &[t_blocker, t_dep]).unwrap();

        let blocker_id: String = conn
            .query_row(
                "SELECT id FROM tickets WHERE source_id = 'blocker'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let dep_id: String = conn
            .query_row(
                "SELECT id FROM tickets WHERE source_id = 'dep'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        // wt-1 linked to "dep" ticket, wt-2 linked to "blocker" ticket, wt-3 no ticket
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, ticket_id) \
             VALUES ('wt-1', 'r1', 'feat-dep', 'feat/dep', '/tmp/dep', 'active', '2024-01-01T00:00:00Z', ?1)",
            rusqlite::params![dep_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, ticket_id) \
             VALUES ('wt-2', 'r1', 'feat-blocker', 'feat/blocker', '/tmp/blocker', 'active', '2024-01-02T00:00:00Z', ?1)",
            rusqlite::params![blocker_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt-3', 'r1', 'feat-no-ticket', 'feat/no-ticket', '/tmp/no-ticket', 'active', '2024-01-03T00:00:00Z')",
            [],
        )
        .unwrap();

        let agent_mgr = crate::agent::AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        let step_id = wf_mgr
            .insert_step(&run.id, "mixed-foreach", "foreach", false, 0, 0)
            .unwrap();

        wf_mgr
            .insert_fan_out_item(&step_id, "worktree", "wt-1", "feat-dep")
            .unwrap();
        wf_mgr
            .insert_fan_out_item(&step_id, "worktree", "wt-2", "feat-blocker")
            .unwrap();
        wf_mgr
            .insert_fan_out_item(&step_id, "worktree", "wt-3", "feat-no-ticket")
            .unwrap();

        let mut state = make_execution_state_with_worktree(
            &conn,
            config,
            run.id,
            parent.id,
            Some("w1".to_string()),
            Some("r1".to_string()),
            None,
        );

        let edges = load_worktree_dep_edges(&mut state, &step_id).unwrap();

        assert_eq!(edges.len(), 1, "expected one edge: wt-2 blocks wt-1");
        assert_eq!(
            edges[0],
            ("wt-2".to_string(), "wt-1".to_string()),
            "wt-2 (blocker) should point to wt-1 (dependent)"
        );
    }

    /// collect_worktree_items returns an error when repo_id is missing from the execution state.
    #[test]
    fn test_collect_worktree_items_no_repo_id_returns_error() {
        let conn = setup_db();
        let config: &'static crate::config::Config =
            Box::leak(Box::new(crate::config::Config::default()));

        let agent_mgr = crate::agent::AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        // repo_id = None — collect_worktree_items must error
        let mut state = make_execution_state_with_worktree(
            &conn,
            config,
            run.id,
            parent.id,
            Some("w1".to_string()),
            None,
            None,
        );

        let node = ForEachNode {
            name: "wt-foreach".to_string(),
            over: ForeachOver::Worktrees,
            scope: Some(ForeachScope::Worktree(crate::workflow_dsl::WorktreeScope {
                base_branch: "release/1.0".to_string(),
            })),
            filter: std::collections::HashMap::new(),
            ordered: false,
            on_cycle: OnCycle::Fail,
            max_parallel: 2,
            workflow: "child".to_string(),
            inputs: std::collections::HashMap::new(),
            on_child_fail: OnChildFail::Continue,
        };

        let result = collect_worktree_items(&mut state, &node, &HashSet::new());
        assert!(result.is_err(), "expected error when repo_id is missing");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("repo_id"),
            "error should mention repo_id, got: {msg}"
        );
    }

    /// collect_worktree_items returns an error when scope is not a Worktree scope.
    #[test]
    fn test_collect_worktree_items_wrong_scope_returns_error() {
        let conn = setup_db();
        let config: &'static crate::config::Config =
            Box::leak(Box::new(crate::config::Config::default()));

        let agent_mgr = crate::agent::AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        let mut state = make_execution_state_with_worktree(
            &conn,
            config,
            run.id,
            parent.id,
            Some("w1".to_string()),
            Some("r1".to_string()),
            None,
        );

        // Ticket scope instead of worktree scope — should error
        let node = ForEachNode {
            name: "bad-scope".to_string(),
            over: ForeachOver::Worktrees,
            scope: Some(ForeachScope::Ticket(
                crate::workflow_dsl::TicketScope::Unlabeled,
            )),
            filter: std::collections::HashMap::new(),
            ordered: false,
            on_cycle: OnCycle::Fail,
            max_parallel: 2,
            workflow: "child".to_string(),
            inputs: std::collections::HashMap::new(),
            on_child_fail: OnChildFail::Continue,
        };

        let result = collect_worktree_items(&mut state, &node, &HashSet::new());
        assert!(result.is_err(), "expected error when scope type is wrong");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("base_branch"),
            "error should mention base_branch, got: {msg}"
        );
    }

    /// build_item_vars for a worktree with NULL base_branch and NULL ticket_id should
    /// populate those fields with empty strings rather than panicking or erroring.
    #[test]
    fn test_build_item_vars_worktrees_null_optional_fields() {
        let conn = setup_db();
        let config: &'static crate::config::Config =
            Box::leak(Box::new(crate::config::Config::default()));

        // Insert a worktree with no base_branch and no ticket_id (both NULL).
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt-null', 'r1', 'feat-null', 'feat/null', '/tmp/null', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let agent_mgr = crate::agent::AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        let mut state = make_execution_state_with_worktree(
            &conn,
            config,
            run.id,
            parent.id,
            Some("w1".to_string()),
            Some("r1".to_string()),
            None,
        );

        let node = make_foreach_node_for(ForeachOver::Worktrees);
        let item = make_minimal_item("wt-null", "feat-null", "worktree");

        let vars = build_item_vars(&mut state, &node, &item).unwrap();
        assert_eq!(vars.get("item.slug").map(|s| s.as_str()), Some("feat-null"));
        assert_eq!(
            vars.get("item.branch").map(|s| s.as_str()),
            Some("feat/null")
        );
        assert_eq!(vars.get("item.path").map(|s| s.as_str()), Some("/tmp/null"));
        assert_eq!(
            vars.get("item.base_branch").map(|s| s.as_str()),
            Some(""),
            "NULL base_branch should become empty string"
        );
        assert_eq!(
            vars.get("item.ticket_id").map(|s| s.as_str()),
            Some(""),
            "NULL ticket_id should become empty string"
        );
    }

    /// build_item_vars for a worktree with a linked ticket_id should populate
    /// vars["item.ticket_id"] with the ticket ULID.
    #[test]
    fn test_build_item_vars_worktrees_with_ticket_id() {
        let conn = setup_db();
        let config: &'static crate::config::Config =
            Box::leak(Box::new(crate::config::Config::default()));

        // setup_db() inserts repo "r1" — upsert a ticket against it to get a valid ULID.
        let syncer = TicketSyncer::new(&conn);
        syncer
            .upsert_tickets("r1", &[make_ticket("t-linked-1", "Linked ticket")])
            .unwrap();
        let ticket_id: String = conn
            .query_row(
                "SELECT id FROM tickets WHERE source_id = 't-linked-1' AND repo_id = 'r1'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        // Insert a worktree linked to the real ticket.
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, base_branch, ticket_id) \
             VALUES ('wt-linked', 'r1', 'feat-linked', 'feat/linked', '/tmp/linked', 'active', '2024-01-01T00:00:00Z', 'release/1.0', ?1)",
            rusqlite::params![ticket_id],
        )
        .unwrap();

        let agent_mgr = crate::agent::AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        let mut state = make_execution_state_with_worktree(
            &conn,
            config,
            run.id,
            parent.id,
            Some("w1".to_string()),
            Some("r1".to_string()),
            None,
        );

        let node = make_foreach_node_for(ForeachOver::Worktrees);
        let item = make_minimal_item("wt-linked", "feat-linked", "worktree");

        let vars = build_item_vars(&mut state, &node, &item).unwrap();
        assert_eq!(
            vars.get("item.ticket_id").map(|s| s.as_str()),
            Some(ticket_id.as_str()),
            "ticket_id should be populated from the linked ticket"
        );
        assert_eq!(
            vars.get("item.base_branch").map(|s| s.as_str()),
            Some("release/1.0")
        );
        assert_eq!(
            vars.get("item.slug").map(|s| s.as_str()),
            Some("feat-linked")
        );
    }
}
