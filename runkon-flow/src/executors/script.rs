use std::path::{Path, PathBuf};

use crate::dsl::ScriptNode;
use crate::engine::{
    record_step_failure, record_step_skipped, record_step_success, restore_step, should_skip,
    ExecutionState,
};
use crate::engine_error::Result;
use crate::prompt_builder::build_variable_map;
use crate::status::WorkflowStepStatus;
use crate::traits::persistence::{NewStep, StepUpdate};
use crate::traits::run_context::RunContext;

use super::p_err;

/// Create a named temp file that persists after the handle is dropped.
///
/// Logs a warning on any failure mode so the caller can see why capture is missing.
fn make_temp_file(step_name: &str, purpose: &str) -> Option<PathBuf> {
    match tempfile::NamedTempFile::new() {
        Ok(f) => {
            let path = f.path().to_path_buf();
            match f.keep() {
                Ok(_) => Some(path),
                Err(e) => {
                    tracing::warn!(
                        "script '{}': failed to persist temp {purpose} file: {e}",
                        step_name
                    );
                    None
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                "script '{}': failed to create temp {purpose} file: {e}",
                step_name
            );
            None
        }
    }
}

fn apply_script_on_fail(
    state: &mut ExecutionState,
    step_name: &str,
    on_fail: &Option<crate::dsl::OnFail>,
    err_msg: String,
) -> Result<()> {
    match on_fail {
        Some(crate::dsl::OnFail::Continue) => {
            record_step_skipped(state, step_name.to_string(), step_name);
            Ok(())
        }
        _ => record_step_failure(state, step_name.to_string(), step_name, err_msg, 1, true),
    }
}

pub fn execute_script(state: &mut ExecutionState, node: &ScriptNode, iteration: u32) -> Result<()> {
    let pos = state.position;
    state.position += 1;

    // Skip completed script steps on resume
    if should_skip(state, &node.name, iteration) {
        tracing::info!("Skipping completed script step '{}'", node.name);
        restore_step(state, &node.name, iteration);
        return Ok(());
    }

    let step_id = state
        .persistence
        .insert_step(NewStep {
            workflow_run_id: state.workflow_run_id.clone(),
            step_name: node.name.clone(),
            role: "script".to_string(),
            can_commit: false,
            position: pos,
            iteration: iteration as i64,
            retry_count: Some(0),
        })
        .map_err(p_err)?;

    if state.exec_config.dry_run {
        tracing::info!("script '{}': dry-run, skipping execution", node.name);
        state
            .persistence
            .update_step(
                &step_id,
                StepUpdate {
                    status: WorkflowStepStatus::Completed,
                    child_run_id: None,
                    result_text: Some("dry-run: script not executed".to_string()),
                    context_out: None,
                    markers_out: None,
                    retry_count: Some(0),
                    structured_output: None,
                    step_error: None,
                },
            )
            .map_err(p_err)?;

        record_step_success(
            state,
            node.name.clone(),
            &node.name,
            Some("dry-run: script not executed".to_string()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            vec![],
            String::new(),
            None,
            iteration,
            None,
            None,
        );
        return Ok(());
    }

    // Build variable map for substitution, shell-quoting all values to prevent
    // injection when they are interpolated into the sh -c command string.
    let vars = build_variable_map(state);
    let shell_safe_vars: std::collections::HashMap<&str, String> = vars
        .iter()
        .map(|(k, v)| (*k, crate::prompt_builder::shell_quote(v)))
        .collect();
    let script_cmd = crate::prompt_builder::substitute_variables(&node.run, &shell_safe_vars);

    tracing::info!("script '{}': executing command", node.name);

    // Build environment variables
    let mut env_vars: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    // Inject PATH and other env from the script env provider
    {
        struct ScriptRunCtx<'a> {
            working_dir: &'a str,
            repo_path: &'a str,
        }
        impl RunContext for ScriptRunCtx<'_> {
            fn injected_variables(&self) -> std::collections::HashMap<&'static str, String> {
                std::collections::HashMap::new()
            }
            fn working_dir(&self) -> &Path {
                Path::new(self.working_dir)
            }
            fn repo_path(&self) -> &Path {
                Path::new(self.repo_path)
            }
            fn worktree_id(&self) -> Option<&str> {
                None
            }
            fn ticket_id(&self) -> Option<&str> {
                None
            }
            fn repo_id(&self) -> Option<&str> {
                None
            }
        }
        let run_ctx = ScriptRunCtx {
            working_dir: &state.worktree_ctx.working_dir,
            repo_path: &state.worktree_ctx.repo_path,
        };
        let provider_env = state.script_env_provider.env(&run_ctx);
        env_vars.extend(provider_env);
    }

    // Inject all current workflow inputs as env vars (prefixed with CONDUCTOR_).
    // Validate that keys consist only of alphanumeric characters and underscores to
    // prevent malformed env var names if a key contains `=` or a null byte.
    for (k, v) in &state.inputs {
        if !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            tracing::warn!(
                "script '{}': input key {:?} contains characters invalid in an env var name, skipping",
                node.name,
                k
            );
            continue;
        }
        env_vars.insert(format!("CONDUCTOR_{}", k.to_uppercase()), v.clone());
    }

    // Inject explicit env vars from the workflow `env = { ... }` block.
    // Template variables (e.g. `{{prior_output}}`) are substituted using the raw
    // (non-shell-quoted) variable map because values are passed as discrete env
    // var values, not interpolated into a shell command string.
    const SENSITIVE_ENV_VARS: &[&str] = &[
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "PATH",
        "DYLD_INSERT_LIBRARIES",
        "PYTHONPATH",
        "RUBYLIB",
        "NODE_PATH",
    ];
    for (k, v) in &node.env {
        if k.contains('=') || k.contains('\0') {
            tracing::warn!(
                "script '{}': env key {:?} contains '=' or null byte, skipping",
                node.name,
                k
            );
            continue;
        }
        if SENSITIVE_ENV_VARS.contains(&k.as_str()) {
            tracing::warn!(
                "script '{}': env block overrides security-sensitive variable {:?}",
                node.name,
                k
            );
        }
        let resolved = crate::prompt_builder::substitute_variables(v, &vars);
        env_vars.insert(k.clone(), resolved);
    }

    // Execute the script
    let working_dir = &state.worktree_ctx.working_dir;
    let output_file = make_temp_file(&node.name, "stdout");
    // Redirect stderr to a temp file so it never leaks to the TUI terminal.
    let stderr_file = make_temp_file(&node.name, "stderr");

    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(&script_cmd);
    cmd.current_dir(working_dir);
    for (k, v) in &env_vars {
        cmd.env(k, v);
    }

    // Optionally capture stdout to output file
    if let Some(ref out_path) = output_file {
        match std::fs::File::create(out_path) {
            Ok(file) => {
                cmd.stdout(file);
            }
            Err(e) => {
                tracing::warn!(
                    "script '{}': failed to open stdout file for writing: {e}",
                    node.name
                );
            }
        }
    }

    // Redirect stderr to a temp file so it never leaks to the TUI terminal.
    if let Some(ref err_path) = stderr_file {
        match std::fs::File::create(err_path) {
            Ok(file) => {
                cmd.stderr(file);
            }
            Err(e) => {
                tracing::warn!(
                    "script '{}': failed to open stderr capture file: {e}",
                    node.name
                );
            }
        }
    }

    let start = std::time::Instant::now();
    let result = cmd.status();
    let duration_ms = start.elapsed().as_millis() as i64;

    match result {
        Ok(status) if status.success() => {
            tracing::info!(
                "script '{}': completed successfully in {}ms",
                node.name,
                duration_ms
            );

            // Clean up stderr capture file on success.
            if let Some(ref p) = stderr_file {
                let _ = std::fs::remove_file(p);
            }

            // Read stdout and parse the CONDUCTOR_OUTPUT block so markers like
            // `has_code_changes` are available to downstream `if` conditions.
            let stdout = output_file.as_ref().and_then(|p| {
                std::fs::read_to_string(p)
                    .map_err(|e| {
                        tracing::warn!("script '{}': failed to read stdout: {e}", node.name)
                    })
                    .ok()
            });
            let (markers, context) = stdout
                .as_deref()
                .and_then(crate::helpers::parse_conductor_output)
                .map(|out| (out.markers, out.context))
                .unwrap_or_else(|| {
                    let ctx = stdout.as_deref().unwrap_or("").chars().take(2000).collect();
                    (vec![], ctx)
                });

            let markers_json = crate::helpers::serialize_or_empty_array(
                &markers,
                &format!("script '{}'", node.name),
            );
            let output_file_path = output_file
                .as_ref()
                .map(|p| p.to_string_lossy().to_string());

            state
                .persistence
                .update_step(
                    &step_id,
                    StepUpdate {
                        status: WorkflowStepStatus::Completed,
                        child_run_id: None,
                        result_text: Some(format!("Script '{}' completed", node.name)),
                        context_out: Some(context.clone()),
                        markers_out: Some(markers_json),
                        retry_count: Some(0),
                        structured_output: None,
                        step_error: None,
                    },
                )
                .map_err(p_err)?;

            record_step_success(
                state,
                node.name.clone(),
                &node.name,
                Some(format!("Script '{}' completed", node.name)),
                None,
                None,
                Some(duration_ms),
                None,
                None,
                None,
                None,
                markers,
                context,
                None,
                iteration,
                None,
                output_file_path,
            );

            Ok(())
        }
        Ok(status) => {
            let exit_code = status.code().unwrap_or(-1);

            // Read captured stderr (up to 2 000 chars) to include in the error.
            let captured_stderr = stderr_file.as_ref().and_then(|p| {
                std::fs::read_to_string(p)
                    .map_err(|e| {
                        tracing::warn!(
                            "script '{}': failed to read captured stderr: {e}",
                            node.name
                        )
                    })
                    .ok()
                    .map(|s| s.trim().chars().take(2000).collect::<String>())
                    .filter(|s| !s.is_empty())
            });

            let err_msg = match &captured_stderr {
                Some(stderr) => format!(
                    "Script '{}' exited with code {}\n{}",
                    node.name, exit_code, stderr
                ),
                None => format!("Script '{}' exited with code {}", node.name, exit_code),
            };
            tracing::warn!("{}", err_msg);

            state
                .persistence
                .update_step(
                    &step_id,
                    StepUpdate {
                        status: WorkflowStepStatus::Failed,
                        child_run_id: None,
                        result_text: Some(err_msg.clone()),
                        context_out: None,
                        markers_out: None,
                        retry_count: Some(0),
                        structured_output: None,
                        step_error: Some(err_msg.clone()),
                    },
                )
                .map_err(p_err)?;

            // Clean up output file and stderr file on failure
            if let Some(ref p) = output_file {
                let _ = std::fs::remove_file(p);
            }
            if let Some(ref p) = stderr_file {
                let _ = std::fs::remove_file(p);
            }

            apply_script_on_fail(state, &node.name, &node.on_fail, err_msg)
        }
        Err(e) => {
            let err_msg = format!("Script '{}' failed to execute: {e}", node.name);
            tracing::warn!("{}", err_msg);

            state
                .persistence
                .update_step(
                    &step_id,
                    StepUpdate {
                        status: WorkflowStepStatus::Failed,
                        child_run_id: None,
                        result_text: Some(err_msg.clone()),
                        context_out: None,
                        markers_out: None,
                        retry_count: Some(0),
                        structured_output: None,
                        step_error: Some(err_msg.clone()),
                    },
                )
                .map_err(p_err)?;

            if let Some(ref p) = stderr_file {
                let _ = std::fs::remove_file(p);
            }

            apply_script_on_fail(state, &node.name, &node.on_fail, err_msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::ScriptNode;
    use crate::engine::{ExecutionState, WorktreeContext};
    use crate::persistence_memory::InMemoryWorkflowPersistence;
    use crate::traits::action_executor::ActionRegistry;
    use crate::traits::item_provider::ItemProviderRegistry;
    use crate::traits::persistence::{NewRun, WorkflowPersistence};
    use crate::traits::script_env_provider::NoOpScriptEnvProvider;
    use crate::types::WorkflowExecConfig;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn make_persistence() -> (Arc<InMemoryWorkflowPersistence>, String) {
        let p = Arc::new(InMemoryWorkflowPersistence::new());
        let run = p
            .create_run(NewRun {
                workflow_name: "wf".to_string(),
                worktree_id: None,
                ticket_id: None,
                repo_id: None,
                parent_run_id: String::new(),
                dry_run: false,
                trigger: "manual".to_string(),
                definition_snapshot: None,
                parent_workflow_run_id: None,
                target_label: None,
            })
            .unwrap();
        (p, run.id)
    }

    fn make_state(persistence: Arc<InMemoryWorkflowPersistence>, run_id: String) -> ExecutionState {
        ExecutionState {
            persistence,
            action_registry: Arc::new(ActionRegistry::new(HashMap::new(), None)),
            script_env_provider: Arc::new(NoOpScriptEnvProvider),
            workflow_run_id: run_id,
            workflow_name: "wf".to_string(),
            worktree_ctx: WorktreeContext {
                worktree_id: None,
                working_dir: std::env::temp_dir().to_string_lossy().to_string(),
                repo_path: String::new(),
                ticket_id: None,
                repo_id: None,
                extra_plugin_dirs: vec![],
            },
            model: None,
            exec_config: WorkflowExecConfig::default(),
            inputs: HashMap::new(),
            parent_run_id: String::new(),
            depth: 0,
            target_label: None,
            step_results: HashMap::new(),
            contexts: vec![],
            position: 0,
            all_succeeded: true,
            total_cost: 0.0,
            total_turns: 0,
            total_duration_ms: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_input_tokens: 0,
            total_cache_creation_input_tokens: 0,
            last_gate_feedback: None,
            block_output: None,
            block_with: vec![],
            resume_ctx: None,
            default_bot_name: None,
            triggered_by_hook: false,
            schema_resolver: None,
            child_runner: None,
            last_heartbeat_at: ExecutionState::new_heartbeat(),
            registry: Arc::new(ItemProviderRegistry::new()),
            event_sinks: Arc::from(vec![]),
            cancellation: crate::cancellation::CancellationToken::new(),
            current_execution_id: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn make_node(name: &str, run: &str) -> ScriptNode {
        ScriptNode {
            name: name.to_string(),
            run: run.to_string(),
            env: Default::default(),
            timeout: None,
            retries: 0,
            on_fail: None,
            bot_name: None,
        }
    }

    /// When the script emits a valid CONDUCTOR_OUTPUT block, markers and context
    /// must be extracted and stored on the step record.
    #[test]
    fn conductor_output_markers_propagate_to_step_record() {
        let (persistence, run_id) = make_persistence();
        let mut state = make_state(Arc::clone(&persistence), run_id.clone());
        // Use printf to avoid shell newline differences across platforms.
        let script = concat!(
            "printf '<<<CONDUCTOR_OUTPUT>>>\\n",
            r#"{"markers":["test_passed"],"context":"step ctx"}"#,
            "\\n<<<END_CONDUCTOR_OUTPUT>>>\\n'"
        );
        let node = make_node("check", script);
        execute_script(&mut state, &node, 0).unwrap();

        let steps = persistence.get_steps(&run_id).unwrap();
        assert_eq!(steps.len(), 1);
        let step = &steps[0];
        assert_eq!(step.status, WorkflowStepStatus::Completed);
        let markers: Vec<String> = step
            .markers_out
            .as_deref()
            .and_then(|m| serde_json::from_str(m).ok())
            .unwrap_or_default();
        assert_eq!(markers, vec!["test_passed"]);
        assert_eq!(step.context_out.as_deref(), Some("step ctx"));
    }

    /// When the script produces no CONDUCTOR_OUTPUT block, context falls back to
    /// raw stdout truncated to 2000 characters.
    #[test]
    fn falls_back_to_raw_stdout_when_no_conductor_output_block() {
        let (persistence, run_id) = make_persistence();
        let mut state = make_state(Arc::clone(&persistence), run_id.clone());
        let node = make_node("info", "echo 'plain output'");
        execute_script(&mut state, &node, 0).unwrap();

        let steps = persistence.get_steps(&run_id).unwrap();
        let step = &steps[0];
        assert_eq!(step.status, WorkflowStepStatus::Completed);
        let ctx = step.context_out.as_deref().unwrap_or("");
        assert!(
            ctx.contains("plain output"),
            "context should contain stdout: {ctx:?}"
        );
        let markers: Vec<String> = step
            .markers_out
            .as_deref()
            .and_then(|m| serde_json::from_str(m).ok())
            .unwrap_or_default();
        assert!(markers.is_empty(), "no markers expected for plain stdout");
    }

    /// When state.inputs contains a key with invalid env-var characters (e.g. `=`),
    /// that key must be silently dropped while a valid key is still injected.
    #[test]
    fn invalid_env_var_key_is_dropped_valid_key_is_injected() {
        let (persistence, run_id) = make_persistence();
        let mut state = make_state(Arc::clone(&persistence), run_id.clone());

        // One valid key and one invalid key (contains `=`).
        state
            .inputs
            .insert("VALID_KEY".to_string(), "hello".to_string());
        state
            .inputs
            .insert("INVALID=KEY".to_string(), "world".to_string());

        // The script prints the value of the env var for the valid key and exits 0.
        let node = make_node("env_test", "echo $CONDUCTOR_VALID_KEY");
        execute_script(&mut state, &node, 0).unwrap();

        let steps = persistence.get_steps(&run_id).unwrap();
        assert_eq!(steps.len(), 1);
        let step = &steps[0];
        assert_eq!(step.status, WorkflowStepStatus::Completed);
        let ctx = step.context_out.as_deref().unwrap_or("");
        assert!(
            ctx.contains("hello"),
            "valid key should be injected as CONDUCTOR_VALID_KEY; context: {ctx:?}"
        );
    }

    /// env block vars from node.env are passed to the subprocess.
    #[test]
    fn node_env_vars_are_injected_into_subprocess() {
        let (persistence, run_id) = make_persistence();
        let mut state = make_state(Arc::clone(&persistence), run_id.clone());

        let mut env = HashMap::new();
        env.insert("MY_TEST_VAR".to_string(), "expected_value".to_string());
        let node = ScriptNode {
            name: "env-inject".to_string(),
            run: "echo $MY_TEST_VAR".to_string(),
            env,
            timeout: None,
            retries: 0,
            on_fail: None,
            bot_name: None,
        };
        execute_script(&mut state, &node, 0).unwrap();

        let steps = persistence.get_steps(&run_id).unwrap();
        let step = &steps[0];
        assert_eq!(step.status, WorkflowStepStatus::Completed);
        let ctx = step.context_out.as_deref().unwrap_or("");
        assert!(
            ctx.contains("expected_value"),
            "MY_TEST_VAR should be injected from node.env; context: {ctx:?}"
        );
    }

    /// Template variables in node.env values are substituted from workflow state.
    #[test]
    fn node_env_vars_support_template_substitution() {
        let (persistence, run_id) = make_persistence();
        let mut state = make_state(Arc::clone(&persistence), run_id.clone());
        // prior_context comes from state.contexts.last().context in build_variable_map
        state.contexts.push(crate::types::ContextEntry {
            step: "prev-step".to_string(),
            iteration: 0,
            context: "substituted".to_string(),
            markers: vec![],
            structured_output: None,
            output_file: None,
        });

        let mut env = HashMap::new();
        env.insert("TEMPLATED_VAR".to_string(), "{{prior_context}}".to_string());
        let node = ScriptNode {
            name: "env-template".to_string(),
            run: "echo $TEMPLATED_VAR".to_string(),
            env,
            timeout: None,
            retries: 0,
            on_fail: None,
            bot_name: None,
        };
        execute_script(&mut state, &node, 0).unwrap();

        let steps = persistence.get_steps(&run_id).unwrap();
        let step = &steps[0];
        assert_eq!(step.status, WorkflowStepStatus::Completed);
        let ctx = step.context_out.as_deref().unwrap_or("");
        assert!(
            ctx.contains("substituted"),
            "template in env value should be substituted; context: {ctx:?}"
        );
    }
}
