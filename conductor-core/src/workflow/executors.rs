use std::collections::HashSet;
use std::process::Command;
use std::thread;
use std::time::Duration;

use crate::agent::AgentRunStatus;
use crate::agent_config::AgentSpec;
use crate::error::{ConductorError, Result};
use crate::workflow_dsl::{
    ApprovalMode, CallNode, CallWorkflowNode, Condition, DoNode, DoWhileNode, GateNode, GateType,
    IfNode, OnFailAction, OnTimeout, ParallelNode, ScriptNode, UnlessNode, WhileNode,
};

use super::engine::{
    check_max_iterations, check_stuck, execute_nodes, fetch_child_completion_data,
    record_step_failure, record_step_success, resolve_child_inputs, resolve_schema, restore_step,
    run_on_fail_agent, should_skip, ExecutionState,
};
use super::helpers::{find_max_completed_while_iteration, sanitize_tmux_name};
use super::output::{interpret_agent_output, parse_conductor_output};
use super::prompt_builder::{build_agent_prompt, build_variable_map, substitute_variables};
use super::status::{WorkflowRunStatus, WorkflowStepStatus};
use super::types::ContextEntry;

pub(super) fn execute_call(
    state: &mut ExecutionState<'_>,
    node: &CallNode,
    iteration: u32,
) -> Result<()> {
    // Call-level output overrides block-level; if neither is set, use None.
    // We must clone into a local because execute_call_with_schema takes &mut state.
    let effective_output: Option<String> = match (&node.output, &state.block_output) {
        (Some(o), _) => Some(o.clone()),
        (None, Some(b)) => Some(b.clone()),
        (None, None) => None,
    };
    // Block-level `with` snippets prepended to call-level `with`.
    // Only allocate a new Vec when both sources are non-empty; when only one
    // source has entries, clone it into a local so we don't hold a borrow on
    // state across the mutable call to execute_call_with_schema.
    let effective_with: Vec<String> = if state.block_with.is_empty() {
        node.with.clone()
    } else if node.with.is_empty() {
        state.block_with.clone()
    } else {
        state
            .block_with
            .iter()
            .chain(node.with.iter())
            .cloned()
            .collect()
    };
    execute_call_with_schema(
        state,
        node,
        iteration,
        effective_output.as_deref(),
        &effective_with,
    )
}

/// Inner implementation of execute_call that accepts an optional schema override
/// and prompt snippet references.
///
/// The `schema_override` parameter allows parallel blocks to pass their block-level
/// output schema to individual calls. The `with_refs` parameter provides prompt
/// snippet names to load and append to the agent prompt.
fn execute_call_with_schema(
    state: &mut ExecutionState<'_>,
    node: &CallNode,
    iteration: u32,
    schema_name: Option<&str>,
    with_refs: &[String],
) -> Result<()> {
    let pos = state.position;
    state.position += 1;

    let step_key_check = node.agent.step_key();
    if should_skip(state, &step_key_check, iteration) {
        tracing::info!(
            "Skipping completed step '{}' (iteration {})",
            step_key_check,
            iteration
        );
        restore_step(state, &step_key_check, iteration);
        return Ok(());
    }

    // Load agent definition
    let agent_def = crate::agent_config::load_agent(
        &state.working_dir,
        &state.repo_path,
        &AgentSpec::from(&node.agent),
        Some(&state.workflow_name),
    )?;
    let agent_label = node.agent.label();
    let step_key = node.agent.step_key();

    // Load output schema if specified
    let schema = schema_name
        .map(|name| resolve_schema(state, name))
        .transpose()?;

    // Load and concatenate prompt snippets
    let snippet_text = crate::prompt_config::load_and_concat_snippets(
        &state.working_dir,
        &state.repo_path,
        with_refs,
        Some(&state.workflow_name),
    )?;

    let prompt = build_agent_prompt(state, &agent_def, schema.as_ref(), &snippet_text);
    let step_model = agent_def.model.as_deref().or(state.model.as_deref());

    // Retry loop
    let max_attempts = 1 + node.retries;
    let mut last_error = String::new();

    for attempt in 0..max_attempts {
        let step_id = state.wf_mgr.insert_step(
            &state.workflow_run_id,
            agent_label,
            &agent_def.role.to_string(),
            agent_def.can_commit,
            pos,
            iteration as i64,
        )?;

        let window_prefix = if state.worktree_slug.is_empty() {
            state
                .workflow_run_id
                .get(..8)
                .unwrap_or(&state.workflow_run_id)
        } else {
            state.worktree_slug.as_str()
        };
        let child_window = sanitize_tmux_name(&format!("{}-wf-{}", window_prefix, agent_label));
        let effective_bot_name = node
            .bot_name
            .as_deref()
            .or(state.default_bot_name.as_deref());
        let child_run = state.agent_mgr.create_child_run(
            state.worktree_id.as_deref(),
            &prompt,
            Some(&child_window),
            step_model,
            &state.parent_run_id,
            effective_bot_name,
        )?;

        state.wf_mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Running,
            Some(&child_run.id),
            None,
            None,
            None,
            Some(attempt as i64),
        )?;

        tracing::info!(
            "Step '{}' (attempt {}/{}): spawning in '{}'",
            agent_label,
            attempt + 1,
            max_attempts,
            child_window,
        );

        // Spawn in tmux
        if let Err(e) = crate::agent_runtime::spawn_child_tmux(
            &child_run.id,
            &state.working_dir,
            &prompt,
            step_model,
            &child_window,
            effective_bot_name,
        ) {
            tracing::warn!("Failed to spawn child: {e}");
            let _ = state
                .agent_mgr
                .update_run_failed(&child_run.id, &format!("spawn failed: {e}"));
            state.wf_mgr.update_step_status(
                &step_id,
                WorkflowStepStatus::Failed,
                Some(&child_run.id),
                Some(&format!("spawn failed: {e}")),
                None,
                None,
                Some(attempt as i64),
            )?;
            last_error = format!("spawn failed: {e}");
            continue;
        }

        // Poll for completion
        match crate::agent_runtime::poll_child_completion(
            state.conn,
            &child_run.id,
            state.exec_config.poll_interval,
            state.exec_config.step_timeout,
            state.exec_config.shutdown.as_ref(),
        ) {
            Ok(completed_run) => {
                let succeeded = completed_run.status == AgentRunStatus::Completed;

                // Parse output: structured (schema) or generic (markers + context)
                let (markers, context, structured_json) = match interpret_agent_output(
                    completed_run.result_text.as_deref(),
                    schema.as_ref(),
                    succeeded,
                ) {
                    Ok(result) => result,
                    Err(validation_err) => {
                        tracing::warn!(
                            "Step '{}' structured output validation failed: {validation_err}",
                            agent_label,
                        );
                        state.wf_mgr.update_step_status(
                            &step_id,
                            WorkflowStepStatus::Failed,
                            Some(&completed_run.id),
                            completed_run.result_text.as_deref(),
                            None,
                            None,
                            Some(attempt as i64),
                        )?;
                        last_error = validation_err;
                        continue;
                    }
                };

                let markers_json = serde_json::to_string(&markers).unwrap_or_default();

                if succeeded {
                    tracing::info!(
                        "Step '{}' completed: cost=${:.4}, {} turns, markers={:?}",
                        agent_label,
                        completed_run.cost_usd.unwrap_or(0.0),
                        completed_run.num_turns.unwrap_or(0),
                        markers,
                    );

                    state.wf_mgr.update_step_status_full(
                        &step_id,
                        WorkflowStepStatus::Completed,
                        Some(&completed_run.id),
                        completed_run.result_text.as_deref(),
                        Some(&context),
                        Some(&markers_json),
                        Some(attempt as i64),
                        structured_json.as_deref(),
                    )?;

                    record_step_success(
                        state,
                        step_key.clone(),
                        agent_label,
                        completed_run.result_text,
                        completed_run.cost_usd,
                        completed_run.num_turns,
                        completed_run.duration_ms,
                        markers,
                        context,
                        Some(completed_run.id),
                        iteration,
                        structured_json,
                        None,
                    );

                    return Ok(());
                } else {
                    tracing::warn!(
                        "Step '{}' failed (attempt {}/{}): {}",
                        agent_label,
                        attempt + 1,
                        max_attempts,
                        completed_run
                            .result_text
                            .as_deref()
                            .unwrap_or("unknown error"),
                    );

                    state.wf_mgr.update_step_status(
                        &step_id,
                        WorkflowStepStatus::Failed,
                        Some(&completed_run.id),
                        completed_run.result_text.as_deref(),
                        Some(&context),
                        Some(&markers_json),
                        Some(attempt as i64),
                    )?;

                    last_error = completed_run
                        .result_text
                        .unwrap_or_else(|| "unknown error".to_string());
                    continue;
                }
            }
            Err(e) => {
                tracing::warn!("Step '{}' poll error: {e}", agent_label);
                let _ = state.agent_mgr.update_run_cancelled(&child_run.id);
                let cancel_msg = e.to_string();
                state.wf_mgr.update_step_status(
                    &step_id,
                    WorkflowStepStatus::Failed,
                    Some(&child_run.id),
                    Some(&cancel_msg),
                    None,
                    None,
                    Some(attempt as i64),
                )?;
                if matches!(e, crate::agent_runtime::PollError::Shutdown) {
                    return Err(ConductorError::Workflow(cancel_msg));
                }
                last_error = cancel_msg;
                continue;
            }
        }
    }

    // All retries exhausted — run on_fail agent if specified
    if let Some(ref on_fail_agent) = node.on_fail {
        run_on_fail_agent(
            state,
            agent_label,
            on_fail_agent,
            &last_error,
            node.retries,
            iteration,
        );
    }

    record_step_failure(state, step_key, agent_label, last_error, max_attempts)
}

