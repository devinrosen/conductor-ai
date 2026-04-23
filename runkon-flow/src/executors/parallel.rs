use std::sync::Arc;

use crate::cancellation_reason::CancellationReason;
use crate::dsl::ParallelNode;
use crate::engine::{resolve_schema, restore_step, should_skip, ExecutionState};
use crate::engine_error::{EngineError, Result};
use crate::status::WorkflowStepStatus;
use crate::traits::action_executor::{ActionOutput, ActionParams, ExecutionContext};
use crate::traits::persistence::{NewStep, StepUpdate};
use crate::types::{ContextEntry, StepResult};

pub fn execute_parallel(
    state: &mut ExecutionState,
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

    struct ParallelCallResult {
        agent_name: String,
        step_id: String,
        result: std::result::Result<ActionOutput, EngineError>,
        attempt: u32,
    }

    let mut skipped_count = 0u32;
    let mut call_inputs: Vec<(
        usize,
        String,
        Option<crate::output_schema::OutputSchema>,
        Vec<String>,
    )> = Vec::new();

    // First pass: skip any already-completed agents on resume
    for (i, agent_ref) in node.calls.iter().enumerate() {
        let pos = pos_base + i as i64;
        state.position = pos + 1;
        let agent_label = agent_ref.label();
        let agent_step_key = agent_ref.step_key();

        if should_skip(state, &agent_step_key, iteration) {
            tracing::info!("parallel: skipping completed agent '{}'", agent_label);
            restore_step(state, &agent_step_key, iteration);
            skipped_count += 1;
            continue;
        }

        // Determine schema for this call: per-call override > block-level
        let call_schema = node
            .call_outputs
            .get(&i.to_string())
            .map(|name| resolve_schema(state, name))
            .transpose()?;
        let effective_schema = call_schema.as_ref().or(block_schema.as_ref()).cloned();

        // Combine block-level `with` + per-call `with` additions
        let mut effective_with = node.with.clone();
        if let Some(extra) = node.call_with.get(&i.to_string()) {
            effective_with.extend(extra.iter().cloned());
        }

        call_inputs.push((i, agent_step_key.clone(), effective_schema, effective_with));
    }

    // Second pass: execute each non-skipped agent synchronously via action registry
    // Note: in the full implementation these would be parallel; here we serialize for simplicity
    // The conductor-core parallel executor handles true parallelism via headless subprocesses.
    let mut results: Vec<ParallelCallResult> = Vec::new();
    let mut merged_markers: Vec<String> = Vec::new();
    let mut successes = 0u32;
    let mut failures = 0u32;

    // Parallel-scope token: child of the run root. Cancelling it prevents later branches
    // from executing when fail_fast fires.
    let scope_token = state.cancellation.child();

    for (i, _agent_step_key, call_schema, effective_with) in call_inputs {
        // Check scope token before dispatching each branch (fail_fast from a prior branch).
        if scope_token.is_cancelled() {
            tracing::info!(
                "parallel: scope token cancelled (fail_fast), skipping remaining branches"
            );
            break;
        }

        let pos = pos_base + i as i64;
        let agent_ref = &node.calls[i];
        let agent_label = agent_ref.label();

        // Check per-call `if` condition
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
                let step_id = state
                    .persistence
                    .insert_step(NewStep {
                        workflow_run_id: state.workflow_run_id.clone(),
                        step_name: agent_label.to_string(),
                        role: "actor".to_string(),
                        can_commit: false,
                        position: pos,
                        iteration: iteration as i64,
                        retry_count: None,
                    })
                    .map_err(|e| EngineError::Persistence(e.to_string()))?;
                state
                    .persistence
                    .update_step(
                        &step_id,
                        StepUpdate {
                            status: WorkflowStepStatus::Skipped,
                            child_run_id: None,
                            result_text: Some(format!(
                                "skipped: {cond_step}.{cond_marker} not emitted"
                            )),
                            context_out: None,
                            markers_out: None,
                            retry_count: None,
                            structured_output: None,
                            step_error: None,
                        },
                    )
                    .map_err(|e| EngineError::Persistence(e.to_string()))?;
                skipped_count += 1;
                continue;
            }
        }

        let step_id = state
            .persistence
            .insert_step(NewStep {
                workflow_run_id: state.workflow_run_id.clone(),
                step_name: agent_label.to_string(),
                role: "actor".to_string(),
                can_commit: false,
                position: pos,
                iteration: iteration as i64,
                retry_count: Some(0),
            })
            .map_err(|e| EngineError::Persistence(e.to_string()))?;

        let inputs: std::collections::HashMap<String, String> = {
            let var_map = crate::prompt_builder::build_variable_map(state);
            var_map
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect()
        };

        let ectx = ExecutionContext {
            run_id: step_id.clone(),
            working_dir: std::path::PathBuf::from(&state.worktree_ctx.working_dir),
            repo_path: state.worktree_ctx.repo_path.clone(),
            step_timeout: state.exec_config.step_timeout,
            shutdown: state.exec_config.shutdown.clone(),
            model: state.model.clone(),
            bot_name: state.default_bot_name.clone(),
            plugin_dirs: state.worktree_ctx.extra_plugin_dirs.clone(),
            workflow_name: state.workflow_name.clone(),
            worktree_id: state.worktree_ctx.worktree_id.clone(),
            parent_run_id: state.parent_run_id.clone(),
            step_id: step_id.clone(),
        };

        let params = ActionParams {
            name: agent_label.to_string(),
            inputs,
            retries_remaining: 0,
            retry_error: None,
            snippets: effective_with,
            dry_run: state.exec_config.dry_run,
            gate_feedback: state.last_gate_feedback.clone(),
            schema: call_schema.clone(),
        };

        let registry = Arc::clone(&state.action_registry);
        let result = registry.dispatch(&params.name, &ectx, &params);

        // If fail_fast and this branch failed, cancel the scope token to stop remaining branches.
        let failed = result.is_err();
        results.push(ParallelCallResult {
            agent_name: agent_label.to_string(),
            step_id,
            result,
            attempt: 0,
        });
        if failed && node.fail_fast {
            scope_token.cancel(CancellationReason::FailFast);
        }
    }

    // Process results
    for pr in &results {
        match &pr.result {
            Ok(output) => {
                let markers_json = serde_json::to_string(&output.markers).unwrap_or_default();
                let context = output.context.clone().unwrap_or_default();

                state
                    .persistence
                    .update_step(
                        &pr.step_id,
                        StepUpdate {
                            status: WorkflowStepStatus::Completed,
                            child_run_id: output.child_run_id.clone(),
                            result_text: output.result_text.clone(),
                            context_out: Some(context.clone()),
                            markers_out: Some(markers_json),
                            retry_count: Some(pr.attempt as i64),
                            structured_output: output.structured_output.clone(),
                            step_error: None,
                        },
                    )
                    .map_err(|e| EngineError::Persistence(e.to_string()))?;

                tracing::info!(
                    "parallel: '{}' completed (cost=${:.4})",
                    pr.agent_name,
                    output.cost_usd.unwrap_or(0.0),
                );

                successes += 1;
                merged_markers.extend(output.markers.iter().cloned());

                // Push parallel agent context
                state.contexts.push(ContextEntry {
                    step: pr.agent_name.clone(),
                    iteration,
                    context: context.clone(),
                    markers: output.markers.clone(),
                    structured_output: output.structured_output.clone(),
                    output_file: None,
                });

                state.accumulate_metrics(
                    output.cost_usd,
                    output.num_turns,
                    output.duration_ms,
                    output.input_tokens,
                    output.output_tokens,
                    output.cache_read_input_tokens,
                    output.cache_creation_input_tokens,
                );

                if let Err(e) = state.flush_metrics() {
                    tracing::warn!("Failed to flush mid-run metrics after parallel agent: {e}");
                }
            }
            Err(e) => {
                tracing::warn!("parallel: '{}' failed: {e}", pr.agent_name);
                state
                    .persistence
                    .update_step(
                        &pr.step_id,
                        StepUpdate {
                            status: WorkflowStepStatus::Failed,
                            child_run_id: None,
                            result_text: Some(e.to_string()),
                            context_out: None,
                            markers_out: None,
                            retry_count: Some(pr.attempt as i64),
                            structured_output: None,
                            step_error: Some(e.to_string()),
                        },
                    )
                    .map_err(|e2| EngineError::Persistence(e2.to_string()))?;
                failures += 1;
            }
        }
    }

    // Apply min_success policy (skipped-on-resume agents count as successes)
    let effective_successes = successes + skipped_count;
    let total_agents = results.len() as u32 + skipped_count;
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
