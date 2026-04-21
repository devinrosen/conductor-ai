use crate::agent::{AgentManager, AgentRunStatus};
use crate::agent_config::AgentSpec;
use crate::error::{ConductorError, Result};
use crate::workflow_dsl::CallNode;

use crate::workflow::engine::{
    handle_on_fail, record_step_success, resolve_schema, restore_step, should_skip, ExecutionState,
};
use crate::workflow::manager::WorkflowManager;
use crate::workflow::output::interpret_agent_output;
use crate::workflow::prompt_builder::build_agent_prompt;
use crate::workflow::run_context::RunContext;
use crate::workflow::status::WorkflowStepStatus;

/// Mark a single retry attempt as failed in the DB and advance the step status.
///
/// Does not set `last_error` or `continue` — callers do that so the distinct
/// warning message and retry logic remain visible at the call site.
///
/// Takes the managers separately so Rust can see that only specific fields of
/// `ExecutionState` are borrowed, leaving sibling fields (e.g. `model`) free
/// for the caller's own borrows.
fn fail_attempt(
    agent_mgr: &AgentManager<'_>,
    wf_mgr: &WorkflowManager<'_>,
    step_id: &str,
    run_id: &str,
    err_msg: &str,
    attempt: u32,
    agent_label: &str,
) -> Result<()> {
    if let Err(db_e) = agent_mgr.update_run_failed(run_id, err_msg) {
        tracing::warn!(
            "Step '{}': failed to mark run failed in DB: {db_e}",
            agent_label
        );
    }
    wf_mgr.update_step_status(
        step_id,
        WorkflowStepStatus::Failed,
        Some(run_id),
        Some(err_msg),
        None,
        None,
        Some(attempt as i64),
    )
}

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

    let (working_dir, repo_path, extra_plugin_dirs, worktree_id) = {
        let ctx = crate::workflow::run_context::WorktreeRunContext::new(state);
        let working_dir = ctx.working_dir_str();
        let repo_path = ctx.repo_path_str();
        let extra_plugin_dirs = ctx.extra_plugin_dirs().to_vec();
        let worktree_id: Option<String> = ctx.worktree_id().map(|s| s.to_string());
        (working_dir, repo_path, extra_plugin_dirs, worktree_id)
    };

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
    let mut merged_plugin_dirs = extra_plugin_dirs.clone();
    for dir in &node.plugin_dirs {
        if !merged_plugin_dirs.contains(dir) {
            merged_plugin_dirs.push(dir.clone());
        }
    }

    // Load agent definition
    let agent_def = crate::agent_config::load_agent(
        &working_dir,
        &repo_path,
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
        &working_dir,
        &repo_path,
        with_refs,
        Some(&state.workflow_name),
    )?;

    let step_model = agent_def.model.as_deref().or(state.model.as_deref());

    // Retry loop
    let max_attempts = 1 + node.retries;
    let mut last_error = String::new();

    for attempt in 0..max_attempts {
        // Rebuild prompt each attempt so we can inject the previous failure reason
        // on retries. On attempt 0 there is no prior error, so pass None.
        let retry_ctx = if attempt == 0 {
            None
        } else {
            Some(last_error.as_str())
        };
        let prompt =
            build_agent_prompt(state, &agent_def, schema.as_ref(), &snippet_text, retry_ctx);

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
            worktree_id.as_deref(),
            &prompt,
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

        // --- API-enforced path for schema-constrained steps ---
        // When a schema is defined and ANTHROPIC_API_KEY is available, route
        // directly to the Anthropic Messages API using tool_use enforcement.
        // This makes schema field mismatches impossible at the API level.
        if let Some(ref schema) = schema {
            if let Some(api_key) = state.config.anthropic_api_key() {
                // Check shutdown flag before making the API call
                if let Some(ref flag) = state.exec_config.shutdown {
                    if flag.load(std::sync::atomic::Ordering::Relaxed) {
                        let cancel_msg = "executor shutdown requested".to_string();
                        let _ = state
                            .agent_mgr
                            .update_run_failed(&child_run.id, &cancel_msg);
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

                let resolved_model = step_model.unwrap_or(super::api_call::DEFAULT_API_MODEL);
                tracing::info!(
                    "Step '{}' (attempt {}/{}): using direct API path (schema: {})",
                    agent_label,
                    attempt + 1,
                    max_attempts,
                    schema.name,
                );

                match super::api_call::execute_via_api(
                    &prompt,
                    schema,
                    resolved_model,
                    state.exec_config.step_timeout,
                    &api_key,
                ) {
                    Ok(result) => {
                        let structured =
                            crate::schema_config::derive_output_from_value(result.json, schema);
                        let markers_json =
                            serde_json::to_string(&structured.markers).unwrap_or_default();

                        if let Err(e) = state.agent_mgr.update_run_completed(
                            &child_run.id,
                            None,
                            Some(&result.json_string),
                            None,
                            Some(1),
                            None,
                            Some(result.input_tokens),
                            Some(result.output_tokens),
                            None,
                            None,
                        ) {
                            tracing::warn!(
                                "Step '{}': failed to mark API run completed in DB: {e}",
                                agent_label
                            );
                        }

                        tracing::info!(
                            "Step '{}' completed via API: {} input tokens, {} output tokens, markers={:?}",
                            agent_label,
                            result.input_tokens,
                            result.output_tokens,
                            structured.markers,
                        );

                        state.wf_mgr.update_step_status_full(
                            &step_id,
                            WorkflowStepStatus::Completed,
                            Some(&child_run.id),
                            Some(&result.json_string),
                            Some(&structured.context),
                            Some(&markers_json),
                            Some(attempt as i64),
                            Some(&structured.json_string),
                            None,
                        )?;

                        record_step_success(
                            state,
                            step_key.clone(),
                            agent_label,
                            Some(result.json_string),
                            None,
                            Some(1),
                            None,
                            Some(result.input_tokens),
                            Some(result.output_tokens),
                            None,
                            None,
                            structured.markers,
                            structured.context,
                            Some(child_run.id),
                            iteration,
                            Some(structured.json_string),
                            None,
                        );

                        return Ok(());
                    }
                    Err(err_msg) => {
                        tracing::warn!(
                            "Step '{}' API call failed (attempt {}/{}): {err_msg}",
                            agent_label,
                            attempt + 1,
                            max_attempts,
                        );
                        if let Err(e) = state.agent_mgr.update_run_failed(&child_run.id, &err_msg) {
                            tracing::warn!(
                                "Step '{}': failed to mark API run failed in DB: {e}",
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
                }
            }
        }

        // Resolve runtime for this agent and spawn via trait dispatch.
        let runtime = match crate::runtime::resolve_runtime(&agent_def.runtime, state.config) {
            Ok(rt) => rt,
            Err(e) => {
                let err_msg = e.to_string();
                tracing::warn!("Step '{}': {err_msg}", agent_label);
                fail_attempt(
                    &state.agent_mgr,
                    &state.wf_mgr,
                    &step_id,
                    &child_run.id,
                    &err_msg,
                    attempt,
                    agent_label,
                )?;
                last_error = err_msg;
                continue;
            }
        };

        let request = crate::runtime::RuntimeRequest {
            run_id: child_run.id.clone(),
            agent_def: agent_def.clone(),
            prompt: prompt.clone(),
            working_dir: std::path::PathBuf::from(&working_dir),
            model: step_model.map(String::from),
            bot_name: effective_bot_name.map(String::from),
            plugin_dirs: merged_plugin_dirs.clone(),
            db_path: crate::config::db_path(),
        };

        tracing::info!(
            "Step '{}' (attempt {}/{}): spawning via runtime '{}'",
            agent_label,
            attempt + 1,
            max_attempts,
            agent_def.runtime,
        );

        if let Err(e) = runtime.spawn(&request) {
            let err_msg = e.to_string();
            tracing::warn!("Step '{}': spawn failed: {err_msg}", agent_label);
            fail_attempt(
                &state.agent_mgr,
                &state.wf_mgr,
                &step_id,
                &child_run.id,
                &err_msg,
                attempt,
                agent_label,
            )?;
            last_error = err_msg;
            continue;
        }

        match runtime.poll(
            &child_run.id,
            state.exec_config.shutdown.as_ref(),
            state.exec_config.step_timeout,
            &request.db_path,
        ) {
            Err(crate::runtime::PollError::Cancelled) => {
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
            Err(e) => {
                let err_msg = e.to_string();
                tracing::warn!(
                    "Step '{}' (attempt {}/{}): {err_msg}",
                    agent_label,
                    attempt + 1,
                    max_attempts,
                );
                fail_attempt(
                    &state.agent_mgr,
                    &state.wf_mgr,
                    &step_id,
                    &child_run.id,
                    &err_msg,
                    attempt,
                    agent_label,
                )?;
                last_error = err_msg;
                continue;
            }
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
                        state.wf_mgr.update_step_status_full(
                            &step_id,
                            WorkflowStepStatus::Failed,
                            Some(&completed_run.id),
                            completed_run.result_text.as_deref(),
                            None,
                            None,
                            Some(attempt as i64),
                            None,
                            Some(&validation_err),
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
                        None,
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
        }
    }

    handle_on_fail(
        state,
        step_key,
        agent_label,
        &node.on_fail,
        last_error,
        node.retries,
        iteration,
        max_attempts,
    )
}