pub(super) fn execute_call_workflow(
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
            None,
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

        let resume_input = super::types::WorkflowResumeInput {
            conn: state.conn,
            config: state.config,
            workflow_run_id: &prior_child.id,
            model: state.model.as_deref(),
            from_step: None,
            restart: false,
            conductor_bin_dir: state.conductor_bin_dir.clone(),
        };

        match super::engine::resume_workflow(&resume_input) {
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
                    None,
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
                    None,
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
                    None,
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
        let child_input = super::types::WorkflowExecInput {
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
        };

        match super::engine::execute_workflow(&child_input) {
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
                        None,
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

    record_step_failure(state, step_key, &node.workflow, last_error, max_attempts)
}

fn eval_condition(state: &ExecutionState<'_>, condition: &Condition) -> bool {
    match condition {
        Condition::StepMarker { step, marker } => state
            .step_results
            .get(step)
            .map(|r| r.markers.iter().any(|m| m == marker))
            .unwrap_or(false),
        Condition::BoolInput { input } => state
            .inputs
            .get(input)
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false),
    }
}

pub(super) fn execute_if(state: &mut ExecutionState<'_>, node: &IfNode) -> Result<()> {
    let condition_met = eval_condition(state, &node.condition);

    if condition_met {
        tracing::info!(condition = ?node.condition, "if — condition met, executing body");
        execute_nodes(state, &node.body)?;
    } else {
        tracing::info!(condition = ?node.condition, "if — condition not met, skipping");
    }

    Ok(())
}

pub(super) fn execute_unless(state: &mut ExecutionState<'_>, node: &UnlessNode) -> Result<()> {
    let condition_met = eval_condition(state, &node.condition);

    if !condition_met {
        tracing::info!(condition = ?node.condition, "unless — condition not met, executing body");
        execute_nodes(state, &node.body)?;
    } else {
        tracing::info!(condition = ?node.condition, "unless — condition met, skipping");
    }

    Ok(())
}

pub(super) fn execute_while(state: &mut ExecutionState<'_>, node: &WhileNode) -> Result<()> {
    // On resume, determine the last completed iteration so we can fast-forward
    let start_iteration = if state.resume_ctx.is_some() {
        find_max_completed_while_iteration(state, node)
    } else {
        0u32
    };
    let mut iteration = start_iteration;
    let mut prev_marker_sets: Vec<HashSet<String>> = Vec::new();

    loop {
        // Check condition
        let has_marker = state
            .step_results
            .get(&node.step)
            .map(|r| r.markers.iter().any(|m| m == &node.marker))
            .unwrap_or(false);

        if !has_marker {
            tracing::info!(
                "while {}.{} — condition no longer met after {} iterations",
                node.step,
                node.marker,
                iteration
            );
            break;
        }

        if check_max_iterations(
            state,
            iteration,
            node.max_iterations,
            &node.on_max_iter,
            &node.step,
            &node.marker,
            "while",
        )? {
            break;
        }

        tracing::info!(
            "while {}.{} — iteration {}/{}",
            node.step,
            node.marker,
            iteration + 1,
            node.max_iterations
        );

        // Execute body
        for body_node in &node.body {
            super::engine::execute_single_node(state, body_node, iteration)?;

            if !state.all_succeeded && state.exec_config.fail_fast {
                return Ok(());
            }
        }

        // Stuck detection
        if let Some(stuck_after) = node.stuck_after {
            check_stuck(
                state,
                &mut prev_marker_sets,
                &node.step,
                &node.marker,
                stuck_after,
                "while",
            )?;
        }

        iteration += 1;
    }

    Ok(())
}

pub(super) fn execute_do_while(state: &mut ExecutionState<'_>, node: &DoWhileNode) -> Result<()> {
    let mut iteration = 0u32;
    let mut prev_marker_sets: Vec<HashSet<String>> = Vec::new();

    loop {
        if check_max_iterations(
            state,
            iteration,
            node.max_iterations,
            &node.on_max_iter,
            &node.step,
            &node.marker,
            "do",
        )? {
            break;
        }

        tracing::info!(
            "do {}.{} — iteration {}/{}",
            node.step,
            node.marker,
            iteration + 1,
            node.max_iterations
        );

        // Execute body first (do-while: body always runs before condition check)
        for body_node in &node.body {
            super::engine::execute_single_node(state, body_node, iteration)?;

            if !state.all_succeeded && state.exec_config.fail_fast {
                return Ok(());
            }
        }

        // Check condition after body
        let has_marker = state
            .step_results
            .get(&node.step)
            .map(|r| r.markers.iter().any(|m| m == &node.marker))
            .unwrap_or(false);

        // Stuck detection
        if let Some(stuck_after) = node.stuck_after {
            check_stuck(
                state,
                &mut prev_marker_sets,
                &node.step,
                &node.marker,
                stuck_after,
                "do",
            )?;
        }

        if !has_marker {
            tracing::info!(
                "do {}.{} — condition no longer met after {} iterations",
                node.step,
                node.marker,
                iteration + 1
            );
            break;
        }

        iteration += 1;
    }

    Ok(())
}

pub(super) fn execute_do(state: &mut ExecutionState<'_>, node: &DoNode) -> Result<()> {
    tracing::info!(
        "do block: executing {} body nodes sequentially",
        node.body.len()
    );

    // Save and apply block-level output/with so nested calls can inherit them
    let saved_output = state.block_output.clone();
    let saved_with = state.block_with.clone();

    if node.output.is_some() {
        state.block_output = node.output.clone();
    }
    if !node.with.is_empty() {
        // Prepend block's with to any outer block_with already in state
        let mut combined = node.with.clone();
        combined.extend(saved_with.iter().cloned());
        state.block_with = combined;
    }

    for body_node in &node.body {
        if let Err(e) = super::engine::execute_single_node(state, body_node, 0) {
            // Restore block-level context before propagating so that
            // always-blocks and subsequent nodes don't inherit do-block state.
            state.block_output = saved_output;
            state.block_with = saved_with;
            return Err(e);
        }
        if !state.all_succeeded && state.exec_config.fail_fast {
            break;
        }
    }

    // Restore block-level context
    state.block_output = saved_output;
    state.block_with = saved_with;

    Ok(())
}

pub(super) fn execute_parallel(
    state: &mut ExecutionState<'_>,
    node: &ParallelNode,
    iteration: u32,
) -> Result<()> {
    let group_id = crate::new_id();
    let pos_base = state.position;

    tracing::info!(
        "parallel: spawning {} agents (fail_fast={}, min_success={:?})",
        node.calls.len(),
        node.fail_fast,
        node.min_success,
    );

    // Load block-level schema (if any)
    let block_schema = node
        .output
        .as_deref()
        .map(|name| resolve_schema(state, name))
        .transpose()?;

    // Spawn all agents
    struct ParallelChild {
        agent_name: String,
        child_run_id: String,
        step_id: String,
        window_name: String,
        /// Resolved schema for this child (computed at spawn time).
        schema: Option<crate::schema_config::OutputSchema>,
    }

    let mut children = Vec::new();
    let mut skipped_count = 0u32;

    for (i, agent_ref) in node.calls.iter().enumerate() {
        let pos = pos_base + i as i64;
        state.position = pos + 1;
        let agent_label = agent_ref.label();

        // Skip completed agents on resume
        let agent_step_key = agent_ref.step_key();
        if should_skip(state, &agent_step_key, iteration) {
            tracing::info!("parallel: skipping completed agent '{}'", agent_label);
            restore_step(state, &agent_step_key, iteration);
            skipped_count += 1;
            continue;
        }

        let agent_def = crate::agent_config::load_agent(
            &state.working_dir,
            &state.repo_path,
            &AgentSpec::from(agent_ref),
            Some(&state.workflow_name),
        )?;

        // Check per-call `if` condition: skip this call unless the named prior step
        // emitted the named marker. The step is recorded as Skipped in the DB so
        // it is visible in run-show and TUI, but contributes no markers or context.
        if let Some((cond_step, cond_marker)) = node.call_if.get(&i.to_string()) {
            let has_marker = state
                .step_results
                .get(cond_step)
                .map(|r| r.markers.iter().any(|m| m == cond_marker))
                .unwrap_or(false);
            if !has_marker {
                tracing::info!(
                    "parallel: skipping '{}' (if={}.{} not satisfied)",
                    agent_label,
                    cond_step,
                    cond_marker
                );
                let step_id = state.wf_mgr.insert_step(
                    &state.workflow_run_id,
                    agent_label,
                    &agent_def.role.to_string(),
                    agent_def.can_commit,
                    pos,
                    iteration as i64,
                )?;
                state.wf_mgr.set_step_parallel_group(&step_id, &group_id)?;
                state.wf_mgr.update_step_status(
                    &step_id,
                    WorkflowStepStatus::Skipped,
                    None,
                    Some(&format!("skipped: {cond_step}.{cond_marker} not emitted")),
                    None,
                    None,
                    None,
                )?;
                skipped_count += 1;
                continue;
            }
        }

        // Determine schema for this call: per-call override > block-level
        let call_schema = node
            .call_outputs
            .get(&i.to_string())
            .map(|name| resolve_schema(state, name))
            .transpose()?;
        let effective_schema = call_schema.as_ref().or(block_schema.as_ref());

        // Combine block-level `with` + per-call `with` additions
        let mut effective_with = node.with.clone();
        if let Some(extra) = node.call_with.get(&i.to_string()) {
            effective_with.extend(extra.iter().cloned());
        }

        let snippet_text = crate::prompt_config::load_and_concat_snippets(
            &state.working_dir,
            &state.repo_path,
            &effective_with,
            Some(&state.workflow_name),
        )?;

        let prompt = build_agent_prompt(state, &agent_def, effective_schema, &snippet_text);
        let step_model = agent_def.model.as_deref().or(state.model.as_deref());
        let step_id = state.wf_mgr.insert_step(
            &state.workflow_run_id,
            agent_label,
            &agent_def.role.to_string(),
            agent_def.can_commit,
            pos,
            iteration as i64,
        )?;
        state.wf_mgr.set_step_parallel_group(&step_id, &group_id)?;

        let window_prefix = if state.worktree_slug.is_empty() {
            state
                .workflow_run_id
                .get(..8)
                .unwrap_or(&state.workflow_run_id)
        } else {
            state.worktree_slug.as_str()
        };
        let window_name =
            sanitize_tmux_name(&format!("{}-wf-{}-{}", window_prefix, agent_label, i));
        let child_run = state.agent_mgr.create_child_run(
            state.worktree_id.as_deref(),
            &prompt,
            Some(&window_name),
            step_model,
            &state.parent_run_id,
            state.default_bot_name.as_deref(),
        )?;

        state.wf_mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Running,
            Some(&child_run.id),
            None,
            None,
            None,
            None,
        )?;

        if let Err(e) = crate::agent_runtime::spawn_child_tmux(
            &child_run.id,
            &state.working_dir,
            &prompt,
            step_model,
            &window_name,
            state.default_bot_name.as_deref(),
        ) {
            tracing::warn!("Failed to spawn parallel agent '{agent_label}': {e}");
            let _ = state
                .agent_mgr
                .update_run_failed(&child_run.id, &format!("spawn failed: {e}"));
            state.wf_mgr.update_step_status(
                &step_id,
                WorkflowStepStatus::Failed,
                Some(&child_run.id),
                Some(&format!("spawn failed: {e}")),
                None,
                None,
                None,
            )?;
            continue;
        }

        children.push(ParallelChild {
            agent_name: agent_label.to_string(),
            child_run_id: child_run.id,
            step_id,
            window_name,
            schema: call_schema.or_else(|| block_schema.clone()),
        });
    }

    // Poll all children until completion
    let start = std::time::Instant::now();
    let mut completed: HashSet<usize> = HashSet::new();
    let mut successes = 0u32;
    let mut failures = 0u32;
    let mut merged_markers: Vec<String> = Vec::new();

    loop {
        if completed.len() == children.len() {
            break;
        }
        if start.elapsed() > state.exec_config.step_timeout {
            tracing::warn!("parallel: timeout reached");
            // Cancel remaining
            for (i, child) in children.iter().enumerate() {
                if !completed.contains(&i) {
                    if let Err(e) = state.agent_mgr.update_run_cancelled(&child.child_run_id) {
                        tracing::warn!(
                            "parallel: failed to cancel run for '{}': {e}",
                            child.agent_name
                        );
                    }
                    let _ = Command::new("tmux")
                        .args(["kill-window", "-t", &format!(":{}", child.window_name)])
                        .output();
                    if let Err(e) = state.wf_mgr.update_step_status(
                        &child.step_id,
                        WorkflowStepStatus::Failed,
                        Some(&child.child_run_id),
                        Some("timed out"),
                        None,
                        None,
                        None,
                    ) {
                        tracing::warn!(
                            "parallel: failed to update timed-out step for '{}': {e}",
                            child.agent_name
                        );
                    }
                    failures += 1;
                    completed.insert(i);
                }
            }
            break;
        }

        for (i, child) in children.iter().enumerate() {
            if completed.contains(&i) {
                continue;
            }
            if let Ok(Some(run)) = state.agent_mgr.get_run(&child.child_run_id) {
                match run.status {
                    AgentRunStatus::Completed
                    | AgentRunStatus::Failed
                    | AgentRunStatus::Cancelled => {
                        completed.insert(i);
                        let succeeded = run.status == AgentRunStatus::Completed;

                        // In parallel blocks, schema validation failures fall back
                        // to generic parsing (no retry mechanism for individual calls).
                        let (markers, context, structured_json) = interpret_agent_output(
                            run.result_text.as_deref(),
                            child.schema.as_ref(),
                            succeeded,
                        )
                        .unwrap_or_else(|e| {
                            tracing::warn!(
                                "parallel: '{}' schema validation failed, falling back: {e}",
                                child.agent_name
                            );
                            let fb = run
                                .result_text
                                .as_deref()
                                .and_then(parse_conductor_output)
                                .unwrap_or_default();
                            (fb.markers, fb.context, None)
                        });

                        let markers_json = serde_json::to_string(&markers).unwrap_or_default();

                        let step_status = if succeeded {
                            successes += 1;
                            merged_markers.extend(markers.iter().cloned());
                            // Push parallel agent context so downstream {{prior_contexts}} can see it
                            state.contexts.push(ContextEntry {
                                step: child.agent_name.clone(),
                                iteration,
                                context: context.clone(),
                                markers: markers.clone(),
                                structured_output: structured_json.clone(),
                                output_file: None,
                            });
                            WorkflowStepStatus::Completed
                        } else {
                            failures += 1;
                            WorkflowStepStatus::Failed
                        };

                        if let Err(e) = state.wf_mgr.update_step_status_full(
                            &child.step_id,
                            step_status,
                            Some(&child.child_run_id),
                            run.result_text.as_deref(),
                            Some(&context),
                            Some(&markers_json),
                            None,
                            structured_json.as_deref(),
                        ) {
                            tracing::warn!(
                                "parallel: failed to update step status for '{}': {e}",
                                child.agent_name
                            );
                        }

                        if let Some(cost) = run.cost_usd {
                            state.total_cost += cost;
                        }
                        if let Some(turns) = run.num_turns {
                            state.total_turns += turns;
                        }
                        if let Some(dur) = run.duration_ms {
                            state.total_duration_ms += dur;
                        }

                        tracing::info!(
                            "parallel: '{}' {} (cost=${:.4})",
                            child.agent_name,
                            if succeeded { "completed" } else { "failed" },
                            run.cost_usd.unwrap_or(0.0),
                        );

                        // fail_fast: cancel remaining on first failure
                        if !succeeded && node.fail_fast {
                            tracing::warn!("parallel: fail_fast — cancelling remaining");
                            for (j, other) in children.iter().enumerate() {
                                if !completed.contains(&j) {
                                    if let Err(e) =
                                        state.agent_mgr.update_run_cancelled(&other.child_run_id)
                                    {
                                        tracing::warn!(
                                            "parallel: failed to cancel run for '{}': {e}",
                                            other.agent_name
                                        );
                                    }
                                    let _ = Command::new("tmux")
                                        .args([
                                            "kill-window",
                                            "-t",
                                            &format!(":{}", other.window_name),
                                        ])
                                        .output();
                                    if let Err(e) = state.wf_mgr.update_step_status(
                                        &other.step_id,
                                        WorkflowStepStatus::Failed,
                                        Some(&other.child_run_id),
                                        Some("cancelled by fail_fast"),
                                        None,
                                        None,
                                        None,
                                    ) {
                                        tracing::warn!(
                                            "parallel: failed to update step for '{}': {e}",
                                            other.agent_name
                                        );
                                    }
                                    completed.insert(j);
                                    failures += 1;
                                }
                            }
                        }
                    }
                    AgentRunStatus::Running | AgentRunStatus::WaitingForFeedback => {}
                }
            }
        }

        thread::sleep(state.exec_config.poll_interval);
    }

    // Apply min_success policy (skipped-on-resume agents count as successes)
    let effective_successes = successes + skipped_count;
    let total_agents = children.len() as u32 + skipped_count;
    let min_required = node.min_success.unwrap_or(total_agents);
    tracing::info!(
        "parallel: {successes} succeeded, {failures} failed, {skipped_count} skipped out of {total_agents} agents",
    );
    if effective_successes < min_required {
        tracing::warn!(
            "parallel: only {}/{} succeeded (min_success={})",
            effective_successes,
            total_agents,
            min_required
        );
        state.all_succeeded = false;
    }

    // Store merged markers as a synthetic result
    use crate::workflow::types::StepResult;
    let synthetic_result = StepResult {
        step_name: format!("parallel:{}", group_id),
        status: if effective_successes >= min_required {
            WorkflowStepStatus::Completed
        } else {
            WorkflowStepStatus::Failed
        },
        result_text: None,
        cost_usd: None,
        num_turns: None,
        duration_ms: None,
        markers: merged_markers,
        context: String::new(),
        child_run_id: None,
        structured_output: None,
        output_file: None,
    };
    state
        .step_results
        .insert(format!("parallel:{}", group_id), synthetic_result);

    Ok(())
}

