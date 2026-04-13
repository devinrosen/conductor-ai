use std::collections::{HashMap, HashSet};

use crate::error::{ConductorError, Result};
use crate::workflow::engine::{
    record_step_failure, record_step_success, restore_step, should_skip, ExecutionState,
};
use crate::workflow::prompt_builder::build_variable_map;
use crate::workflow::status::WorkflowStepStatus;
use crate::workflow::types::WorkflowExecStandalone;
use crate::workflow_dsl::{ForEachNode, ForeachOver, OnChildFail};

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

        return record_step_failure(state, step_key, &node.name, error_msg, 1);
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
        Some(crate::workflow_dsl::TicketScope::TicketId(ticket_id)) => {
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
        Some(crate::workflow_dsl::TicketScope::Label(label)) => {
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
        Some(crate::workflow_dsl::TicketScope::Unlabeled) => {
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
        None => {
            // No scope specified — collect all open tickets for the repo.
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

/// Run the dispatch loop. Returns Ok(true) on stall, Ok(false) on clean finish,
/// Err on executor error.
fn run_dispatch_loop(
    state: &mut ExecutionState<'_>,
    node: &ForEachNode,
    step_id: &str,
    child_def: &crate::workflow_dsl::WorkflowDef,
    iteration: u32,
) -> Result<bool> {
    // Effective on_child_fail: default to SkipDependents for ordered tickets.
    let on_child_fail = if node.on_child_fail == OnChildFail::Continue
        && node.over == ForeachOver::Tickets
        && node.ordered
    {
        OnChildFail::SkipDependents
    } else {
        node.on_child_fail.clone()
    };

    // Load dependency edges once upfront (for ordered ticket fan-outs).
    let dep_edges: Vec<(String, String)> = if node.ordered && node.over == ForeachOver::Tickets {
        load_ticket_dep_edges(state, step_id)?
    } else {
        vec![]
    };

    // Detect cycles if ordered.
    if node.ordered && node.over == ForeachOver::Tickets {
        let all_items = state.wf_mgr.get_fan_out_items(step_id, None)?;
        let item_ids: Vec<String> = all_items.iter().map(|i| i.item_id.clone()).collect();
        if let Some(cycle) = detect_ticket_cycles(&item_ids, &dep_edges) {
            match node.on_cycle {
                crate::workflow_dsl::OnCycle::Fail => {
                    return Err(ConductorError::Workflow(format!(
                        "foreach '{}': ticket cycle detected: {}",
                        node.name,
                        cycle.join(" → ")
                    )));
                }
                crate::workflow_dsl::OnCycle::Warn => {
                    tracing::warn!(
                        "foreach '{}': ticket cycle detected (continuing): {}",
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
        let eligible: Vec<_> = if node.ordered && node.over == ForeachOver::Tickets {
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
    }
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

    let params = WorkflowExecStandalone {
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
    };

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
    use crate::workflow_dsl::{ForeachOver, OnChildFail, OnCycle, TicketScope};

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
            scope: Some(TicketScope::Unlabeled),
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
}
