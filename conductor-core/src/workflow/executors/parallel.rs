use crate::agent::AgentRunStatus;
use crate::agent_config::AgentSpec;
use crate::error::Result;
use crate::workflow_dsl::ParallelNode;
use std::collections::HashSet;
use std::process::Command;
use std::thread;

use crate::workflow::engine::{resolve_schema, restore_step, should_skip, ExecutionState};
use crate::workflow::helpers::sanitize_tmux_name;
use crate::workflow::output::{interpret_agent_output, parse_conductor_output};
use crate::workflow::prompt_builder::build_agent_prompt;
use crate::workflow::status::WorkflowStepStatus;
use crate::workflow::types::ContextEntry;

pub fn execute_parallel(
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

        let pending_ids: Vec<&str> = children
            .iter()
            .enumerate()
            .filter(|(i, _)| !completed.contains(i))
            .map(|(_, c)| c.child_run_id.as_str())
            .collect();
        let run_map = state.agent_mgr.get_runs_by_ids(&pending_ids).unwrap_or_default();

        for (i, child) in children.iter().enumerate() {
            if completed.contains(&i) {
                continue;
            }
            if let Some(run) = run_map.get(&child.child_run_id) {
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