pub(super) fn execute_gate(
    state: &mut ExecutionState<'_>,
    node: &GateNode,
    iteration: u32,
) -> Result<()> {
    let pos = state.position;
    state.position += 1;

    // Skip completed gates on resume — restore feedback for downstream steps
    if should_skip(state, &node.name, iteration) {
        tracing::info!("Skipping completed gate '{}'", node.name);
        restore_step(state, &node.name, iteration);
        return Ok(());
    }

    // Quality gates evaluate immediately — no blocking/waiting.
    if node.gate_type == GateType::QualityGate {
        return execute_quality_gate(state, node, pos, iteration);
    }

    // Dry-run: auto-approve all gates
    if state.exec_config.dry_run {
        tracing::info!("gate '{}': dry-run auto-approved", node.name);
        let step_id = state.wf_mgr.insert_step(
            &state.workflow_run_id,
            &node.name,
            "reviewer",
            false,
            pos,
            iteration as i64,
        )?;
        state.wf_mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Completed,
            None,
            Some("dry-run: auto-approved"),
            None,
            None,
            None,
        )?;
        return Ok(());
    }

    let step_id = state.wf_mgr.insert_step(
        &state.workflow_run_id,
        &node.name,
        "gate",
        false,
        pos,
        iteration as i64,
    )?;

    state.wf_mgr.set_step_gate_info(
        &step_id,
        node.gate_type.clone(),
        node.prompt.as_deref(),
        &format!("{}s", node.timeout_secs),
    )?;

    state.wf_mgr.update_step_status(
        &step_id,
        WorkflowStepStatus::Waiting,
        None,
        None,
        None,
        None,
        None,
    )?;

    // Atomically set status=Waiting and blocked_on in a single DB statement so
    // there is no observable window where status=Waiting but blocked_on=NULL.
    let gate_name = node.name.clone();
    let blocked_on = match node.gate_type {
        GateType::HumanApproval => super::types::BlockedOn::HumanApproval {
            gate_name,
            prompt: node.prompt.clone(),
        },
        GateType::HumanReview => super::types::BlockedOn::HumanReview {
            gate_name,
            prompt: node.prompt.clone(),
        },
        GateType::PrApproval => super::types::BlockedOn::PrApproval {
            gate_name,
            approvals_needed: node.min_approvals,
        },
        GateType::PrChecks => super::types::BlockedOn::PrChecks { gate_name },
        GateType::QualityGate => unreachable!("quality gates are handled above"),
    };
    state
        .wf_mgr
        .set_waiting_blocked_on(&state.workflow_run_id, &blocked_on)?;

    // Capture the bot name used for this gate (resolved fresh on each poll to avoid
    // using an expired installation token in long-running gate loops).
    let gate_effective_bot: Option<String> = node
        .bot_name
        .clone()
        .or_else(|| state.default_bot_name.clone());
    let gate_config = state.config;
    // Cache the installation token so we don't make a live HTTPS call on every
    // poll iteration.  Installation tokens are valid for 1 hour; we refresh
    // after 55 minutes to stay well inside that window.
    // Cache entry: (token_or_none, fetched_at).  `None` token means the last
    // fetch failed; those entries use a short 30-second TTL so we don't
    // hammer a misconfigured GitHub App key on every 5-second poll tick.
    let gate_token_cache: std::cell::RefCell<Option<(Option<String>, std::time::Instant)>> =
        std::cell::RefCell::new(None);
    let resolve_gate_token = || -> Option<String> {
        if gate_effective_bot.is_none() && gate_config.github.app.is_none() {
            return None;
        }
        let mut cache = gate_token_cache.borrow_mut();
        let needs_refresh = cache
            .as_ref()
            .map(|(cached_token, fetched_at)| {
                let ttl = if cached_token.is_some() {
                    Duration::from_secs(55 * 60)
                } else {
                    // Short retry TTL for failed fetches.
                    Duration::from_secs(30)
                };
                fetched_at.elapsed() > ttl
            })
            .unwrap_or(true);
        if needs_refresh {
            let token = crate::github_app::resolve_named_app_token(
                gate_config,
                gate_effective_bot.as_deref(),
                "gate",
            )
            .token()
            .map(String::from);
            // Always write to cache — on failure we store None with a short
            // TTL so repeated poll ticks don't retrigger the subprocess call.
            *cache = Some((token.clone(), std::time::Instant::now()));
            token
        } else {
            cache.as_ref().and_then(|(t, _)| t.clone())
        }
    };

    match node.gate_type {
        GateType::HumanApproval | GateType::HumanReview => {
            tracing::info!("Gate '{}' waiting for human action:", node.name);
            if let Some(ref p) = node.prompt {
                tracing::info!("  Prompt: {p}");
            }
            tracing::info!(
                "  Approve:  conductor workflow gate-approve {}",
                state.workflow_run_id
            );
            tracing::info!(
                "  Reject:   conductor workflow gate-reject {}",
                state.workflow_run_id
            );
            if node.gate_type == GateType::HumanReview {
                tracing::info!(
                    "  Feedback: conductor workflow gate-feedback {} \"<text>\"",
                    state.workflow_run_id
                );
            }

            // Poll DB for approval
            let start = std::time::Instant::now();
            loop {
                if start.elapsed() > Duration::from_secs(node.timeout_secs) {
                    return handle_gate_timeout(state, &step_id, node);
                }

                // Check if gate has been approved/rejected.
                // Use find_waiting_gate as a fast path, fall back to reading the
                // step directly when our gate is no longer the active waiting gate.
                let resolved_step =
                    if let Some(step) = state.wf_mgr.find_waiting_gate(&state.workflow_run_id)? {
                        if step.id == step_id {
                            Some(step)
                        } else {
                            // Another gate is now waiting — ours must have been resolved
                            state.wf_mgr.get_step_by_id(&step_id)?
                        }
                    } else {
                        // No waiting gate — ours must have been resolved
                        state.wf_mgr.get_step_by_id(&step_id)?
                    };

                if let Some(ref step) = resolved_step {
                    if step.gate_approved_at.is_some()
                        || step.status == WorkflowStepStatus::Completed
                    {
                        tracing::info!("Gate '{}' approved", node.name);
                        if let Some(ref feedback) = step.gate_feedback {
                            state.last_gate_feedback = Some(feedback.clone());
                        }
                        state.wf_mgr.update_workflow_status(
                            &state.workflow_run_id,
                            WorkflowRunStatus::Running,
                            None,
                        )?;
                        return Ok(());
                    }
                    if step.status == WorkflowStepStatus::Failed {
                        tracing::warn!("Gate '{}' rejected", node.name);
                        state.all_succeeded = false;
                        state.wf_mgr.update_workflow_status(
                            &state.workflow_run_id,
                            WorkflowRunStatus::Running,
                            None,
                        )?;
                        return Err(ConductorError::Workflow(format!(
                            "Gate '{}' rejected",
                            node.name
                        )));
                    }
                }

                thread::sleep(state.exec_config.poll_interval);
            }
        }
        GateType::PrApproval => {
            tracing::info!("Gate '{}' polling for PR approvals...", node.name);
            let start = std::time::Instant::now();
            loop {
                if start.elapsed() > Duration::from_secs(node.timeout_secs) {
                    return handle_gate_timeout(state, &step_id, node);
                }

                let gate_bot_token = resolve_gate_token();
                match node.approval_mode {
                    ApprovalMode::MinApprovals => {
                        // Poll gh pr view for raw approval count
                        let mut cmd = Command::new("gh");
                        cmd.args(["pr", "view", "--json", "reviews,author"])
                            .current_dir(&state.working_dir);
                        if let Some(ref token) = gate_bot_token {
                            cmd.env("GH_TOKEN", token);
                        }
                        let output = cmd.output();

                        if let Ok(out) = output {
                            if out.status.success() {
                                let json_str = String::from_utf8_lossy(&out.stdout);
                                if let Ok(val) =
                                    serde_json::from_str::<serde_json::Value>(&json_str)
                                {
                                    let pr_author =
                                        val["author"]["login"].as_str().unwrap_or("").to_string();
                                    let approvals = val["reviews"]
                                        .as_array()
                                        .map(|reviews| {
                                            reviews
                                                .iter()
                                                .filter(|r| {
                                                    r["state"].as_str() == Some("APPROVED")
                                                        && r["author"]["login"]
                                                            .as_str()
                                                            .unwrap_or("")
                                                            != pr_author
                                                })
                                                .count()
                                                as u32
                                        })
                                        .unwrap_or(0);
                                    if approvals >= node.min_approvals {
                                        tracing::info!(
                                            "Gate '{}': {} approvals (required {})",
                                            node.name,
                                            approvals,
                                            node.min_approvals
                                        );
                                        state.wf_mgr.approve_gate(&step_id, "gh", None)?;
                                        state.wf_mgr.update_workflow_status(
                                            &state.workflow_run_id,
                                            WorkflowRunStatus::Running,
                                            None,
                                        )?;
                                        return Ok(());
                                    }
                                }
                            }
                        }
                    }
                    ApprovalMode::ReviewDecision => {
                        // Poll gh pr view for GitHub's branch-protection-aware reviewDecision
                        let mut cmd = Command::new("gh");
                        cmd.args(["pr", "view", "--json", "reviewDecision"])
                            .current_dir(&state.working_dir);
                        if let Some(ref token) = gate_bot_token {
                            cmd.env("GH_TOKEN", token);
                        }
                        let output = cmd.output();

                        if let Ok(out) = output {
                            if out.status.success() {
                                let json_str = String::from_utf8_lossy(&out.stdout);
                                if let Ok(val) =
                                    serde_json::from_str::<serde_json::Value>(&json_str)
                                {
                                    let decision = val["reviewDecision"].as_str().unwrap_or("");
                                    tracing::info!(
                                        "Gate '{}': reviewDecision = {}",
                                        node.name,
                                        decision
                                    );
                                    if decision == "APPROVED" {
                                        state.wf_mgr.approve_gate(&step_id, "gh", None)?;
                                        state.wf_mgr.update_workflow_status(
                                            &state.workflow_run_id,
                                            WorkflowRunStatus::Running,
                                            None,
                                        )?;
                                        return Ok(());
                                    }
                                    // CHANGES_REQUESTED or REVIEW_REQUIRED: keep polling
                                }
                            }
                        }
                    }
                }

                thread::sleep(state.exec_config.poll_interval);
            }
        }
        GateType::PrChecks => {
            tracing::info!("Gate '{}' polling for PR checks...", node.name);
            let start = std::time::Instant::now();
            loop {
                if start.elapsed() > Duration::from_secs(node.timeout_secs) {
                    return handle_gate_timeout(state, &step_id, node);
                }

                let gate_bot_token = resolve_gate_token();
                let mut cmd = Command::new("gh");
                cmd.args(["pr", "checks", "--json", "state"])
                    .current_dir(&state.working_dir);
                if let Some(ref token) = gate_bot_token {
                    cmd.env("GH_TOKEN", token);
                }
                let output = cmd.output();

                if let Ok(out) = output {
                    if out.status.success() {
                        let json_str = String::from_utf8_lossy(&out.stdout);
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&json_str) {
                            if let Some(checks) = val.as_array() {
                                let all_pass = !checks.is_empty()
                                    && checks.iter().all(|c| {
                                        c["state"].as_str() == Some("SUCCESS")
                                            || c["state"].as_str() == Some("SKIPPED")
                                    });
                                if all_pass {
                                    tracing::info!("Gate '{}': all checks passing", node.name);
                                    state.wf_mgr.approve_gate(&step_id, "gh", None)?;
                                    state.wf_mgr.update_workflow_status(
                                        &state.workflow_run_id,
                                        WorkflowRunStatus::Running,
                                        None,
                                    )?;
                                    return Ok(());
                                }
                            }
                        }
                    }
                }

                thread::sleep(state.exec_config.poll_interval);
            }
        }
        GateType::QualityGate => {
            // Quality gates are handled earlier in execute_gate via execute_quality_gate.
            unreachable!("quality gates should not reach the blocking gate poll loop");
        }
    }
}

