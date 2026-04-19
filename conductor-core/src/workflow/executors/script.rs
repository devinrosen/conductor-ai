use std::process::Command;
use std::thread;
use std::time::Duration;

use crate::error::{ConductorError, Result};
use crate::workflow_dsl::ScriptNode;

use crate::workflow::engine::{
    handle_on_fail, record_step_success, restore_step, should_skip, ExecutionState,
};
use crate::workflow::output::parse_conductor_output;
use crate::workflow::prompt_builder::{
    build_variable_map, substitute_variables, substitute_variables_keep_literal,
};
use crate::workflow::run_context::RunContext;
use crate::workflow::status::WorkflowStepStatus;

// ---------------------------------------------------------------------------
// Script step executor
// ---------------------------------------------------------------------------

/// Maximum bytes read from a script's stdout file into memory.
/// Output beyond this limit is truncated with a notice appended.
const MAX_STDOUT_BYTES: usize = 100 * 1024; // 100 KB

/// Read at most [`MAX_STDOUT_BYTES`] from `path`, returning a UTF-8 string.
/// If the file is larger than the limit the content is truncated and a notice
/// is appended so callers can see that truncation occurred.
pub fn read_stdout_bounded(path: &str) -> std::io::Result<String> {
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
pub enum ScriptPollResult {
    /// Process exited with success (exit code 0).
    Succeeded,
    /// Process exited with failure (non-zero exit code or wait error).
    Failed(String),
    /// Script exceeded its timeout; the process has been killed.
    TimedOut,
    /// Workflow shutdown signal received; the process has been killed.
    Cancelled,
}

/// Gracefully terminate a child process and wait for it to exit.
///
/// On Unix: sends SIGTERM to the process group (via [`crate::process_utils::cancel_subprocess`]),
/// which also handles escalation to SIGKILL after a grace period.
/// On non-Unix: falls back to [`std::process::Child::kill`].
fn kill_child(child: &mut std::process::Child, pid: u32) {
    #[cfg(unix)]
    crate::process_utils::cancel_subprocess(pid);
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    let _ = child.wait();
}

/// Poll a child process until it exits, times out, or the shutdown signal fires.
///
/// Checks the shutdown flag and elapsed time every 200 ms using `try_wait`.
/// `pid` is the child's PID, passed to [`kill_child`] (Unix: SIGTERM → SIGKILL
/// via process group; non-Unix: `kill()`) on timeout or cancellation.
pub fn poll_script_child(
    child: &mut std::process::Child,
    pid: u32,
    timeout_secs: Option<u64>,
    shutdown: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> ScriptPollResult {
    let poll_interval = Duration::from_millis(200);
    let start = std::time::Instant::now();

    loop {
        // Check shutdown signal
        if let Some(flag) = shutdown {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                kill_child(child, pid);
                return ScriptPollResult::Cancelled;
            }
        }

        // Check per-step timeout
        if let Some(timeout) = timeout_secs {
            if start.elapsed().as_secs() >= timeout {
                kill_child(child, pid);
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

pub fn execute_script(
    state: &mut ExecutionState<'_>,
    node: &ScriptNode,
    iteration: u32,
) -> Result<()> {
    let pos = state.position;
    state.position += 1;

    let (working_dir, repo_path, script_env) = {
        let ctx = crate::workflow::run_context::WorktreeRunContext::new(state);
        (
            ctx.working_dir().to_path_buf(),
            ctx.repo_path().to_path_buf(),
            ctx.script_env(),
        )
    };

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
        working_dir.to_str().unwrap_or(""),
        repo_path.to_str().unwrap_or(""),
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
        .map(|(k, v)| (k.clone(), substitute_variables_keep_literal(v, &vars)))
        .collect();

    // Retry loop
    let max_attempts = 1 + node.retries;
    let mut last_error = String::new();

    for attempt in 0..max_attempts {
        let step_id = state.wf_mgr.insert_step_running(
            &state.workflow_run_id,
            step_label,
            "script",
            false,
            pos,
            iteration as i64,
            attempt as i64,
        )?;

        // Create a temp file for the script's stdout+stderr.
        // Both streams are redirected here so that subprocess output never
        // reaches the terminal directly (which would corrupt TUI rendering).
        let output_path = format!("{}/script-{}.out", working_dir.display(), step_id);
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
            .envs(&script_env)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdout(output_file)
            .stderr(stderr_file)
            .current_dir(&working_dir);
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
        // Put the script in its own process group so cancel_subprocess can send
        // SIGTERM to the entire group (script + any subprocesses it spawns).
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
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

        let pid = child.id();
        if let Err(e) = state.wf_mgr.set_step_subprocess_pid(&step_id, Some(pid)) {
            tracing::warn!(
                "Script step '{}': failed to persist PID {pid}: {e}",
                step_label
            );
        }

        let poll_result = poll_script_child(
            &mut child,
            pid,
            node.timeout,
            state.exec_config.shutdown.as_ref(),
        );
        // Clear PID exactly once — covers all result variants, including any
        // future additions to ScriptPollResult, providing a mechanical guarantee
        // that no terminal path can accidentally leave a stale PID in the DB.
        if let Err(e) = state.wf_mgr.set_step_subprocess_pid(&step_id, None) {
            tracing::warn!(
                step_id = %step_id,
                error = %e,
                "Failed to clear subprocess PID during script step cleanup"
            );
        }

        match poll_result {
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
                    None,
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

    handle_on_fail(
        state,
        step_key,
        step_label,
        &node.on_fail,
        last_error,
        node.retries,
        iteration,
        max_attempts,
    )
}
