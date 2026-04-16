use crate::error::{ConductorError, Result};
use crate::workflow_dsl::CallWorkflowNode;

use crate::workflow::engine::{
    fetch_child_completion_data, record_step_failure, record_step_success, resolve_child_inputs,
    restore_step, run_on_fail_agent, should_skip, ExecutionState,
};
use crate::workflow::prompt_builder::build_variable_map;
use crate::workflow::status::WorkflowStepStatus;

pub fn execute_call_workflow(
    state: &mut ExecutionState<'_>,
    node: &CallWorkflowNode,
    iteration: u32,
) -> Result<()> {
    let pos = state.position;
    state.position += 1;

    // Skip completed sub-workflow steps on resume
    let wf_step_name = format!("workflow:{}", node.workflow);
    if should_skip(state, &wf_step_name, iteration) {
        tracing::info!("Skipping completed sub-workflow '{}'", node.workflow);
        restore_step(state, &wf_step_name, iteration);
        return Ok(());
    }

    let child_depth = state.depth + 1;
    if child_depth > crate::workflow_dsl::MAX_WORKFLOW_DEPTH {
        let msg = format!(
            "Workflow nesting depth exceeds maximum of {}: parent '{}' calling '{}'",
            crate::workflow_dsl::MAX_WORKFLOW_DEPTH,
            state.workflow_name,
            node.workflow,
        );
        state.all_succeeded = false;
        if state.exec_config.fail_fast {
            return Err(ConductorError::Workflow(msg));
        }
        tracing::error!("{msg}");
        return Ok(());
    }

    let step_key = node.workflow.clone();

    // Load the child workflow definition once (it won't change between retries)
    let child_def = crate::workflow_dsl::load_workflow_by_name(
        &state.working_dir,
        &state.repo_path,
        &node.workflow,
    )
    .map_err(|e| {
        ConductorError::Workflow(format!(
            "Failed to load sub-workflow '{}': {e}",
            node.workflow
        ))
    })?;

    // Retry loop
    let max_attempts = 1 + node.retries;
    let mut last_error = String::new();

    // Pre-loop: if a prior child run exists in a resumable state, try resuming it
    // before starting fresh. This preserves already-completed steps inside the child.
    // The resume attempt does not count against max_attempts.
    if let Some(prior_child) = state
        .wf_mgr
        .find_resumable_child_run(&state.workflow_run_id, &node.workflow)?
    {
        let step_id = state.wf_mgr.insert_step(
            &state.workflow_run_id,
            &wf_step_name,
            "workflow",
            false,
            pos,
            iteration as i64,
        )?;

        state.wf_mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Running,
            Some(&prior_child.id),
            None,
            None,
            None,
            Some(0),
        )?;

        tracing::info!(
            "Step 'workflow:{}': resuming prior child run '{}'",
            node.workflow,
            prior_child.id,
        );

        let resume_input = crate::workflow::types::WorkflowResumeInput {
            conn: state.conn,
            config: state.config,
            workflow_run_id: &prior_child.id,
            model: state.model.as_deref(),
            from_step: None,
            restart: false,
            conductor_bin_dir: state.conductor_bin_dir.clone(),
        };

        match crate::workflow::engine::resume_workflow(&resume_input) {
            Ok(result) if result.all_succeeded => {
                tracing::info!(
                    "Sub-workflow '{}' resumed and completed: cost=${:.4}, {} turns",
                    node.workflow,
                    result.total_cost,
                    result.total_turns,
                );

                let ((markers, context), child_steps) =
                    fetch_child_completion_data(&state.wf_mgr, &result.workflow_run_id);

                let markers_json = serde_json::to_string(&markers).unwrap_or_default();

                state.wf_mgr.update_step_status(
                    &step_id,
                    WorkflowStepStatus::Completed,
                    Some(result.workflow_run_id.as_str()),
                    Some(&format!("Sub-workflow '{}' completed", node.workflow)),
                    Some(&context),
                    Some(&markers_json),
                    Some(0),
                )?;

                record_step_success(
                    state,
                    step_key.clone(),
                    &node.workflow,
                    Some(format!(
                        "Sub-workflow '{}' completed successfully",
                        node.workflow
                    )),
                    Some(result.total_cost),
                    Some(result.total_turns),
                    Some(result.total_duration_ms),
                    Some(result.total_input_tokens),
                    Some(result.total_output_tokens),
                    Some(result.total_cache_read_input_tokens),
                    Some(result.total_cache_creation_input_tokens),
                    markers,
                    context,
                    Some(result.workflow_run_id.clone()),
                    iteration,
                    None,
                    None,
                );

                for (key, value) in child_steps {
                    state.step_results.insert(key, value);
                }

                return Ok(());
            }
            Ok(result) => {
                let msg = format!("Sub-workflow '{}' failed (resume)", node.workflow);
                tracing::warn!(
                    "'{}': resume of child run '{}' did not succeed (all_succeeded=false)",
                    node.workflow,
                    result.workflow_run_id,
                );
                state.wf_mgr.update_step_status(
                    &step_id,
                    WorkflowStepStatus::Failed,
                    Some(result.workflow_run_id.as_str()),
                    Some(&msg),
                    None,
                    None,
                    Some(0),
                )?;
                last_error = msg;
                // Fall through to the retry loop
            }
            Err(e) => {
                let msg = format!("Sub-workflow '{}' resume error: {e}", node.workflow);
                tracing::warn!(
                    "'{}': error resuming child run '{}': {e}",
                    node.workflow,
                    prior_child.id,
                );
                state.wf_mgr.update_step_status(
                    &step_id,
                    WorkflowStepStatus::Failed,
                    Some(&prior_child.id),
                    Some(&msg),
                    None,
                    None,
                    Some(0),
                )?;
                last_error = msg;
                // Fall through to the retry loop
            }
        }
    }

    for attempt in 0..max_attempts {
        let step_id = state.wf_mgr.insert_step(
            &state.workflow_run_id,
            &wf_step_name,
            "workflow",
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
            Some(attempt as i64),
        )?;

        tracing::info!(
            "Step 'workflow:{}' (attempt {}/{}): executing sub-workflow",
            node.workflow,
            attempt + 1,
            max_attempts,
        );

        // Resolve child inputs: substitute variables, apply defaults, check required
        let vars = build_variable_map(state);
        let child_inputs = match resolve_child_inputs(&node.inputs, &vars, &child_def.inputs) {
            Ok(inputs) => inputs,
            Err(missing) => {
                let msg = format!(
                    "Sub-workflow '{}' requires input '{}' but it was not provided",
                    node.workflow, missing,
                );
                tracing::warn!("{msg}");
                state.wf_mgr.update_step_status(
                    &step_id,
                    WorkflowStepStatus::Failed,
                    None,
                    Some(&msg),
                    None,
                    None,
                    Some(attempt as i64),
                )?;
                last_error = msg;
                continue;
            }
        };

        // Execute the child workflow
        let child_input = crate::workflow::types::WorkflowExecInput {
            conn: state.conn,
            config: state.config,
            workflow: &child_def,
            worktree_id: state.worktree_id.as_deref(),
            working_dir: &state.working_dir,
            repo_path: &state.repo_path,
            ticket_id: state.ticket_id.as_deref(),
            repo_id: state.repo_id.as_deref(),
            model: state.model.as_deref(),
            exec_config: &state.exec_config,
            inputs: child_inputs,
            depth: child_depth,
            parent_workflow_run_id: Some(&state.workflow_run_id),
            target_label: state.target_label.as_deref(),
            default_bot_name: node
                .bot_name
                .clone()
                .or_else(|| state.default_bot_name.clone()),
            feature_id: state.feature_id.as_deref(),
            iteration,
            run_id_notify: None,
            triggered_by_hook: state.triggered_by_hook,
            conductor_bin_dir: state.conductor_bin_dir.clone(),
            force: false,
            extra_plugin_dirs: state.extra_plugin_dirs.clone(),
        };

        match crate::workflow::engine::execute_workflow(&child_input) {
            Ok(result) => {
                if result.all_succeeded {
                    tracing::info!(
                        "Sub-workflow '{}' completed: cost=${:.4}, {} turns",
                        node.workflow,
                        result.total_cost,
                        result.total_turns,
                    );

                    // Bubble up the child's final step output (markers + context) and
                    // all completed step results in a single DB query.
                    let ((markers, context), child_steps) =
                        fetch_child_completion_data(&state.wf_mgr, &result.workflow_run_id);

                    let markers_json = serde_json::to_string(&markers).unwrap_or_default();

                    state.wf_mgr.update_step_status(
                        &step_id,
                        WorkflowStepStatus::Completed,
                        Some(result.workflow_run_id.as_str()),
                        Some(&format!("Sub-workflow '{}' completed", node.workflow)),
                        Some(&context),
                        Some(&markers_json),
                        Some(attempt as i64),
                    )?;

                    record_step_success(
                        state,
                        step_key.clone(),
                        &node.workflow,
                        Some(format!(
                            "Sub-workflow '{}' completed successfully",
                            node.workflow
                        )),
                        Some(result.total_cost),
                        Some(result.total_turns),
                        Some(result.total_duration_ms),
                        Some(result.total_input_tokens),
                        Some(result.total_output_tokens),
                        Some(result.total_cache_read_input_tokens),
                        Some(result.total_cache_creation_input_tokens),
                        markers,
                        context,
                        Some(result.workflow_run_id.clone()),
                        iteration,
                        None,
                        None,
                    );

                    // Bubble up child step results so parent can reference internal
                    // sub-workflow markers (e.g. review-aggregator.has_review_issues).
                    for (key, value) in child_steps {
                        state.step_results.insert(key, value);
                    }

                    return Ok(());
                } else {
                    let msg = format!("Sub-workflow '{}' failed", node.workflow);
                    tracing::warn!("{} (attempt {}/{})", msg, attempt + 1, max_attempts,);
                    state.wf_mgr.update_step_status(
                        &step_id,
                        WorkflowStepStatus::Failed,
                        Some(result.workflow_run_id.as_str()),
                        Some(&msg),
                        None,
                        None,
                        Some(attempt as i64),
                    )?;
                    last_error = msg;
                    continue;
                }
            }
            Err(e) => {
                let msg = format!("Sub-workflow '{}' error: {e}", node.workflow);
                tracing::warn!("{} (attempt {}/{})", msg, attempt + 1, max_attempts,);
                state.wf_mgr.update_step_status(
                    &step_id,
                    WorkflowStepStatus::Failed,
                    None,
                    Some(&msg),
                    None,
                    None,
                    Some(attempt as i64),
                )?;
                last_error = msg;
                continue;
            }
        }
    }

    // All retries exhausted — run on_fail agent if specified
    if let Some(ref on_fail_agent) = node.on_fail {
        run_on_fail_agent(
            state,
            &node.workflow,
            on_fail_agent,
            &last_error,
            node.retries,
            iteration,
        );
    }

    record_step_failure(
        state,
        step_key,
        &node.workflow,
        last_error,
        max_attempts,
        true,
    )
}