/// Evaluate a quality gate by checking a prior step's structured output against a threshold.
///
/// Quality gates are non-blocking: they evaluate immediately by reading the
/// `structured_output` from `step_results` for the configured `source` step,
/// parsing the JSON, and comparing the `confidence` field against `threshold`.
fn execute_quality_gate(
    state: &mut ExecutionState<'_>,
    node: &GateNode,
    pos: i64,
    iteration: u32,
) -> Result<()> {
    let qg = node.quality_gate.as_ref().ok_or_else(|| {
        ConductorError::Workflow(format!(
            "Quality gate '{}' is missing required quality_gate configuration (source, threshold)",
            node.name
        ))
    })?;
    let source = qg.source.as_str();
    let threshold = qg.threshold;
    let on_fail_action = qg.on_fail_action.clone();

    let step_id = state.wf_mgr.insert_step(
        &state.workflow_run_id,
        &node.name,
        "gate",
        false,
        pos,
        iteration as i64,
    )?;

    // Helper: update_step_status with no run_id, cost, duration, or attempt fields.
    let set_step_status = |status: WorkflowStepStatus, context: &str| -> Result<()> {
        state
            .wf_mgr
            .update_step_status(&step_id, status, None, Some(context), None, None, None)
    };

    // Look up the source step's structured output
    let (confidence, degradation_reason): (u32, Option<String>) = match state
        .step_results
        .get(source)
    {
        Some(result) => {
            if let Some(ref json_str) = result.structured_output {
                // Parse JSON and extract confidence field
                match serde_json::from_str::<serde_json::Value>(json_str) {
                    Ok(val) => {
                        // Try integer first, then fall back to float.
                        // Clamp to 100 to prevent u64→u32 truncation from wrapping
                        // large values into the passing range.
                        if let Some(c) = val.get("confidence").and_then(|v| v.as_u64()) {
                            (c.min(100) as u32, None)
                        } else if let Some(f) = val.get("confidence").and_then(|v| v.as_f64()) {
                            ((f as u64).min(100) as u32, None)
                        } else {
                            let reason = format!(
                                    "'confidence' key missing or not a number in structured output from '{}'",
                                    source
                                );
                            tracing::warn!("quality_gate '{}': {}", node.name, reason);
                            (0, Some(reason))
                        }
                    }
                    Err(e) => {
                        let reason =
                            format!("failed to parse structured output from '{}': {}", source, e);
                        tracing::warn!("quality_gate '{}': {}", node.name, reason);
                        (0, Some(reason))
                    }
                }
            } else {
                let reason = format!("source step '{}' has no structured output", source);
                tracing::warn!("quality_gate '{}': {}", node.name, reason);
                (0, Some(reason))
            }
        }
        None => {
            let msg = format!(
                "Quality gate '{}': source step '{}' not found in step results",
                node.name, source
            );
            set_step_status(WorkflowStepStatus::Failed, &msg)?;
            return Err(ConductorError::Workflow(msg));
        }
    };

    let passed = confidence >= threshold;
    let mut context = format!(
        "quality_gate: confidence={}, threshold={}, result={}",
        confidence,
        threshold,
        if passed { "pass" } else { "fail" }
    );
    if let Some(ref reason) = degradation_reason {
        context.push_str(&format!(" (confidence defaulted to 0: {})", reason));
    }

    if passed {
        tracing::info!(
            "quality_gate '{}': passed (confidence {} >= threshold {})",
            node.name,
            confidence,
            threshold
        );
        set_step_status(WorkflowStepStatus::Completed, &context)?;
    } else {
        tracing::warn!(
            "quality_gate '{}': failed (confidence {} < threshold {})",
            node.name,
            confidence,
            threshold
        );
        match on_fail_action {
            OnFailAction::Fail => {
                set_step_status(WorkflowStepStatus::Failed, &context)?;
                return Err(ConductorError::Workflow(format!(
                    "Quality gate '{}' failed: confidence {} is below threshold {}",
                    node.name, confidence, threshold
                )));
            }
            OnFailAction::Continue => {
                set_step_status(
                    WorkflowStepStatus::Completed,
                    &format!("{} (on_fail=continue, proceeding)", context),
                )?;
            }
        }
    }

    Ok(())
}

