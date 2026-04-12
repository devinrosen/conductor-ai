use crate::agent::AgentRunStatus;
use crate::agent_config::AgentSpec;
use crate::error::{ConductorError, Result};
use crate::workflow_dsl::CallNode;

use crate::workflow::engine::{
    record_step_failure, record_step_success, resolve_schema, restore_step, run_on_fail_agent,
    should_skip, ExecutionState,
};
use crate::workflow::output::interpret_agent_output;
use crate::workflow::prompt_builder::build_agent_prompt;
use crate::workflow::status::WorkflowStepStatus;

pub fn execute_call(state: &mut ExecutionState<'_>, node: &CallNode, iteration: u32) -> Result<()> {
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

    // Merge per-step plugin_dirs (from .wf) with repo-level extra_plugin_dirs.
    let mut merged_plugin_dirs = state.extra_plugin_dirs.clone();
    for dir in &node.plugin_dirs {
        if !merged_plugin_dirs.contains(dir) {
            merged_plugin_dirs.push(dir.clone());
        }
    }

    // Load agent definition
    let agent_def = crate::agent_config::load_agent(
        &state.working_dir,
        &state.repo_path,
        &AgentSpec::from(&node.agent),
        Some(&state.workflow_name),
        &merged_plugin_dirs,
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

        let effective_bot_name = node
            .bot_name
            .as_deref()
            .or(state.default_bot_name.as_deref());
        let child_run = state.agent_mgr.create_child_run(
            state.worktree_id.as_deref(),
            &prompt,
            None,
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
            "Step '{}' (attempt {}/{}): spawning headless",
            agent_label,
            attempt + 1,
            max_attempts,
        );

        // Build args and spawn headless subprocess
        let (handle, prompt_file) = match crate::agent_runtime::try_spawn_headless_run(
            &child_run.id,
            &state.working_dir,
            &prompt,
            None,
            step_model,
            effective_bot_name,
            Some(&state.config.general.agent_permission_mode),
            &merged_plugin_dirs,
        ) {
            Ok(pair) => pair,
            Err(err_msg) => {
                tracing::warn!("Step '{}': {err_msg}", agent_label);
                if let Err(e) = state.agent_mgr.update_run_failed(&child_run.id, &err_msg) {
                    tracing::warn!(
                        "Step '{}': failed to mark run failed in DB: {e}",
                        agent_label
                    );
                }
                state.wf_mgr.update_step_status(
                    &step_id,
                    WorkflowStepStatus::Failed,
                    Some(&child_run.id),
                    Some(&err_msg),
                    None,
                    None,
                    Some(attempt as i64),
                )?;
                last_error = err_msg;
                continue;
            }
        };

        let pid = handle.pid;
        if let Err(e) = state
            .agent_mgr
            .update_run_subprocess_pid(&child_run.id, pid)
        {
            tracing::warn!("Failed to persist subprocess pid: {e}");
        }

        // Spawn drain thread — opens its own DB connection (Connection is not Send)
        let run_id_clone = child_run.id.clone();
        let log_path = crate::config::agent_log_path(&child_run.id);
        let (tx, rx) = std::sync::mpsc::channel::<crate::agent_runtime::DrainOutcome>();

        // Drain subprocess stderr on a dedicated thread.
        //
        // The subprocess (`conductor agent run`) is spawned with stderr piped
        // (spawn_headless uses Stdio::piped for both stdout and stderr).  Inside
        // the subprocess, `claude --verbose` inherits that pipe and writes many KB
        // of human-readable output to it.  If nobody reads the pipe the kernel
        // buffer fills (~64 KB on macOS) and the write blocks — freezing the
        // claude subprocess and stalling stdout too.  Drain here to keep the pipe
        // flowing.  Output is intentionally discarded: forwarding to our own
        // stderr corrupts the TUI (which owns the terminal in raw mode), and the
        // subprocess already writes the useful stream-json events to its stdout.
        let stderr_pipe = handle.stderr;
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            let reader = BufReader::new(stderr_pipe);
            for line in reader.lines().map_while(|l| l.ok()) {
                tracing::trace!(target: "conductor::agent::stderr", "{line}");
            }
        });

        std::thread::spawn(move || {
            let conn = match crate::db::open_database(&crate::config::db_path()) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("drain thread: failed to open DB: {e}");
                    let _ = std::fs::remove_file(&prompt_file);
                    let _ = tx.send(crate::agent_runtime::DrainOutcome::NoResult);
                    return;
                }
            };
            let mgr = crate::agent::AgentManager::new(&conn);
            let outcome = crate::agent_runtime::drain_stream_json(
                handle.stdout,
                &run_id_clone,
                &log_path,
                &mgr,
                |_| {},
            );
            let _ = std::fs::remove_file(&prompt_file);
            let _ = {
                let mut c = handle.child;
                c.wait()
            };
            let _ = tx.send(outcome);
        });

        // Wait for drain thread with periodic shutdown/timeout checks
        let start = std::time::Instant::now();
        let drain_outcome = loop {
            match rx.recv_timeout(std::time::Duration::from_secs(1)) {
                Ok(outcome) => break outcome,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Check shutdown flag
                    if let Some(ref flag) = state.exec_config.shutdown {
                        if flag.load(std::sync::atomic::Ordering::Relaxed) {
                            tracing::warn!(
                                "Step '{}': shutdown requested, cancelling",
                                agent_label
                            );
                            // Mark cancelled BEFORE sending SIGTERM (RFC 016 Q2)
                            let _ = state.agent_mgr.update_run_cancelled(&child_run.id);
                            crate::process_utils::cancel_subprocess(pid);
                            // Drain the channel for final outcome (best-effort)
                            let _ = rx.recv_timeout(std::time::Duration::from_secs(6));
                            let cancel_msg = "executor shutdown requested".to_string();
                            state.wf_mgr.update_step_status(
                                &step_id,
                                WorkflowStepStatus::Failed,
                                Some(&child_run.id),
                                Some(&cancel_msg),
                                None,
                                None,
                                Some(attempt as i64),
                            )?;
                            return Err(ConductorError::Workflow(cancel_msg));
                        }
                    }
                    // Check step timeout
                    if start.elapsed() > state.exec_config.step_timeout {
                        tracing::warn!("Step '{}': timeout reached, cancelling", agent_label);
                        // Mark cancelled BEFORE sending SIGTERM (RFC 016 Q2)
                        let _ = state.agent_mgr.update_run_cancelled(&child_run.id);
                        crate::process_utils::cancel_subprocess(pid);
                        let _ = rx.recv_timeout(std::time::Duration::from_secs(6));
                        break crate::agent_runtime::DrainOutcome::NoResult;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    // Drain thread panicked
                    tracing::warn!(
                        "Step '{}': drain thread disconnected unexpectedly",
                        agent_label
                    );
                    break crate::agent_runtime::DrainOutcome::NoResult;
                }
            }
        };

        match drain_outcome {
            crate::agent_runtime::DrainOutcome::Completed => {
                // Re-read run from DB for final status and metrics
                let completed_run = match state.agent_mgr.get_run(&child_run.id) {
                    Ok(Some(r)) => r,
                    Ok(None) => {
                        last_error = format!("run {} not found after drain", child_run.id);
                        continue;
                    }
                    Err(e) => {
                        last_error = format!("DB error after drain: {e}");
                        continue;
                    }
                };
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
                        completed_run.input_tokens,
                        completed_run.output_tokens,
                        completed_run.cache_read_input_tokens,
                        completed_run.cache_creation_input_tokens,
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
            crate::agent_runtime::DrainOutcome::NoResult => {
                // Subprocess exited without a result event (timeout or crash)
                tracing::warn!(
                    "Step '{}' (attempt {}/{}): no result event from drain",
                    agent_label,
                    attempt + 1,
                    max_attempts,
                );
                // Ensure the run is marked failed if not already cancelled
                if let Err(e) = state
                    .agent_mgr
                    .update_run_failed_if_running(&child_run.id, "agent exited without result")
                {
                    tracing::warn!("Step '{}': failed to mark run as failed: {e}", agent_label);
                }
                state.wf_mgr.update_step_status(
                    &step_id,
                    WorkflowStepStatus::Failed,
                    Some(&child_run.id),
                    Some("agent exited without result"),
                    None,
                    None,
                    Some(attempt as i64),
                )?;
                last_error = "agent exited without result".to_string();
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
