use crate::agent::AgentManager;
use crate::agent_config::AgentSpec;
use crate::error::{ConductorError, Result};
use crate::workflow_dsl::CallNode;

use crate::workflow::action_executor::{ActionParams, ExecutionContext};
use crate::workflow::engine::{
    handle_on_fail, record_step_success, resolve_schema, restore_step, should_skip, ExecutionState,
};
use crate::workflow::manager::WorkflowManager;
use crate::workflow::prompt_builder::{build_agent_prompt, build_variable_map};
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

        // Dispatch through the ActionRegistry.
        //
        // Build the variable map once per attempt — gate_feedback and other
        // state fields may have changed on retry. Convert owned so the borrow
        // on `state` ends before we use `state` again below.
        let inputs: std::collections::HashMap<String, String> = {
            let var_map = build_variable_map(state);
            var_map
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect()
        };

        let ectx = ExecutionContext {
            run_id: child_run.id.clone(),
            working_dir: std::path::PathBuf::from(&working_dir),
            repo_path: repo_path.clone(),
            db_path: crate::config::db_path(),
            step_timeout: state.exec_config.step_timeout,
            shutdown: state.exec_config.shutdown.clone(),
            model: step_model.map(String::from),
            bot_name: effective_bot_name.map(String::from),
            plugin_dirs: merged_plugin_dirs.clone(),
            workflow_name: state.workflow_name.clone(),
        };

        let params = ActionParams {
            name: node.agent.label().to_string(),
            inputs,
            retries_remaining: max_attempts - attempt - 1,
            retry_error: if attempt == 0 {
                None
            } else {
                Some(last_error.clone())
            },
            snippets: if snippet_text.is_empty() {
                vec![]
            } else {
                vec![snippet_text.clone()]
            },
            dry_run: state.exec_config.dry_run,
            gate_feedback: state.last_gate_feedback.clone(),
            schema: schema.clone(),
        };

        // Clone the Arc before dispatch so we hold no borrow on `state` while
        // the executor runs (the executor may need the DB independently).
        let registry = std::sync::Arc::clone(&state.action_registry);
        match registry.dispatch(&params.name, &ectx, &params) {
            Ok(output) => {
                let markers_json = serde_json::to_string(&output.markers).unwrap_or_default();
                let context = output.context.unwrap_or_default();

                tracing::info!(
                    "Step '{}' completed: cost=${:.4}, {} turns, markers={:?}",
                    agent_label,
                    output.cost_usd.unwrap_or(0.0),
                    output.num_turns.unwrap_or(0),
                    output.markers,
                );

                state.wf_mgr.update_step_status_full(
                    &step_id,
                    WorkflowStepStatus::Completed,
                    Some(&child_run.id),
                    output.result_text.as_deref(),
                    Some(&context),
                    Some(&markers_json),
                    Some(attempt as i64),
                    output.structured_output.as_deref(),
                    None,
                )?;

                record_step_success(
                    state,
                    step_key.clone(),
                    agent_label,
                    output.result_text,
                    output.cost_usd,
                    output.num_turns,
                    output.duration_ms,
                    output.input_tokens,
                    output.output_tokens,
                    output.cache_read_input_tokens,
                    output.cache_creation_input_tokens,
                    output.markers,
                    context,
                    Some(child_run.id),
                    iteration,
                    output.structured_output,
                    None,
                );

                return Ok(());
            }
            Err(ConductorError::WorkflowCancelled) => {
                state.wf_mgr.update_step_status(
                    &step_id,
                    WorkflowStepStatus::Failed,
                    Some(&child_run.id),
                    Some("executor shutdown requested"),
                    None,
                    None,
                    Some(attempt as i64),
                )?;
                return Err(ConductorError::WorkflowCancelled);
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
