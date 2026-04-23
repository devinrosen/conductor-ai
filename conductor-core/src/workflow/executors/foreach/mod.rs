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
    emit_event, record_step_failure, record_step_success, restore_step, should_skip, ExecutionState,
};
use crate::workflow::item_provider::ProviderContext;
use crate::workflow::prompt_builder::build_variable_map;
use crate::workflow::run_context::RunContext;
use crate::workflow::status::WorkflowStepStatus;
use crate::workflow::types::WorkflowExecStandalone;
use crate::workflow_dsl::{ForEachNode, OnChildFail};
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

    // Validate the provider early so unknown-provider errors surface before workflow I/O.
    let provider = state.registry.get(&node.over).ok_or_else(|| {
        ConductorError::Workflow(format!(
            "foreach '{}': unknown provider '{}' — no ItemProvider registered for this name",
            node.name, node.over
        ))
    })?;

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

    let repo_id_owned = {
        let ctx = crate::workflow::run_context::WorktreeRunContext::new(state);
        ctx.repo_id().map(String::from)
    };
    let worktree_id_owned = {
        let ctx = crate::workflow::run_context::WorktreeRunContext::new(state);
        ctx.worktree_id().map(String::from)
    };
    let provider_ctx = ProviderContext {
        conn: state.conn,
        config: state.config,
        repo_id: repo_id_owned.as_deref(),
        worktree_id: worktree_id_owned.as_deref(),
    };
    let provider_items = provider.items(
        &provider_ctx,
        node.scope.as_ref(),
        &node.filter,
        &existing_set,
    )?;
    let items: Vec<(String, String, String)> = provider_items
        .into_iter()
        .map(|i| (i.item_type, i.item_id, i.item_ref))
        .collect();

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
    emit_event(
        state,
        runkon_flow::events::EngineEvent::FanOutItemsCollected {
            count: total_items as usize,
        },
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
    let result = run_dispatch_loop(
        state,
        node,
        &step_id,
        &child_def,
        iteration,
        provider.as_ref(),
    );

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
        node.over,
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

#[cfg(test)]
pub(super) fn filter_worktrees_by_open_pr(
    mut candidates: Vec<crate::worktree::Worktree>,
    want_open_pr: bool,
    open_prs: Vec<crate::github::GithubPr>,
) -> Vec<crate::worktree::Worktree> {
    let open_branches: HashSet<String> = open_prs.into_iter().map(|pr| pr.head_ref_name).collect();
    candidates.retain(|wt| open_branches.contains(&wt.branch) == want_open_pr);
    candidates
}

/// Run the dispatch loop. Returns Ok(true) on stall, Ok(false) on clean finish,
/// Err on executor error.
fn run_dispatch_loop(
    state: &mut ExecutionState<'_>,
    node: &ForEachNode,
    step_id: &str,
    child_def: &crate::workflow_dsl::WorkflowDef,
    iteration: u32,
    provider: &dyn crate::workflow::item_provider::ItemProvider,
) -> Result<bool> {
    // Effective on_child_fail: default to SkipDependents for ordered tickets/worktrees.
    let ordered_dep_type = node.ordered && provider.supports_ordered();
    let on_child_fail = if node.on_child_fail == OnChildFail::Continue && ordered_dep_type {
        OnChildFail::SkipDependents
    } else {
        node.on_child_fail.clone()
    };

    // Load dependency edges once upfront (for ordered ticket/worktree fan-outs).
    let dep_edges: Vec<(String, String)> = if ordered_dep_type {
        provider.dependencies(state.conn, state.config, step_id)?
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
                        emit_event(
                            state,
                            runkon_flow::events::EngineEvent::FanOutItemCompleted {
                                item_id: item.item_id.clone(),
                                succeeded: true,
                            },
                        );
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
                            emit_event(
                                state,
                                runkon_flow::events::EngineEvent::FanOutItemCompleted {
                                    item_id: item.item_id.clone(),
                                    succeeded: false,
                                },
                            );
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
    over: &str,
    item_id: &str,
    parent_ticket_id: &Option<String>,
    parent_repo_id: &Option<String>,
    parent_worktree_id: &Option<String>,
) -> (Option<String>, Option<String>, Option<String>) {
    match over {
        "tickets" => (Some(item_id.to_string()), parent_repo_id.clone(), None),
        "repos" => (None, Some(item_id.to_string()), None),
        "workflow_runs" => (
            parent_ticket_id.clone(),
            parent_repo_id.clone(),
            parent_worktree_id.clone(),
        ),
        "worktrees" => (None, parent_repo_id.clone(), Some(item_id.to_string())),
        _ => (
            parent_ticket_id.clone(),
            parent_repo_id.clone(),
            parent_worktree_id.clone(),
        ),
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
    let conductor_bin_dir = state.worktree_ctx.conductor_bin_dir.clone();
    let extra_plugin_dirs = state.worktree_ctx.extra_plugin_dirs.clone();
    let (working_dir, repo_path, ticket_id, repo_id, worktree_id) = {
        let ctx = crate::workflow::run_context::WorktreeRunContext::new(state);
        let working_dir = ctx.working_dir_str();
        let repo_path = ctx.repo_path_str();
        let ticket_id: Option<String> = ctx.ticket_id().map(|s| s.to_string());
        let repo_id: Option<String> = ctx.repo_id().map(|s| s.to_string());
        let worktree_id: Option<String> = ctx.worktree_id().map(|s| s.to_string());
        (working_dir, repo_path, ticket_id, repo_id, worktree_id)
    };

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
    let target_label = match node.over.as_str() {
        "workflow_runs" => Some(format!("run:{}", item.item_ref)),
        _ => Some(item.item_ref.clone()),
    };

    // ticket_id, repo_id, and worktree_id: pass through based on item type.
    // For ticket/repo fan-outs, clear worktree_id so each child gets its own
    // independent context instead of colliding with the parent's active run.
    let (item_ticket_id, item_repo_id, item_worktree_id) = resolve_child_context_ids(
        &node.over,
        &item.item_id,
        &ticket_id,
        &repo_id,
        &worktree_id,
    );

    // For worktree fan-outs, run the child in the child worktree's directory,
    // not the parent's CWD (which may be a different worktree entirely).
    let child_working_dir = if node.over == "worktrees" {
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
            event_sinks: vec![],
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

    emit_event(
        state,
        runkon_flow::events::EngineEvent::FanOutItemStarted {
            item_id: item.item_id.clone(),
        },
    );
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

    match node.over.as_str() {
        "tickets" => {
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
        "repos" => {
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
        "workflow_runs" => {
            vars.insert("item.id".to_string(), item.item_id.clone());
            vars.insert("item.workflow_name".to_string(), item.item_ref.clone());
        }
        "worktrees" => {
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
        _ => {
            vars.insert("item.id".to_string(), item.item_id.clone());
            vars.insert("item.ref".to_string(), item.item_ref.clone());
        }
    }

    Ok(vars)
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

/// Test bridge: wraps run_dispatch_loop by looking up the provider from the default registry.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn run_dispatch_loop_for_test(
    state: &mut ExecutionState<'_>,
    node: &ForEachNode,
    step_id: &str,
    child_def: &crate::workflow_dsl::WorkflowDef,
    iteration: u32,
) -> Result<bool> {
    let provider = state.registry.get(&node.over).ok_or_else(|| {
        ConductorError::Workflow(format!("no provider for '{}' in test", node.over))
    })?;
    run_dispatch_loop(
        state,
        node,
        step_id,
        child_def,
        iteration,
        provider.as_ref(),
    )
}

/// Test bridge: delegates to WorktreesProvider.dependencies().
#[cfg(test)]
fn load_worktree_dep_edges(
    state: &mut ExecutionState<'_>,
    step_id: &str,
) -> Result<Vec<(String, String)>> {
    let provider = crate::workflow::item_provider::worktrees::WorktreesProvider;
    crate::workflow::item_provider::ItemProvider::dependencies(
        &provider,
        state.conn,
        state.config,
        step_id,
    )
}

/// Test bridge: delegates to TicketsProvider.dependencies().
#[cfg(test)]
fn load_ticket_dep_edges(
    state: &mut ExecutionState<'_>,
    step_id: &str,
) -> Result<Vec<(String, String)>> {
    let provider = crate::workflow::item_provider::tickets::TicketsProvider;
    crate::workflow::item_provider::ItemProvider::dependencies(
        &provider,
        state.conn,
        state.config,
        step_id,
    )
}

/// Test bridge: delegates to the TicketsProvider via the registry.
/// Kept for backward compatibility with unit tests.
#[cfg(test)]
fn collect_ticket_items(
    state: &mut ExecutionState<'_>,
    node: &ForEachNode,
    existing_set: &HashSet<String>,
) -> Result<Vec<(String, String, String)>> {
    let repo_id_owned = {
        let c = crate::workflow::run_context::WorktreeRunContext::new(state);
        c.repo_id().map(String::from)
    };
    let ctx = crate::workflow::item_provider::ProviderContext {
        conn: state.conn,
        config: state.config,
        repo_id: repo_id_owned.as_deref(),
        worktree_id: None,
    };
    let provider = crate::workflow::item_provider::tickets::TicketsProvider;
    let items = crate::workflow::item_provider::ItemProvider::items(
        &provider,
        &ctx,
        node.scope.as_ref(),
        &node.filter,
        existing_set,
    )?;
    Ok(items
        .into_iter()
        .map(|i| (i.item_type, i.item_id, i.item_ref))
        .collect())
}

/// Test bridge: delegates to the WorktreesProvider via the registry.
/// Kept for backward compatibility with unit tests.
#[cfg(test)]
fn collect_worktree_items(
    state: &mut ExecutionState<'_>,
    node: &ForEachNode,
    existing_set: &HashSet<String>,
) -> Result<Vec<(String, String, String)>> {
    let repo_id_owned = {
        let c = crate::workflow::run_context::WorktreeRunContext::new(state);
        c.repo_id().map(String::from)
    };
    let worktree_id_owned = {
        let c = crate::workflow::run_context::WorktreeRunContext::new(state);
        c.worktree_id().map(String::from)
    };
    let ctx = crate::workflow::item_provider::ProviderContext {
        conn: state.conn,
        config: state.config,
        repo_id: repo_id_owned.as_deref(),
        worktree_id: worktree_id_owned.as_deref(),
    };
    let provider = crate::workflow::item_provider::worktrees::WorktreesProvider;
    let items = crate::workflow::item_provider::ItemProvider::items(
        &provider,
        &ctx,
        node.scope.as_ref(),
        &node.filter,
        existing_set,
    )?;
    Ok(items
        .into_iter()
        .map(|i| (i.item_type, i.item_id, i.item_ref))
        .collect())
}

#[cfg(test)]
mod tests;