pub(super) fn handle_gate_timeout(
    state: &mut ExecutionState<'_>,
    step_id: &str,
    node: &GateNode,
) -> Result<()> {
    tracing::warn!("Gate '{}' timed out", node.name);
    match node.on_timeout {
        OnTimeout::Fail => {
            state.wf_mgr.update_step_status(
                step_id,
                WorkflowStepStatus::Failed,
                None,
                Some("gate timed out"),
                None,
                None,
                None,
            )?;
            state.all_succeeded = false;
            state.wf_mgr.update_workflow_status(
                &state.workflow_run_id,
                WorkflowRunStatus::Running,
                None,
            )?;
            Err(ConductorError::Workflow(format!(
                "Gate '{}' timed out",
                node.name
            )))
        }
        OnTimeout::Continue => {
            state.wf_mgr.update_step_status(
                step_id,
                WorkflowStepStatus::TimedOut,
                None,
                Some("gate timed out (continuing)"),
                None,
                None,
                None,
            )?;
            state.wf_mgr.update_workflow_status(
                &state.workflow_run_id,
                WorkflowRunStatus::Running,
                None,
            )?;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Script step executor
// ---------------------------------------------------------------------------

/// Maximum bytes read from a script's stdout file into memory.
/// Output beyond this limit is truncated with a notice appended.
const MAX_STDOUT_BYTES: usize = 100 * 1024; // 100 KB

/// Read at most [`MAX_STDOUT_BYTES`] from `path`, returning a UTF-8 string.
/// If the file is larger than the limit the content is truncated and a notice
/// is appended so callers can see that truncation occurred.
fn read_stdout_bounded(path: &str) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = Vec::with_capacity(MAX_STDOUT_BYTES + 1);
    f.by_ref()
        .take((MAX_STDOUT_BYTES + 1) as u64)
        .read_to_end(&mut buf)?;
    let truncated = buf.len() > MAX_STDOUT_BYTES;
    if truncated {
        buf.truncate(MAX_STDOUT_BYTES);
    }
    let mut s = String::from_utf8_lossy(&buf).into_owned();
    if truncated {
        s.push_str("\n...[output truncated at 100 KB]");
    }
    Ok(s)
}

/// Outcome of polling a spawned script child process.
enum ScriptPollResult {
    /// Process exited with success (exit code 0).
    Succeeded,
    /// Process exited with failure (non-zero exit code or wait error).
    Failed(String),
    /// Script exceeded its timeout; the process has been killed.
    TimedOut,
    /// Workflow shutdown signal received; the process has been killed.
    Cancelled,
}

/// Poll a child process until it exits, times out, or the shutdown signal fires.
///
/// Checks the shutdown flag and elapsed time every 200 ms using `try_wait`.
fn poll_script_child(
    child: &mut std::process::Child,
    timeout_secs: Option<u64>,
    shutdown: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> ScriptPollResult {
    let poll_interval = Duration::from_millis(200);
    let start = std::time::Instant::now();

    loop {
        // Check shutdown signal
        if let Some(flag) = shutdown {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                return ScriptPollResult::Cancelled;
            }
        }

        // Check per-step timeout
        if let Some(timeout) = timeout_secs {
            if start.elapsed().as_secs() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                return ScriptPollResult::TimedOut;
            }
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return ScriptPollResult::Succeeded;
                } else {
                    return ScriptPollResult::Failed(format!(
                        "script exited with non-zero status: {status}"
                    ));
                }
            }
            Ok(None) => thread::sleep(poll_interval),
            Err(e) => {
                return ScriptPollResult::Failed(format!("wait error: {e}"));
            }
        }
    }
}

