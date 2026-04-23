use crate::agent::AgentManager;
use crate::agent::AgentRunStatus;
use crate::agent_config::AgentSpec;
use crate::error::Result;
use crate::workflow::action_executor::{ActionOutput, ActionParams, ExecutionContext};
use crate::workflow::engine::{resolve_schema, restore_step, should_skip, ExecutionState};
use crate::workflow::output::{interpret_agent_output, parse_conductor_output};
use crate::workflow::prompt_builder::{build_agent_prompt, build_variable_map};
use crate::workflow::run_context::{RunContext, WorktreeRunContext};
use crate::workflow::status::WorkflowStepStatus;
use crate::workflow::types::ContextEntry;
use crate::workflow::WorkflowManager;
use crate::workflow_dsl::ParallelNode;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn cancel_child(
    agent_mgr: &AgentManager<'_>,
    wf_mgr: &WorkflowManager<'_>,
    run_id: &str,
    step_id: &str,
    agent_name: &str,
    thread_shutdown: &Arc<AtomicBool>,
    reason: &str,
) {
    thread_shutdown.store(true, Ordering::Relaxed);
    if let Err(e) = agent_mgr.update_run_cancelled(run_id) {
        tracing::warn!("parallel: failed to cancel run for '{agent_name}': {e}");
    }
    if let Err(e) = wf_mgr.update_step_status(
        step_id,
        WorkflowStepStatus::Failed,
        Some(run_id),
        Some(reason),
        None,
        None,
        None,
    ) {
        tracing::warn!("parallel: failed to update step for '{agent_name}': {e}");
    }
}

fn mark_child_failed(
    agent_mgr: &AgentManager<'_>,
    wf_mgr: &WorkflowManager<'_>,
    run_id: &str,
    step_id: &str,
    agent_name: &str,
    reason: &str,
) {
    if let Err(e) = agent_mgr.update_run_failed_if_running(run_id, reason) {
        tracing::warn!("parallel: failed to mark run failed for '{agent_name}': {e}");
    }
    if let Err(e) = wf_mgr.update_step_status(
        step_id,
        WorkflowStepStatus::Failed,
        Some(run_id),
        Some(reason),
        None,
        None,
        None,
    ) {
        tracing::warn!("parallel: failed to update step for '{agent_name}': {e}");
    }
}

