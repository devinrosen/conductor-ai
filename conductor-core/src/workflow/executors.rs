use std::collections::HashSet;
use std::process::Command;
use std::thread;
use std::time::Duration;

use crate::agent::AgentRunStatus;
use crate::agent_config::AgentSpec;
use crate::error::{ConductorError, Result};
use crate::workflow_dsl::{
    ApprovalMode, CallNode, CallWorkflowNode, DoNode, DoWhileNode, GateNode, GateType, IfNode,
    OnTimeout, ParallelNode, UnlessNode, WhileNode,
};

use super::engine::{
    bubble_up_child_step_results, check_max_iterations, check_stuck, execute_nodes,
    fetch_child_final_output, record_step_failure, record_step_success, resolve_child_inputs,
    resolve_schema, restore_step, run_on_fail_agent, should_skip, ExecutionState,
};
use super::helpers::{find_max_completed_while_iteration, sanitize_tmux_name};
use super::output::{interpret_agent_output, parse_conductor_output};
use super::prompt_builder::{build_agent_prompt, build_variable_map};
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
        };

        match super::engine::resume_workflow(&resume_input) {
            Ok(result) if result.all_succeeded => {
                tracing::info!(
                    "Sub-workflow '{}' resumed and completed: cost=${:.4}, {} turns",
                    node.workflow,
                    result.total_cost,
                    result.total_turns,
                );

                let (markers, context) =
                    fetch_child_final_output(&state.wf_mgr, &result.workflow_run_id);

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
                );

                let child_steps =
                    bubble_up_child_step_results(&state.wf_mgr, &result.workflow_run_id);
                for (key, value) in child_steps {
                    state.step_results.entry(key).or_insert(value);
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
            run_id_notify: None,
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

                    // Bubble up the child's final step output (markers + context)
                    let (markers, context) =
                        fetch_child_final_output(&state.wf_mgr, &result.workflow_run_id);

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
                    );

                    // Bubble up child step results so parent can reference internal
                    // sub-workflow markers (e.g. review-aggregator.has_review_issues).
                    let child_steps =
                        bubble_up_child_step_results(&state.wf_mgr, &result.workflow_run_id);
                    for (key, value) in child_steps {
                        state.step_results.entry(key).or_insert(value);
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

pub(super) fn execute_if(state: &mut ExecutionState<'_>, node: &IfNode) -> Result<()> {
    let has_marker = state
        .step_results
        .get(&node.step)
        .map(|r| r.markers.iter().any(|m| m == &node.marker))
        .unwrap_or(false);

    if has_marker {
        tracing::info!(
            "if {}.{} — condition met, executing body",
            node.step,
            node.marker
        );
        execute_nodes(state, &node.body)?;
    } else {
        tracing::info!(
            "if {}.{} — condition not met, skipping",
            node.step,
            node.marker
        );
    }

    Ok(())
}

pub(super) fn execute_unless(state: &mut ExecutionState<'_>, node: &UnlessNode) -> Result<()> {
    let has_marker = state
        .step_results
        .get(&node.step)
        .map(|r| r.markers.iter().any(|m| m == &node.marker))
        .unwrap_or(false);

    if !has_marker {
        tracing::info!(
            "unless {}.{} — marker absent, executing body",
            node.step,
            node.marker
        );
        execute_nodes(state, &node.body)?;
    } else {
        tracing::info!(
            "unless {}.{} — marker present, skipping",
            node.step,
            node.marker
        );
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

pub(super) fn execute_do_while(
    state: &mut ExecutionState<'_>,
    node: &DoWhileNode,
) -> Result<()> {
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
    let group_id = ulid::Ulid::new().to_string();
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
        if let Some((cond_step, cond_marker)) = node.call_if.get(&i) {
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
            .get(&i)
            .map(|name| resolve_schema(state, name))
            .transpose()?;
        let effective_schema = call_schema.as_ref().or(block_schema.as_ref());

        // Combine block-level `with` + per-call `with` additions
        let mut effective_with = node.with.clone();
        if let Some(extra) = node.call_with.get(&i) {
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
    use super::types::StepResult;
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
        &node.gate_type.to_string(),
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

    // Update workflow run to waiting status
    state.wf_mgr.update_workflow_status(
        &state.workflow_run_id,
        WorkflowRunStatus::Waiting,
        None,
    )?;

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
    }
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