pub(super) fn execute_script(
    state: &mut ExecutionState<'_>,
    node: &ScriptNode,
    iteration: u32,
) -> Result<()> {
    let pos = state.position;
    state.position += 1;

    // Check skip on resume
    if should_skip(state, &node.name, iteration) {
        tracing::info!(
            "Skipping completed script step '{}' (iteration {})",
            node.name,
            iteration
        );
        restore_step(state, &node.name, iteration);
        return Ok(());
    }

    let step_key = node.name.clone();
    let step_label = node.name.as_str();

    // Build variable map for substitution
    let vars = build_variable_map(state);

    // Resolve script path (substitute variables in run path first)
    let run_path_raw = substitute_variables(&node.run, &vars);
    let skills_dir =
        std::env::var_os("HOME").map(|h| std::path::PathBuf::from(&h).join(".claude/skills"));
    let resolved_path = crate::workflow_dsl::resolve_script_path(
        &run_path_raw,
        &state.working_dir,
        &state.repo_path,
        skills_dir.as_deref(),
    )
    .ok_or_else(|| {
        ConductorError::Workflow(format!(
            "Script step '{}': script '{}' not found in worktree, repo, or ~/.claude/skills/",
            step_label, run_path_raw
        ))
    })?;

    // Resolve env var values
    let resolved_env: std::collections::HashMap<String, String> = node
        .env
        .iter()
        .map(|(k, v)| (k.clone(), substitute_variables(v, &vars)))
        .collect();

    // Retry loop
    let max_attempts = 1 + node.retries;
    let mut last_error = String::new();

    for attempt in 0..max_attempts {
        let step_id = state.wf_mgr.insert_step(
            &state.workflow_run_id,
            step_label,
            "script",
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

        // Create a temp file for the script's stdout+stderr.
        // Both streams are redirected here so that subprocess output never
        // reaches the terminal directly (which would corrupt TUI rendering).
        let output_path = format!("{}/script-{}.out", state.working_dir, step_id);
        let output_file = std::fs::File::create(&output_path).map_err(|e| {
            ConductorError::Workflow(format!(
                "Script step '{}': failed to create output file: {e}",
                step_label
            ))
        })?;
        let stderr_file = output_file.try_clone().map_err(|e| {
            ConductorError::Workflow(format!(
                "Script step '{}': failed to clone output file handle for stderr: {e}",
                step_label
            ))
        })?;

        tracing::info!(
            "Script step '{}' (attempt {}/{}): running '{}'",
            step_label,
            attempt + 1,
            max_attempts,
            resolved_path.display(),
        );

        // Resolve GitHub App token for the bot identity (if `as = "..."` is set).
        // Inject it as GH_TOKEN so the script's `gh` calls use that bot identity.
        let effective_bot = node
            .bot_name
            .as_deref()
            .or(state.default_bot_name.as_deref());
        let mut cmd = Command::new(&resolved_path);
        cmd.envs(&resolved_env)
            .stdout(output_file)
            .stderr(stderr_file)
            .current_dir(&state.working_dir);
        // Inject conductor's binary directory into PATH so scripts can call `conductor`
        if let Some(ref bin_dir) = state.conductor_bin_dir {
            let path = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{}:{}", bin_dir.display(), path));
        }
        match crate::github_app::resolve_named_app_token(state.config, effective_bot, "script") {
            crate::github_app::TokenResolution::AppToken(token) => {
                cmd.env("GH_TOKEN", token);
            }
            crate::github_app::TokenResolution::Fallback { reason } => {
                tracing::warn!(
                    "Script step '{}': GitHub App token failed, using gh user identity: {reason}",
                    step_label
                );
            }
            crate::github_app::TokenResolution::NotConfigured => {}
        }
        let spawn_result = cmd.spawn();

        let mut child = match spawn_result {
            Ok(c) => c,
            Err(e) => {
                let err = format!(
                    "Script step '{}': failed to spawn '{}': {e}",
                    step_label,
                    resolved_path.display()
                );
                tracing::warn!("{err}");
                state.wf_mgr.update_step_status(
                    &step_id,
                    WorkflowStepStatus::Failed,
                    None,
                    Some(&err),
                    None,
                    None,
                    Some(attempt as i64),
                )?;
                last_error = err;
                continue;
            }
        };

        match poll_script_child(
            &mut child,
            node.timeout,
            state.exec_config.shutdown.as_ref(),
        ) {
            ScriptPollResult::Succeeded => {
                let stdout = read_stdout_bounded(&output_path).map_err(|e| {
                    ConductorError::Workflow(format!(
                        "Script step '{}': failed to read stdout file '{}': {e}",
                        step_label, output_path
                    ))
                })?;
                let parsed = parse_conductor_output(&stdout);
                let (markers, context) = match parsed {
                    Some(out) => (out.markers, out.context),
                    None => {
                        // Fallback: use truncated stdout as context
                        let truncated: String = stdout.chars().take(2000).collect();
                        (Vec::new(), truncated)
                    }
                };

                let markers_json = serde_json::to_string(&markers).unwrap_or_default();

                tracing::info!(
                    "Script step '{}' completed: markers={:?}",
                    step_label,
                    markers,
                );

                state.wf_mgr.update_step_status(
                    &step_id,
                    WorkflowStepStatus::Completed,
                    None,
                    Some(&stdout),
                    Some(&context),
                    Some(&markers_json),
                    Some(attempt as i64),
                )?;
                state.wf_mgr.set_step_output_file(&step_id, &output_path)?;

                record_step_success(
                    state,
                    step_key.clone(),
                    step_label,
                    Some(stdout),
                    None,
                    None,
                    None,
                    markers,
                    context,
                    None,
                    iteration,
                    None,
                    Some(output_path),
                );

                return Ok(());
            }

            ScriptPollResult::Failed(err) => {
                // Try to capture stdout so the failure message includes script output
                let stdout_snippet = read_stdout_bounded(&output_path)
                    .ok()
                    .filter(|s| !s.is_empty())
                    .map(|s| {
                        let snippet: String = s.chars().take(2000).collect();
                        format!("\n--- script stdout ---\n{snippet}")
                    })
                    .unwrap_or_default();
                let full_err = format!("{err}{stdout_snippet}");
                tracing::warn!(
                    "Script step '{}' failed (attempt {}/{}): {err}",
                    step_label,
                    attempt + 1,
                    max_attempts,
                );
                state.wf_mgr.update_step_status(
                    &step_id,
                    WorkflowStepStatus::Failed,
                    None,
                    Some(&full_err),
                    None,
                    None,
                    Some(attempt as i64),
                )?;
                last_error = full_err;
                // continue to next attempt
            }

            ScriptPollResult::TimedOut => {
                let msg = format!(
                    "script step '{}' timed out after {}s",
                    step_label,
                    node.timeout.unwrap_or(0)
                );
                tracing::warn!("{msg}");
                state.wf_mgr.update_step_status(
                    &step_id,
                    WorkflowStepStatus::TimedOut,
                    None,
                    Some(&msg),
                    None,
                    None,
                    Some(attempt as i64),
                )?;
                state.all_succeeded = false;
                if state.exec_config.fail_fast {
                    return Err(ConductorError::Workflow(msg));
                }
                return Ok(());
            }

            ScriptPollResult::Cancelled => {
                let msg = format!("script step '{step_label}' cancelled: workflow shutdown");
                state.wf_mgr.update_step_status(
                    &step_id,
                    WorkflowStepStatus::Failed,
                    None,
                    Some(&msg),
                    None,
                    None,
                    Some(attempt as i64),
                )?;
                return Err(ConductorError::Workflow(msg));
            }
        }
    }

    // All retries exhausted — run on_fail agent if specified
    if let Some(ref on_fail_agent) = node.on_fail {
        run_on_fail_agent(
            state,
            step_label,
            on_fail_agent,
            &last_error,
            node.retries,
            iteration,
        );
    }

    record_step_failure(state, step_key, step_label, last_error, max_attempts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::types::StepResult;
    use crate::workflow_dsl::QualityGateConfig;

    // -----------------------------------------------------------------------
    // Shared test helper: build an ExecutionState backed by a real in-memory DB.
    // -----------------------------------------------------------------------

    /// Build an `ExecutionState` wired to a real in-memory SQLite connection.
    ///
    /// The caller owns the `Connection`; the state borrows it.  `working_dir`
    /// and `repo_path` are both set to `dir`.
    fn make_test_state<'a>(
        conn: &'a rusqlite::Connection,
        config: &'a crate::config::Config,
        dir: &str,
        exec_config: crate::workflow::types::WorkflowExecConfig,
    ) -> ExecutionState<'a> {
        let agent_mgr = crate::agent::AgentManager::new(conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "test", None, None)
            .unwrap();
        let wf_mgr = crate::workflow::manager::WorkflowManager::new(conn);
        let run = wf_mgr
            .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        ExecutionState {
            conn,
            config,
            workflow_run_id: run.id,
            workflow_name: "test-wf".into(),
            worktree_id: None,
            working_dir: dir.to_string(),
            worktree_slug: String::new(),
            repo_path: dir.to_string(),
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config,
            inputs: std::collections::HashMap::new(),
            agent_mgr: crate::agent::AgentManager::new(conn),
            wf_mgr: crate::workflow::manager::WorkflowManager::new(conn),
            parent_run_id: String::new(),
            depth: 0,
            target_label: None,
            step_results: std::collections::HashMap::new(),
            contexts: Vec::new(),
            position: 0,
            all_succeeded: true,
            total_cost: 0.0,
            total_turns: 0,
            total_duration_ms: 0,
            last_gate_feedback: None,
            block_output: None,
            block_with: Vec::new(),
            resume_ctx: None,
            default_bot_name: None,
            feature_id: None,
            triggered_by_hook: false,
            conductor_bin_dir: None,
        }
    }

    // -----------------------------------------------------------------------
    // read_stdout_bounded tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_read_stdout_bounded_small_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        std::fs::write(&path, "hello world").unwrap();
        let s = read_stdout_bounded(path.to_str().unwrap()).unwrap();
        assert_eq!(s, "hello world");
    }

    #[test]
    fn test_read_stdout_bounded_large_file_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        // Write 200 KB of data (over the 100 KB limit)
        let data = "A".repeat(200 * 1024);
        std::fs::write(&path, &data).unwrap();
        let s = read_stdout_bounded(path.to_str().unwrap()).unwrap();
        assert!(s.len() < data.len(), "output should be truncated");
        assert!(
            s.contains("[output truncated at 100 KB]"),
            "truncation notice should be present"
        );
    }

    #[test]
    fn test_read_stdout_bounded_missing_file() {
        let result = read_stdout_bounded("/nonexistent/path/file.txt");
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // execute_script integration tests
    // -----------------------------------------------------------------------

    /// Write a shell script to `path`, make it executable, and return the absolute path string.
    fn write_script(path: &std::path::Path, body: &str) -> String {
        std::fs::write(path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path.to_str().unwrap().to_string()
    }

    #[test]
    fn test_execute_script_success() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = write_script(
            &dir.path().join("hello.sh"),
            "#!/bin/sh\necho '<<<CONDUCTOR_OUTPUT>>>\n{\"markers\": [\"done\"], \"context\": \"ran ok\"}\n<<<END_CONDUCTOR_OUTPUT>>>'",
        );

        let conn = crate::test_helpers::setup_db();
        let config = Box::leak(Box::new(crate::config::Config::default()));
        let dir_str = dir.path().to_str().unwrap().to_string();
        let mut state = make_test_state(
            &conn,
            config,
            &dir_str,
            crate::workflow::types::WorkflowExecConfig::default(),
        );

        let node = crate::workflow_dsl::ScriptNode {
            name: "hello".into(),
            run: script_path,
            env: std::collections::HashMap::new(),
            timeout: Some(10),
            retries: 0,
            on_fail: None,
            bot_name: None,
        };

        let result = execute_script(&mut state, &node, 0);
        assert!(result.is_ok(), "execute_script should succeed: {result:?}");
        assert!(state.all_succeeded);
        let step_res = state.step_results.get("hello").unwrap();
        assert!(step_res.markers.contains(&"done".to_string()));
        assert_eq!(step_res.context, "ran ok");
        assert!(
            state.contexts.iter().any(|c| c.output_file.is_some()),
            "output_file should be set in context"
        );
    }

    #[test]
    fn test_execute_script_failure_captures_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = write_script(
            &dir.path().join("fail.sh"),
            "#!/bin/sh\necho 'something before failure'\nexit 1",
        );

        let conn = crate::test_helpers::setup_db();
        let config = Box::leak(Box::new(crate::config::Config::default()));
        let dir_str = dir.path().to_str().unwrap().to_string();
        let mut state = make_test_state(
            &conn,
            config,
            &dir_str,
            crate::workflow::types::WorkflowExecConfig {
                fail_fast: false,
                ..Default::default()
            },
        );

        let node = crate::workflow_dsl::ScriptNode {
            name: "fail".into(),
            run: script_path,
            env: std::collections::HashMap::new(),
            timeout: Some(10),
            retries: 0,
            on_fail: None,
            bot_name: None,
        };

        // Should return Ok (not an Err) because fail_fast is false; all_succeeded flips false
        let result = execute_script(&mut state, &node, 0);
        assert!(result.is_ok());
        assert!(!state.all_succeeded);
        let step_res = state.step_results.get("fail").unwrap();
        // The result_text should contain the stdout snippet
        let result_text = step_res.result_text.as_deref().unwrap_or("");
        assert!(
            result_text.contains("something before failure"),
            "stdout should be captured in failure result, got: {result_text}"
        );
    }

    // -----------------------------------------------------------------------
    // poll_script_child unit tests — timeout and cancellation
    // -----------------------------------------------------------------------

    #[test]
    fn test_poll_script_child_timeout() {
        // Spawn a long-running process; timeout=0 fires immediately.
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("failed to spawn sleep");
        let result = poll_script_child(&mut child, Some(0), None);
        assert!(
            matches!(result, ScriptPollResult::TimedOut),
            "expected TimedOut, got other variant"
        );
    }

    #[test]
    fn test_poll_script_child_cancelled() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let flag = Arc::new(AtomicBool::new(true)); // already cancelled
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("failed to spawn sleep");
        let result = poll_script_child(&mut child, None, Some(&flag));
        assert!(
            matches!(result, ScriptPollResult::Cancelled),
            "expected Cancelled, got other variant"
        );
        // Verify flag didn't reset
        assert!(flag.load(Ordering::Relaxed));
    }

    // -----------------------------------------------------------------------
    // execute_script — bot_name / GH_TOKEN injection path
    // -----------------------------------------------------------------------

    #[test]
    fn test_execute_script_with_bot_name_not_configured() {
        // When bot_name is set but no GitHub App is configured, the script
        // should still run successfully (NotConfigured path — no GH_TOKEN injected).
        let dir = tempfile::tempdir().unwrap();
        let script_path = write_script(
            &dir.path().join("bot.sh"),
            "#!/bin/sh\necho '<<<CONDUCTOR_OUTPUT>>>\n{\"context\": \"bot ran\"}\n<<<END_CONDUCTOR_OUTPUT>>>'",
        );

        let conn = crate::test_helpers::setup_db();
        let config = Box::leak(Box::new(crate::config::Config::default()));
        let dir_str = dir.path().to_str().unwrap().to_string();
        let mut state = make_test_state(
            &conn,
            config,
            &dir_str,
            crate::workflow::types::WorkflowExecConfig::default(),
        );

        let node = crate::workflow_dsl::ScriptNode {
            name: "bot-step".into(),
            run: script_path,
            env: std::collections::HashMap::new(),
            timeout: Some(10),
            retries: 0,
            on_fail: None,
            bot_name: Some("my-bot".into()),
        };

        let result = execute_script(&mut state, &node, 0);
        assert!(
            result.is_ok(),
            "execute_script with bot_name should succeed: {result:?}"
        );
        assert!(state.all_succeeded);
        let step_res = state.step_results.get("bot-step").unwrap();
        assert_eq!(step_res.context, "bot ran");
    }

    #[test]
    fn test_execute_script_bot_name_falls_back_to_default() {
        // When node.bot_name is None but state.default_bot_name is set,
        // the effective_bot should use the default. With no app configured,
        // this exercises the fallback logic without crashing.
        let dir = tempfile::tempdir().unwrap();
        let script_path = write_script(
            &dir.path().join("default-bot.sh"),
            "#!/bin/sh\necho '<<<CONDUCTOR_OUTPUT>>>\n{\"context\": \"default bot ran\"}\n<<<END_CONDUCTOR_OUTPUT>>>'",
        );

        let conn = crate::test_helpers::setup_db();
        let config = Box::leak(Box::new(crate::config::Config::default()));
        let dir_str = dir.path().to_str().unwrap().to_string();
        let mut state = make_test_state(
            &conn,
            config,
            &dir_str,
            crate::workflow::types::WorkflowExecConfig::default(),
        );
        state.default_bot_name = Some("default-bot".into());

        let node = crate::workflow_dsl::ScriptNode {
            name: "default-bot-step".into(),
            run: script_path,
            env: std::collections::HashMap::new(),
            timeout: Some(10),
            retries: 0,
            on_fail: None,
            bot_name: None,
        };

        let result = execute_script(&mut state, &node, 0);
        assert!(
            result.is_ok(),
            "execute_script with default bot should succeed: {result:?}"
        );
        assert!(state.all_succeeded);
        let step_res = state.step_results.get("default-bot-step").unwrap();
        assert_eq!(step_res.context, "default bot ran");
    }

    // -----------------------------------------------------------------------
    // execute_script — timeout path
    // -----------------------------------------------------------------------

    #[test]
    fn test_execute_script_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = write_script(&dir.path().join("slow.sh"), "#!/bin/sh\nsleep 60");

        let conn = crate::test_helpers::setup_db();
        let config = Box::leak(Box::new(crate::config::Config::default()));
        let dir_str = dir.path().to_str().unwrap().to_string();
        let mut state = make_test_state(
            &conn,
            config,
            &dir_str,
            crate::workflow::types::WorkflowExecConfig {
                fail_fast: false,
                ..Default::default()
            },
        );

        let node = crate::workflow_dsl::ScriptNode {
            name: "slow".into(),
            run: script_path,
            env: std::collections::HashMap::new(),
            timeout: Some(0), // expires immediately
            retries: 0,
            on_fail: None,
            bot_name: None,
        };

        let result = execute_script(&mut state, &node, 0);
        // fail_fast=false → returns Ok, but all_succeeded is false
        assert!(
            result.is_ok(),
            "expected Ok on timeout with fail_fast=false: {result:?}"
        );
        assert!(
            !state.all_succeeded,
            "all_succeeded should be false after timeout"
        );

        // DB step should be marked TimedOut
        let steps = state
            .wf_mgr
            .get_workflow_steps(&state.workflow_run_id)
            .unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(
            steps[0].status,
            super::super::status::WorkflowStepStatus::TimedOut
        );
    }

    // -----------------------------------------------------------------------
    // execute_script — cancellation path
    // -----------------------------------------------------------------------

    #[test]
    fn test_execute_script_cancelled() {
        use std::sync::{atomic::AtomicBool, Arc};
        let dir = tempfile::tempdir().unwrap();
        let script_path = write_script(&dir.path().join("cancel.sh"), "#!/bin/sh\nsleep 60");

        let shutdown = Arc::new(AtomicBool::new(true)); // pre-cancelled
        let conn = crate::test_helpers::setup_db();
        let config = Box::leak(Box::new(crate::config::Config::default()));
        let dir_str = dir.path().to_str().unwrap().to_string();
        let mut state = make_test_state(
            &conn,
            config,
            &dir_str,
            crate::workflow::types::WorkflowExecConfig {
                shutdown: Some(Arc::clone(&shutdown)),
                ..Default::default()
            },
        );

        let node = crate::workflow_dsl::ScriptNode {
            name: "cancel".into(),
            run: script_path,
            env: std::collections::HashMap::new(),
            timeout: None,
            retries: 0,
            on_fail: None,
            bot_name: None,
        };

        let result = execute_script(&mut state, &node, 0);
        assert!(result.is_err(), "expected Err on cancellation");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("cancel") || err_msg.contains("cancelled"),
            "error message should mention cancellation: {err_msg}"
        );
        assert!(
            err_msg.contains("cancel"), // step name included
            "error message should include step name 'cancel': {err_msg}"
        );
    }

    // -----------------------------------------------------------------------
    // execute_script — retry path
    // -----------------------------------------------------------------------

    #[test]
    fn test_execute_script_retries_exhausted() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = write_script(&dir.path().join("flaky.sh"), "#!/bin/sh\nexit 1");

        let conn = crate::test_helpers::setup_db();
        let config = Box::leak(Box::new(crate::config::Config::default()));
        let dir_str = dir.path().to_str().unwrap().to_string();
        let mut state = make_test_state(
            &conn,
            config,
            &dir_str,
            crate::workflow::types::WorkflowExecConfig {
                fail_fast: false, // don't short-circuit on first failure
                ..Default::default()
            },
        );
        let run_id = state.workflow_run_id.clone();

        let node = crate::workflow_dsl::ScriptNode {
            name: "flaky".into(),
            run: script_path,
            env: std::collections::HashMap::new(),
            timeout: Some(10),
            retries: 2, // 3 attempts total
            on_fail: None,
            bot_name: None,
        };

        let result = execute_script(&mut state, &node, 0);
        assert!(
            result.is_ok(),
            "fail_fast=false: expected Ok after retries: {result:?}"
        );
        assert!(
            !state.all_succeeded,
            "all_succeeded should be false after exhausting retries"
        );

        // Three step records should exist (one per attempt)
        let steps = state.wf_mgr.get_workflow_steps(&run_id).unwrap();
        assert_eq!(
            steps.len(),
            3,
            "expected 3 step DB records (one per attempt), got {}",
            steps.len()
        );
        for step in &steps {
            assert_eq!(
                step.status,
                super::super::status::WorkflowStepStatus::Failed,
                "each attempt should be marked Failed"
            );
        }
    }

    // -----------------------------------------------------------------------
    // eval_condition / execute_if / execute_unless — BoolInput tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_eval_condition_bool_input_true() {
        let db = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let mut state = make_test_state(&db, &config, "/tmp", Default::default());
        state.inputs.insert("flag".to_string(), "true".to_string());

        let cond = Condition::BoolInput {
            input: "flag".to_string(),
        };
        assert!(eval_condition(&state, &cond));
    }

    #[test]
    fn test_eval_condition_bool_input_false() {
        let db = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let mut state = make_test_state(&db, &config, "/tmp", Default::default());
        state.inputs.insert("flag".to_string(), "false".to_string());

        let cond = Condition::BoolInput {
            input: "flag".to_string(),
        };
        assert!(!eval_condition(&state, &cond));
    }

    #[test]
    fn test_eval_condition_bool_input_missing_defaults_false() {
        let db = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let state = make_test_state(&db, &config, "/tmp", Default::default());

        let cond = Condition::BoolInput {
            input: "missing".to_string(),
        };
        assert!(!eval_condition(&state, &cond));
    }

    #[test]
    fn test_eval_condition_bool_input_case_insensitive() {
        let db = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let mut state = make_test_state(&db, &config, "/tmp", Default::default());
        state.inputs.insert("flag".to_string(), "TRUE".to_string());

        let cond = Condition::BoolInput {
            input: "flag".to_string(),
        };
        assert!(eval_condition(&state, &cond));
    }

    #[test]
    fn test_execute_if_bool_input_runs_body_when_true() {
        let db = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let mut state = make_test_state(&db, &config, "/tmp", Default::default());
        state
            .inputs
            .insert("run_it".to_string(), "true".to_string());

        // Body has one echo script node — just verify execute_if doesn't error
        // and returns Ok (actual body execution is covered by script tests).
        let node = IfNode {
            condition: Condition::BoolInput {
                input: "run_it".to_string(),
            },
            body: vec![],
        };
        assert!(execute_if(&mut state, &node).is_ok());
    }

    #[test]
    fn test_execute_if_bool_input_skips_body_when_false() {
        let db = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let mut state = make_test_state(&db, &config, "/tmp", Default::default());
        state
            .inputs
            .insert("run_it".to_string(), "false".to_string());

        let node = IfNode {
            condition: Condition::BoolInput {
                input: "run_it".to_string(),
            },
            body: vec![],
        };
        assert!(execute_if(&mut state, &node).is_ok());
    }

    #[test]
    fn test_execute_unless_bool_input_runs_body_when_false() {
        let db = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let mut state = make_test_state(&db, &config, "/tmp", Default::default());
        state.inputs.insert("skip".to_string(), "false".to_string());

        let node = UnlessNode {
            condition: Condition::BoolInput {
                input: "skip".to_string(),
            },
            body: vec![],
        };
        assert!(execute_unless(&mut state, &node).is_ok());
    }

    #[test]
    fn test_execute_script_injects_conductor_on_path() {
        // Verify that the conductor binary's directory is prepended to PATH
        // by printing PATH from inside the script and checking it contains
        // the current exe's parent directory.
        let dir = tempfile::tempdir().unwrap();
        let script_path = write_script(
            &dir.path().join("check_path.sh"),
            "#!/bin/sh\necho \"$PATH\"",
        );

        let conn = crate::test_helpers::setup_db();
        let config = Box::leak(Box::new(crate::config::Config::default()));
        let dir_str = dir.path().to_str().unwrap().to_string();
        let mut state = make_test_state(
            &conn,
            config,
            &dir_str,
            crate::workflow::types::WorkflowExecConfig::default(),
        );

        // Simulate what binary crates do: resolve conductor binary dir from current_exe.
        let bin_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        state.conductor_bin_dir = bin_dir;

        let node = crate::workflow_dsl::ScriptNode {
            name: "check_path".into(),
            run: script_path,
            env: std::collections::HashMap::new(),
            timeout: Some(10),
            retries: 0,
            on_fail: None,
            bot_name: None,
        };

        let result = execute_script(&mut state, &node, 0);
        assert!(result.is_ok(), "execute_script should succeed: {result:?}");

        // Read the stdout log to verify PATH contains the conductor binary dir
        let ctx = state.contexts.last().unwrap();
        let log_path = ctx.output_file.as_ref().unwrap();
        let output = std::fs::read_to_string(log_path).unwrap();
        let exe_dir = state
            .conductor_bin_dir
            .as_ref()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(
            output.contains(&exe_dir),
            "PATH should contain conductor binary dir '{exe_dir}', got: {output}"
        );
    }

    #[test]
    fn test_execute_unless_bool_input_skips_body_when_true() {
        let db = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let mut state = make_test_state(&db, &config, "/tmp", Default::default());
        state.inputs.insert("skip".to_string(), "true".to_string());

        let node = UnlessNode {
            condition: Condition::BoolInput {
                input: "skip".to_string(),
            },
            body: vec![],
        };
        assert!(execute_unless(&mut state, &node).is_ok());
    }

    // -----------------------------------------------------------------------
    // execute_quality_gate tests
    // -----------------------------------------------------------------------

    fn make_step_result(structured_output: Option<&str>) -> StepResult {
        StepResult {
            step_name: "review".to_string(),
            status: WorkflowStepStatus::Completed,
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            markers: vec![],
            context: String::new(),
            child_run_id: None,
            structured_output: structured_output.map(|s| s.to_string()),
            output_file: None,
        }
    }

    fn make_quality_gate_node(
        name: &str,
        source: Option<&str>,
        threshold: Option<u32>,
        on_fail: OnFailAction,
    ) -> GateNode {
        let quality_gate = match (source, threshold) {
            (Some(s), Some(t)) => Some(QualityGateConfig {
                source: s.to_string(),
                threshold: t,
                on_fail_action: on_fail,
            }),
            // Allow constructing nodes with missing config for error-path tests
            _ => None,
        };
        GateNode {
            name: name.to_string(),
            gate_type: GateType::QualityGate,
            prompt: None,
            min_approvals: 1,
            approval_mode: ApprovalMode::default(),
            timeout_secs: 60,
            on_timeout: OnTimeout::Fail,
            bot_name: None,
            quality_gate,
        }
    }

    #[test]
    fn test_quality_gate_passes_when_confidence_meets_threshold() {
        let conn = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

        state.step_results.insert(
            "review".to_string(),
            make_step_result(Some(r#"{"confidence": 80}"#)),
        );

        let node = make_quality_gate_node("qg", Some("review"), Some(70), OnFailAction::Fail);
        let result = execute_quality_gate(&mut state, &node, 0, 0);
        assert!(result.is_ok(), "gate should pass: {result:?}");
    }

    #[test]
    fn test_quality_gate_fails_when_confidence_below_threshold() {
        let conn = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

        state.step_results.insert(
            "review".to_string(),
            make_step_result(Some(r#"{"confidence": 40}"#)),
        );

        let node = make_quality_gate_node("qg", Some("review"), Some(70), OnFailAction::Fail);
        let result = execute_quality_gate(&mut state, &node, 0, 0);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("below threshold"), "got: {err}");
    }

    #[test]
    fn test_quality_gate_continues_on_fail_when_configured() {
        let conn = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

        state.step_results.insert(
            "review".to_string(),
            make_step_result(Some(r#"{"confidence": 20}"#)),
        );

        let node = make_quality_gate_node("qg", Some("review"), Some(70), OnFailAction::Continue);
        let result = execute_quality_gate(&mut state, &node, 0, 0);
        assert!(
            result.is_ok(),
            "on_fail=continue should not error: {result:?}"
        );
    }

    #[test]
    fn test_quality_gate_errors_when_source_step_missing() {
        let conn = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

        let node = make_quality_gate_node("qg", Some("nonexistent"), Some(70), OnFailAction::Fail);
        let result = execute_quality_gate(&mut state, &node, 0, 0);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found in step results"), "got: {err}");
    }

    #[test]
    fn test_quality_gate_errors_when_config_missing() {
        let conn = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

        let node = make_quality_gate_node("qg", None, None, OnFailAction::Fail);
        let result = execute_quality_gate(&mut state, &node, 0, 0);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("missing required quality_gate configuration"),
            "got: {err}"
        );
    }

    #[test]
    fn test_quality_gate_malformed_json_treats_as_zero_confidence() {
        let conn = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

        state.step_results.insert(
            "review".to_string(),
            make_step_result(Some("not valid json")),
        );

        // threshold=0 so even confidence=0 passes
        let node = make_quality_gate_node("qg", Some("review"), Some(0), OnFailAction::Fail);
        let result = execute_quality_gate(&mut state, &node, 0, 0);
        assert!(
            result.is_ok(),
            "malformed JSON → confidence=0, threshold=0 should pass: {result:?}"
        );
    }

    #[test]
    fn test_quality_gate_missing_confidence_key_treats_as_zero() {
        let conn = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

        state.step_results.insert(
            "review".to_string(),
            make_step_result(Some(r#"{"score": 95}"#)),
        );

        // JSON is valid but has no "confidence" key — should fail at threshold 70
        let node = make_quality_gate_node("qg", Some("review"), Some(70), OnFailAction::Fail);
        let result = execute_quality_gate(&mut state, &node, 0, 0);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("below threshold"), "got: {err}");
    }

    #[test]
    fn test_quality_gate_no_structured_output_treats_as_zero() {
        let conn = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

        state
            .step_results
            .insert("review".to_string(), make_step_result(None));

        let node = make_quality_gate_node("qg", Some("review"), Some(50), OnFailAction::Fail);
        let result = execute_quality_gate(&mut state, &node, 0, 0);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("below threshold"), "got: {err}");
    }

    #[test]
    fn test_quality_gate_float_confidence_handled() {
        let conn = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

        state.step_results.insert(
            "review".to_string(),
            make_step_result(Some(r#"{"confidence": 85.5}"#)),
        );

        // Float 85.5 should be truncated to 85 and pass threshold of 70
        let node = make_quality_gate_node("qg", Some("review"), Some(70), OnFailAction::Fail);
        let result = execute_quality_gate(&mut state, &node, 0, 0);
        assert!(
            result.is_ok(),
            "float confidence should be handled: {result:?}"
        );
    }

    #[test]
    fn test_quality_gate_clamps_large_confidence_to_100() {
        let conn = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

        state.step_results.insert(
            "review".to_string(),
            make_step_result(Some(r#"{"confidence": 999999}"#)),
        );

        // Large value should be clamped to 100, passing threshold of 90
        let node = make_quality_gate_node("qg", Some("review"), Some(90), OnFailAction::Fail);
        let result = execute_quality_gate(&mut state, &node, 0, 0);
        assert!(
            result.is_ok(),
            "large confidence should be clamped to 100 and pass: {result:?}"
        );
    }

    #[test]
    fn test_quality_gate_clamps_large_float_confidence_to_100() {
        let conn = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

        // Use a float value to exercise the as_f64() fallback branch
        state.step_results.insert(
            "review".to_string(),
            make_step_result(Some(r#"{"confidence": 9999.9}"#)),
        );

        // Large float should be clamped to 100, passing threshold of 90
        let node = make_quality_gate_node("qg", Some("review"), Some(90), OnFailAction::Fail);
        let result = execute_quality_gate(&mut state, &node, 0, 0);
        assert!(
            result.is_ok(),
            "large float confidence should be clamped to 100 and pass: {result:?}"
        );
    }

    #[test]
    fn test_execute_gate_dispatches_quality_gate() {
        let conn = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let mut state = make_test_state(&conn, &config, "/tmp", Default::default());

        state.step_results.insert(
            "review".to_string(),
            make_step_result(Some(r#"{"confidence": 90}"#)),
        );

        let node = make_quality_gate_node("qg", Some("review"), Some(70), OnFailAction::Fail);
        let result = execute_gate(&mut state, &node, 0);
        assert!(
            result.is_ok(),
            "execute_gate should dispatch QualityGate correctly: {result:?}"
        );
    }
}