pub fn execute_parallel(
    state: &mut ExecutionState<'_>,
    node: &ParallelNode,
    iteration: u32,
) -> Result<()> {
    let extra_plugin_dirs = state.worktree_ctx.extra_plugin_dirs.clone();
    let (working_dir, repo_path, worktree_id) = {
        let ctx = WorktreeRunContext::new(state);
        (
            ctx.working_dir().to_path_buf(),
            ctx.repo_path().to_path_buf(),
            ctx.worktree_id().map(String::from),
        )
    };
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

    // Build the variable map once — all parallel agents share the same substitution context.
    let inputs: std::collections::HashMap<String, String> = {
        let var_map = build_variable_map(state);
        var_map
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect()
    };

    // Clone the registry so we can dispatch from worker threads.
    let registry = Arc::clone(&state.action_registry);

    struct ParallelChild {
        agent_name: String,
        child_run_id: String,
        step_id: String,
        dispatch_handle: std::thread::JoinHandle<()>,
        thread_shutdown: Arc<AtomicBool>,
        /// Resolved schema for this child (computed at spawn time).
        schema: Option<crate::schema_config::OutputSchema>,
    }

    // Completion channel: dispatch threads signal (child_index, Result) when done.
    let (completion_tx, completion_rx) =
        std::sync::mpsc::channel::<(usize, std::result::Result<ActionOutput, String>)>();

    let mut children: Vec<ParallelChild> = Vec::new();
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
            working_dir.to_str().unwrap_or(""),
            repo_path.to_str().unwrap_or(""),
            &AgentSpec::from(agent_ref),
            Some(&state.workflow_name),
            &extra_plugin_dirs,
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
            working_dir.to_str().unwrap_or(""),
            repo_path.to_str().unwrap_or(""),
            &effective_with,
            Some(&state.workflow_name),
        )?;

        let prompt = build_agent_prompt(state, &agent_def, effective_schema, &snippet_text, None);
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

        let child_run = state.agent_mgr.create_child_run(
            worktree_id.as_deref(),
            &prompt,
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

        // Per-thread cancellation flag: set by the main polling loop on timeout,
        // fail_fast, or global shutdown. The executor's `poll` checks this flag
        // and returns `PollError::Cancelled`, which the executor converts to an
        // error that we receive through the completion channel.
        let thread_shutdown = Arc::new(AtomicBool::new(false));

        let ectx = ExecutionContext {
            run_id: child_run.id.clone(),
            working_dir: working_dir.clone(),
            repo_path: repo_path.to_string_lossy().to_string(),
            db_path: crate::config::db_path(),
            step_timeout: state.exec_config.step_timeout,
            shutdown: Some(Arc::clone(&thread_shutdown)),
            model: step_model.map(String::from),
            bot_name: state.default_bot_name.clone(),
            plugin_dirs: extra_plugin_dirs.clone(),
            workflow_name: state.workflow_name.clone(),
        };

        let params = ActionParams {
            name: agent_label.to_string(),
            inputs: inputs.clone(),
            retries_remaining: 0,
            retry_error: None,
            snippets: if snippet_text.is_empty() {
                vec![]
            } else {
                vec![snippet_text.clone()]
            },
            dry_run: state.exec_config.dry_run,
            gate_feedback: state.last_gate_feedback.clone(),
            schema: call_schema.clone().or_else(|| block_schema.clone()),
        };

        let registry_clone = Arc::clone(&registry);
        let outcome_tx = completion_tx.clone();
        let child_index = children.len();
        let dispatch_handle = std::thread::spawn(move || {
            let result = registry_clone
                .dispatch(&params.name, &ectx, &params)
                .map_err(|e| e.to_string());
            let _ = outcome_tx.send((child_index, result));
        });

        children.push(ParallelChild {
            agent_name: agent_label.to_string(),
            child_run_id: child_run.id,
            step_id,
            dispatch_handle,
            thread_shutdown,
            schema: call_schema.or_else(|| block_schema.clone()),
        });
    }

    // Drop our own sender so the channel disconnects once all dispatch threads finish.
    drop(completion_tx);

    // Capture count before polling loop (needed for min_success after join)
    let children_count = children.len() as u32;

    // Poll all children until completion, using channel signals from dispatch threads.
    let start = std::time::Instant::now();
    let mut completed: HashSet<usize> = HashSet::new();
    let mut successes = 0u32;
    let mut failures = 0u32;
    let mut merged_markers: Vec<String> = Vec::new();

    loop {
        if completed.len() == children.len() {
            break;
        }

        // Check global shutdown flag
        if let Some(ref flag) = state.exec_config.shutdown {
            if flag.load(Ordering::Relaxed) {
                tracing::warn!("parallel: shutdown requested, cancelling remaining agents");
                for (i, child) in children.iter().enumerate() {
                    if !completed.contains(&i) {
                        cancel_child(
                            &state.agent_mgr,
                            &state.wf_mgr,
                            &child.child_run_id,
                            &child.step_id,
                            &child.agent_name,
                            &child.thread_shutdown,
                            "cancelled: executor shutdown",
                        );
                        completed.insert(i);
                        failures += 1;
                    }
                }
                break;
            }
        }

        if start.elapsed() > state.exec_config.step_timeout {
            tracing::warn!("parallel: timeout reached");
            for (i, child) in children.iter().enumerate() {
                if !completed.contains(&i) {
                    cancel_child(
                        &state.agent_mgr,
                        &state.wf_mgr,
                        &child.child_run_id,
                        &child.step_id,
                        &child.agent_name,
                        &child.thread_shutdown,
                        "timed out",
                    );
                    failures += 1;
                    completed.insert(i);
                }
            }
            break;
        }

        // Wait for the next dispatch-thread completion signal (up to poll_interval)
        match completion_rx.recv_timeout(state.exec_config.poll_interval) {
            Ok((child_idx, dispatch_result)) => {
                if completed.contains(&child_idx) {
                    // Already processed (e.g., cancelled by fail_fast or timeout)
                    continue;
                }
                let child = &children[child_idx];

                // Targeted DB lookup for just this run (metrics, status confirmation)
                let run = match state.agent_mgr.get_run(&child.child_run_id) {
                    Ok(Some(r)) => r,
                    Ok(None) => {
                        tracing::warn!(
                            "parallel: run '{}' not found in DB after dispatch",
                            child.child_run_id
                        );
                        completed.insert(child_idx);
                        failures += 1;
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!("parallel: DB error for '{}': {e}", child.agent_name);
                        completed.insert(child_idx);
                        failures += 1;
                        continue;
                    }
                };

                // Guard: dispatch signalled completion but the DB row is still in a
                // transient state (race between channel send and DB commit).  Force the run
                // to failed so it can never remain permanently stuck in Running.
                if matches!(
                    run.status,
                    AgentRunStatus::Running | AgentRunStatus::WaitingForFeedback
                ) {
                    let fail_msg = "drain completed without result";
                    tracing::warn!(
                        "parallel: '{}' still in {:?} after dispatch — applying race guard",
                        child.agent_name,
                        run.status,
                    );
                    mark_child_failed(
                        &state.agent_mgr,
                        &state.wf_mgr,
                        &child.child_run_id,
                        &child.step_id,
                        &child.agent_name,
                        fail_msg,
                    );
                    completed.insert(child_idx);
                    failures += 1;
                    continue;
                }

                let succeeded = matches!(
                    (&dispatch_result, &run.status),
                    (Ok(_), AgentRunStatus::Completed)
                );

                // In parallel blocks, schema validation failures fall back
                // to generic parsing (no retry mechanism for individual calls).
                let (markers, context, structured_json) = if succeeded {
                    // Use the ActionOutput from the dispatch thread when available.
                    match dispatch_result {
                        Ok(ref output) => {
                            let ctx = output.context.clone().unwrap_or_default();
                            let structured = output.structured_output.clone();
                            (output.markers.clone(), ctx, structured)
                        }
                        Err(_) => {
                            // Shouldn't happen because we only enter this branch on Ok,
                            // but handle defensively.
                            interpret_agent_output(
                                run.result_text.as_deref(),
                                child.schema.as_ref(),
                                true,
                            )
                            .unwrap_or_default()
                        }
                    }
                } else {
                    interpret_agent_output(run.result_text.as_deref(), child.schema.as_ref(), false)
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
                        })
                };

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
                    None,
                ) {
                    tracing::warn!(
                        "parallel: failed to update step status for '{}': {e}",
                        child.agent_name
                    );
                }

                state.accumulate_agent_run(&run);

                // Best-effort mid-run metrics flush after each parallel agent
                if let Err(e) = state.flush_metrics() {
                    tracing::warn!("Failed to flush mid-run metrics after parallel agent: {e}");
                }

                tracing::info!(
                    "parallel: '{}' {} (cost=${:.4})",
                    child.agent_name,
                    if succeeded { "completed" } else { "failed" },
                    run.cost_usd.unwrap_or(0.0),
                );

                completed.insert(child_idx);

                // fail_fast: cancel remaining on first failure
                if !succeeded && node.fail_fast {
                    tracing::warn!("parallel: fail_fast — cancelling remaining");
                    for (j, other) in children.iter().enumerate() {
                        if !completed.contains(&j) {
                            cancel_child(
                                &state.agent_mgr,
                                &state.wf_mgr,
                                &other.child_run_id,
                                &other.step_id,
                                &other.agent_name,
                                &other.thread_shutdown,
                                "cancelled by fail_fast",
                            );
                            completed.insert(j);
                            failures += 1;
                        }
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // No completion within poll_interval — loop back and check shutdown/timeout
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // All dispatch threads finished and dropped their senders
                break;
            }
        }
    }

    // Join all dispatch thread handles (best-effort; prevents zombie threads).
    for child in children {
        if let Err(e) = child.dispatch_handle.join() {
            tracing::warn!(
                "parallel: dispatch thread for '{}' panicked: {e:?}",
                child.agent_name
            );
            // The dispatch thread panicked before it could update the DB.
            // Mark the run and step failed so the workflow doesn't hang and
            // min_success accounting is correct.  `update_run_failed_if_running`
            // guards against overwriting a run that was already finalized.
            let fail_msg = "dispatch thread panicked";
            mark_child_failed(
                &state.agent_mgr,
                &state.wf_mgr,
                &child.child_run_id,
                &child.step_id,
                &child.agent_name,
                fail_msg,
            );
            failures += 1;
        }
    }

    // Apply min_success policy (skipped-on-resume agents count as successes)
    let effective_successes = successes + skipped_count;
    let total_agents = children_count + skipped_count;
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
