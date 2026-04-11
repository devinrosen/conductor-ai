//! Shared runtime helpers for spawning and polling agent runs in tmux.
//!
//! Used by both `orchestrator.rs` (plan-step orchestration) and
//! `workflow.rs` (workflow engine execution).

use std::borrow::Cow;
use std::process::Command;
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use libc;
use rusqlite::Connection;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::agent::{list_live_tmux_windows, AgentManager, AgentRun, AgentRunStatus};

/// Build a tmux window name for a repo-scoped agent run.
///
/// Format: `repo-{slug}-{short_id}` where `short_id` is the first 8 chars of `run_id`.
pub fn repo_agent_window_name(slug: &str, run_id: &str) -> String {
    let short_id = &run_id[..8.min(run_id.len())];
    format!("repo-{slug}-{short_id}")
}

/// Resolve the path to the `conductor` binary.
///
/// Looks for a sibling `conductor` next to the current executable first,
/// then falls back to the bare name (relying on `$PATH`).
fn resolve_conductor_bin() -> String {
    let resolved = std::env::current_exe()
        .ok()
        .and_then(|p| {
            let sibling = p.parent()?.join("conductor");
            sibling
                .exists()
                .then(|| sibling.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "conductor".to_string());
    tracing::debug!("[conductor] resolved binary: {resolved}");
    resolved
}

/// Build the path for the stderr capture file for a given tmux window name.
///
/// The window name is sanitized to replace path separators and other
/// potentially dangerous characters, ensuring the file always lands in `/tmp`.
fn stderr_file_path(window_name: &str) -> String {
    let sanitized: String = window_name
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == '\0' {
                '_'
            } else {
                c
            }
        })
        .collect();
    format!("/tmp/conductor-agent-{sanitized}.err")
}

/// Spawn a new tmux window running `conductor <args>`, then verify it is alive.
///
/// `args` are the arguments passed to the `conductor` binary (e.g.
/// `["agent", "run", "--run-id", …]`).  `window_name` is used as the tmux
/// window name (`-n`) and for post-spawn verification.
///
/// If no tmux server is running, a detached session named `conductor` is
/// created automatically so agents can run without a pre-existing tmux session.
///
/// The spawned process's stderr is redirected to a temp file so that crash
/// output is available if the process exits immediately. The file is cleaned
/// up on success; on failure its contents are included in the error message.
pub fn spawn_tmux_window(
    args: &[Cow<'static, str>],
    window_name: &str,
) -> std::result::Result<(), String> {
    let conductor_bin = resolve_conductor_bin();
    let err_file = stderr_file_path(window_name);

    // Build the shell command: conductor <args> 2>/tmp/conductor-agent-<name>.err
    let shell_cmd = build_shell_command(&conductor_bin, args, &err_file);

    let tmux_args: Vec<Cow<'static, str>> = vec![
        Cow::Borrowed("new-window"),
        Cow::Borrowed("-d"),
        Cow::Borrowed("-n"),
        Cow::Owned(window_name.to_string()),
        Cow::Borrowed("--"),
        Cow::Borrowed("bash"),
        Cow::Borrowed("-c"),
        Cow::Owned(shell_cmd.clone()),
    ];

    let result = Command::new("tmux")
        .args(tmux_args.iter().map(|a| a.as_ref()))
        .output()
        .map_err(|e| format!("Failed to spawn tmux: {e}"))?;

    if result.status.success() {
        return verify_tmux_window(window_name, &err_file);
    }

    // No tmux server running — create a detached session and retry.
    // tmux error messages for a missing server vary across versions and platforms
    // ("no server running on …", "error connecting to …", "No such file or directory"),
    // so we attempt the session fallback on any new-window failure.
    let session_args: Vec<Cow<'static, str>> = vec![
        Cow::Borrowed("new-session"),
        Cow::Borrowed("-d"),
        Cow::Borrowed("-s"),
        Cow::Borrowed("conductor"),
        Cow::Borrowed("-n"),
        Cow::Owned(window_name.to_string()),
        Cow::Borrowed("--"),
        Cow::Borrowed("bash"),
        Cow::Borrowed("-c"),
        Cow::Owned(shell_cmd),
    ];

    let retry = Command::new("tmux")
        .args(session_args.iter().map(|a| a.as_ref()))
        .output()
        .map_err(|e| format!("Failed to start tmux session: {e}"))?;

    if retry.status.success() {
        return verify_tmux_window(window_name, &err_file);
    }
    let retry_stderr = String::from_utf8_lossy(&retry.stderr);
    Err(format!("Failed to start tmux session: {retry_stderr}"))
}

/// Build a shell command string that runs conductor with stderr redirected.
///
/// Each argument is single-quoted with internal single quotes escaped as `'\''`.
fn build_shell_command(conductor_bin: &str, args: &[Cow<'static, str>], err_file: &str) -> String {
    let mut parts = Vec::with_capacity(args.len() + 3);
    parts.push(shell_escape(conductor_bin));
    for arg in args {
        parts.push(shell_escape(arg.as_ref()));
    }
    format!("{} 2>{}", parts.join(" "), shell_escape(err_file))
}

/// Shell-escape a string by wrapping it in single quotes.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Parse conductor-specific failure patterns from stderr output and format a
/// clean, actionable error message.
///
/// Looks for:
/// - `[conductor] Agent failed: {reason}` — the real failure reason
/// - `[conductor] Agent log saved to {path}` — the agent log path
///
/// Returns `Some(formatted_message)` if the conductor failure pattern is found,
/// or `None` if no conductor-specific patterns are present (caller should fall
/// through to existing raw-stderr behavior).
fn format_spawn_failure_error(_window_name: &str, stderr: &str) -> Option<String> {
    let mut failure_reason: Option<&str> = None;
    let mut log_path: Option<&str> = None;

    for line in stderr.lines() {
        if let Some(reason) = line.strip_prefix("[conductor] Agent failed: ") {
            failure_reason = Some(reason);
        } else if let Some(path) = line.strip_prefix("[conductor] Agent log saved to ") {
            log_path = Some(path);
        }
    }

    let reason = failure_reason?;

    Some(match log_path {
        Some(path) => format!("Agent exited immediately: {reason}\nSee full log: {path}"),
        None => format!("Agent exited immediately: {reason}"),
    })
}

/// After a successful `tmux new-window`, wait briefly and verify the window
/// actually exists. Retries once with a longer delay before declaring failure.
///
/// On success, cleans up the stderr capture file. On failure, reads the
/// stderr file contents and includes them in the error message.
fn verify_tmux_window(window_name: &str, err_file: &str) -> std::result::Result<(), String> {
    // First check: 300ms after spawn.
    thread::sleep(Duration::from_millis(300));

    let live = list_live_tmux_windows();
    if live.contains(window_name) {
        let _ = std::fs::remove_file(err_file);
        return Ok(());
    }

    // Retry: wait another 500ms (800ms total) before declaring failure.
    thread::sleep(Duration::from_millis(500));

    let live = list_live_tmux_windows();
    if live.contains(window_name) {
        let _ = std::fs::remove_file(err_file);
        return Ok(());
    }

    // Window never appeared — read stderr capture for diagnostics.
    let stderr_output = std::fs::read_to_string(err_file).unwrap_or_default();
    let _ = std::fs::remove_file(err_file);

    // Try to extract a clean, actionable error from conductor-specific patterns.
    if let Some(friendly) = format_spawn_failure_error(window_name, &stderr_output) {
        return Err(friendly);
    }

    let detail = if stderr_output.trim().is_empty() {
        String::new()
    } else {
        format!("\n\nCaptured stderr:\n{stderr_output}")
    };

    Err(format!(
        "tmux window '{window_name}' not found after spawn — agent process may have exited immediately{detail}"
    ))
}

/// Typed error returned by [`poll_child_completion`].
#[derive(Debug)]
pub enum PollError {
    /// The caller's shutdown flag was set; the poll was aborted early.
    Shutdown,
    /// The child run did not reach a terminal state within the allotted time.
    Timeout(String),
    /// Any other error (DB error, run not found, etc.).
    Other(String),
}

impl std::fmt::Display for PollError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PollError::Shutdown => write!(f, "executor shutdown requested"),
            PollError::Timeout(msg) | PollError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

/// Poll the database for a child run to reach a terminal status.
///
/// If `shutdown` is provided and the flag is set to `true` during polling,
/// returns [`PollError::Shutdown`] immediately.
///
/// Time spent in `WaitingForFeedback` status is excluded from the timeout
/// calculation so that human response time does not cause step timeouts.
pub fn poll_child_completion(
    conn: &Connection,
    child_run_id: &str,
    poll_interval: Duration,
    timeout: Duration,
    shutdown: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
    on_tick: Option<&dyn Fn()>,
) -> std::result::Result<AgentRun, PollError> {
    let start = std::time::Instant::now();
    // Track cumulative time spent waiting for feedback so we can exclude it
    // from the timeout budget.
    let mut feedback_wait_total = Duration::ZERO;
    let mut feedback_wait_start: Option<std::time::Instant> = None;

    loop {
        if let Some(flag) = shutdown {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(PollError::Shutdown);
            }
        }

        // Effective elapsed = wall time − feedback wait time (including current wait)
        let current_wait = feedback_wait_start
            .map(|ws| ws.elapsed())
            .unwrap_or(Duration::ZERO);
        let effective_elapsed = start
            .elapsed()
            .saturating_sub(feedback_wait_total + current_wait);
        if effective_elapsed > timeout {
            return Err(PollError::Timeout(format!(
                "Child run {} timed out after {:.0}s",
                child_run_id,
                timeout.as_secs_f64()
            )));
        }

        let mgr = AgentManager::new(conn);
        match mgr.get_run(child_run_id) {
            Ok(Some(run)) => match run.status {
                AgentRunStatus::Completed | AgentRunStatus::Failed | AgentRunStatus::Cancelled => {
                    return Ok(run)
                }
                AgentRunStatus::WaitingForFeedback => {
                    // Start tracking feedback wait if not already
                    if feedback_wait_start.is_none() {
                        feedback_wait_start = Some(std::time::Instant::now());
                    }
                }
                AgentRunStatus::Running => {
                    // If we were waiting for feedback, accumulate that time
                    if let Some(wait_start) = feedback_wait_start.take() {
                        feedback_wait_total += wait_start.elapsed();
                    }
                }
            },
            Ok(None) => {
                return Err(PollError::Other(format!(
                    "Child run {child_run_id} not found in database"
                )));
            }
            Err(e) => {
                return Err(PollError::Other(format!(
                    "Database error polling child run: {e}"
                )));
            }
        }

        if let Some(f) = on_tick {
            f();
        }
        thread::sleep(poll_interval);
    }
}

/// Maximum number of CLI arguments produced by `build_agent_args`:
/// 2 subcommands + 4 fixed flags + 2 for prompt/prompt-file + 2 optional resume
/// + 2 optional model + 2 optional bot_name + 2 optional permission-mode.
const AGENT_ARGS_CAPACITY: usize = 18;

/// Build the `conductor agent run` argument list for a child agent.
///
/// If the prompt exceeds the safe tmux command-length threshold, it is written
/// to a temp file (`<working_dir>/.conductor-prompt-<run_id>.txt`) and
/// `--prompt-file` is used instead of `--prompt`.  Returns the argument list
/// ready to pass to [`spawn_tmux_window`].
///
/// `permission_mode` optionally overrides the configured permission mode
/// (e.g. `Some(AgentPermissionMode::RepoSafe)` for repo-scoped read-only agents).
pub fn build_agent_args(
    run_id: &str,
    worktree_path: &str,
    prompt: &str,
    resume_session_id: Option<&str>,
    model: Option<&str>,
    bot_name: Option<&str>,
    extra_plugin_dirs: &[String],
) -> std::result::Result<Vec<Cow<'static, str>>, String> {
    build_agent_args_with_mode(
        run_id,
        worktree_path,
        prompt,
        resume_session_id,
        model,
        bot_name,
        None,
        extra_plugin_dirs,
    )
}

/// Push optional agent flags shared between tmux and headless arg builders.
fn push_optional_agent_flags(
    args: &mut Vec<Cow<'static, str>>,
    resume_session_id: Option<&str>,
    model: Option<&str>,
    bot_name: Option<&str>,
    permission_mode: Option<&crate::config::AgentPermissionMode>,
    extra_plugin_dirs: &[String],
) {
    if let Some(id) = resume_session_id {
        args.push(Cow::Borrowed("--resume"));
        args.push(Cow::Owned(id.to_string()));
    }
    if let Some(m) = model {
        args.push(Cow::Borrowed("--model"));
        args.push(Cow::Owned(m.to_string()));
    }
    if let Some(b) = bot_name {
        args.push(Cow::Borrowed("--bot-name"));
        args.push(Cow::Owned(b.to_string()));
    }
    if let Some(mode) = permission_mode {
        args.push(Cow::Owned(mode.cli_flag().to_string()));
        if let Some(val) = mode.cli_flag_value() {
            args.push(Cow::Owned(val.to_string()));
        }
    }
    for dir in extra_plugin_dirs {
        args.push(Cow::Borrowed("--plugin-dir"));
        args.push(Cow::Owned(dir.clone()));
    }
}

/// Like [`build_agent_args`] but accepts an optional permission mode override.
///
/// The permission mode is encoded via `--permission-mode <name>` in the conductor
/// args (e.g. `--permission-mode repo-safe`). The actual flags passed to the claude
/// subprocess differ and are resolved in `run_agent()` via `claude_permission_flag()`.
#[allow(clippy::too_many_arguments)]
pub fn build_agent_args_with_mode(
    run_id: &str,
    working_dir: &str,
    prompt: &str,
    resume_session_id: Option<&str>,
    model: Option<&str>,
    bot_name: Option<&str>,
    permission_mode: Option<&crate::config::AgentPermissionMode>,
    extra_plugin_dirs: &[String],
) -> std::result::Result<Vec<Cow<'static, str>>, String> {
    // tmux has a hard limit on command-line length (~2 KB depending on version).
    // For prompts that exceed a safe threshold, write to a file and pass
    // --prompt-file instead so we never hit that limit.
    const PROMPT_FILE_THRESHOLD: usize = 512;

    let prompt_file_path: Option<String> = if prompt.len() > PROMPT_FILE_THRESHOLD {
        let path = format!("{working_dir}/.conductor-prompt-{run_id}.txt");
        std::fs::write(&path, prompt)
            .map_err(|e| format!("Failed to write prompt file '{path}': {e}"))?;
        Some(path)
    } else {
        None
    };

    let mut args: Vec<Cow<'static, str>> = Vec::with_capacity(AGENT_ARGS_CAPACITY);
    args.push(Cow::Borrowed("agent"));
    args.push(Cow::Borrowed("run"));
    args.push(Cow::Borrowed("--run-id"));
    args.push(Cow::Owned(run_id.to_string()));
    args.push(Cow::Borrowed("--worktree-path"));
    args.push(Cow::Owned(working_dir.to_string()));

    if let Some(path) = prompt_file_path {
        args.push(Cow::Borrowed("--prompt-file"));
        args.push(Cow::Owned(path));
    } else {
        args.push(Cow::Borrowed("--prompt"));
        args.push(Cow::Owned(prompt.to_string()));
    }

    // NOTE: --allowedTools is NOT passed to the conductor binary here.
    // It is derived from --permission-mode and passed to the `claude` CLI
    // subprocess inside run_agent() (conductor-cli/src/main.rs).
    push_optional_agent_flags(
        &mut args,
        resume_session_id,
        model,
        bot_name,
        permission_mode,
        extra_plugin_dirs,
    );

    Ok(args)
}

/// Build the `conductor agent orchestrate` argument list.
///
/// This function is infallible because orchestrate has no `--prompt` argument
/// and therefore no risk of exceeding tmux's command-line length limit.
pub fn build_orchestrate_args(
    run_id: &str,
    worktree_path: &str,
    model: Option<&str>,
    fail_fast: bool,
    child_timeout_secs: Option<u64>,
) -> Vec<Cow<'static, str>> {
    let mut args: Vec<Cow<'static, str>> = Vec::with_capacity(10);
    args.push(Cow::Borrowed("agent"));
    args.push(Cow::Borrowed("orchestrate"));
    args.push(Cow::Borrowed("--run-id"));
    args.push(Cow::Owned(run_id.to_string()));
    args.push(Cow::Borrowed("--worktree-path"));
    args.push(Cow::Owned(worktree_path.to_string()));

    if let Some(m) = model {
        args.push(Cow::Borrowed("--model"));
        args.push(Cow::Owned(m.to_string()));
    }

    if fail_fast {
        args.push(Cow::Borrowed("--fail-fast"));
    }

    if let Some(secs) = child_timeout_secs {
        args.push(Cow::Borrowed("--child-timeout-secs"));
        args.push(Cow::Owned(secs.to_string()));
    }

    args
}

/// Handle to a headless agent subprocess.
///
/// Caller must pass `stdout` to [`drain_stream_json`] to consume events and
/// finalize the run in the DB.  `pid` should be stored via
/// `AgentManager::update_run_subprocess_pid()` immediately after spawn.
///
/// The `child` field keeps the `Child` handle alive (and its stdio pipes open)
/// for the duration of the drain. After [`drain_stream_json`] completes, call
/// `child.wait()` to collect the exit status and avoid zombie processes.
///
/// **Not for use on the TUI main thread** — `drain_stream_json` is blocking.
#[cfg(unix)]
pub struct HeadlessHandle {
    pub pid: u32,
    pub stdout: std::process::ChildStdout,
    pub stderr: std::process::ChildStderr,
    pub child: std::process::Child,
}

/// Spawn a headless `conductor agent run` subprocess.
///
/// The child is placed in its own process group (`process_group(0)`) so it
/// survives terminal SIGHUP — the same resilience guarantee as tmux, without
/// the indirection.  stdout and stderr are piped back to the caller.
///
/// Pass `args` as produced by [`build_headless_agent_args`].
#[cfg(unix)]
pub fn spawn_headless(
    args: &[&str],
    working_dir: &std::path::Path,
) -> std::result::Result<HeadlessHandle, String> {
    use std::process::Stdio;
    let conductor_bin = resolve_conductor_bin();
    let mut child = Command::new(&conductor_bin)
        .args(args)
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0) // own process group; survives terminal SIGHUP
        .spawn()
        .map_err(|e| format!("Failed to spawn conductor headless: {e}"))?;

    let pid = child.id();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Failed to capture stdout from headless subprocess".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Failed to capture stderr from headless subprocess".to_string())?;
    Ok(HeadlessHandle {
        pid,
        stdout,
        stderr,
        child,
    })
}

/// Result of draining a headless subprocess stdout stream.
pub enum DrainOutcome {
    /// A `result` event was seen; the run was finalized in the DB.
    Completed,
    /// EOF before any `result` event (SIGTERM path or unexpected crash).
    /// Caller must mark the run as cancelled/failed in the DB.
    NoResult,
}

/// Drain the stdout of a headless subprocess, persisting events to the DB.
///
/// Reads `stdout` line-by-line via `BufReader`, writes each line to `log_file`,
/// calls `parse_events_from_value()` to produce `AgentEvent` values for the
/// `on_event` callback, and makes eager DB writes:
/// - `system/init` → `update_run_model_and_session`
/// - `assistant` with usage → `update_run_tokens_partial`
/// - `result` → `update_run_completed_if_running` or `update_run_failed_with_session`
///   and returns [`DrainOutcome::Completed`]
///
/// Returns [`DrainOutcome::NoResult`] on EOF without a `result` event (e.g. SIGTERM).
///
/// **Blocking** — must not be called from the TUI main thread or an async context.
/// Use `std::thread::spawn` to run this in a background thread.
pub fn drain_stream_json(
    stdout: impl std::io::Read,
    run_id: &str,
    log_file: &std::path::Path,
    mgr: &crate::agent::AgentManager<'_>,
    on_event: impl Fn(&crate::agent::types::AgentEvent),
) -> DrainOutcome {
    use std::io::{BufRead, BufReader, Write};

    let mut log_writer = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .map_err(|e| {
            tracing::warn!(
                "[drain_stream_json] failed to open log file {}: {e}",
                log_file.display()
            );
        })
        .ok();

    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };

        // Persist to log file (best-effort; I/O errors don't abort the drain)
        if let Some(ref mut w) = log_writer {
            if let Err(e) = writeln!(w, "{line}") {
                tracing::warn!("[drain_stream_json] failed to write log line: {e}");
            }
        }

        // Parse once for both display events and DB writes
        let value = match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Fire display-event callback
        let events = crate::agent::log_parsing::parse_events_from_value(&value);
        for event in &events {
            on_event(event);
        }

        let event_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match event_type {
            "system" => {
                let subtype = value.get("subtype").and_then(|v| v.as_str()).unwrap_or("");
                if subtype == "init" {
                    let model = value.get("model").and_then(|v| v.as_str());
                    let session_id = value.get("session_id").and_then(|v| v.as_str());
                    if let Err(e) = mgr.update_run_model_and_session(run_id, model, session_id) {
                        tracing::warn!("[drain_stream_json] failed to update model/session: {e}");
                    }
                }
            }
            "assistant" => {
                let usage = value
                    .get("message")
                    .and_then(|m| m.get("usage"))
                    .or_else(|| value.get("usage"));
                if let Some(usage) = usage {
                    let input = usage
                        .get("input_tokens")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let output = usage
                        .get("output_tokens")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let cache_read = usage
                        .get("cache_read_input_tokens")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let cache_create = usage
                        .get("cache_creation_input_tokens")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    if let Err(e) = mgr.update_run_tokens_partial(
                        run_id,
                        input,
                        output,
                        cache_read,
                        cache_create,
                    ) {
                        tracing::warn!("[drain_stream_json] failed to update tokens: {e}");
                    }
                }
            }
            "result" => {
                let log_result = crate::agent::log_parsing::parse_result_event(&value);
                let session_id = value.get("session_id").and_then(|v| v.as_str());
                if log_result.is_error {
                    let error_msg = log_result
                        .result_text
                        .as_deref()
                        .unwrap_or(crate::agent::status::DEFAULT_AGENT_ERROR_MSG);
                    if let Err(e) =
                        mgr.update_run_failed_with_session(run_id, error_msg, session_id)
                    {
                        tracing::warn!("[drain_stream_json] failed to mark run failed: {e}");
                    }
                } else {
                    // Use the if_running variant to avoid clobbering a value already written
                    // by the subprocess itself (double-write safety).
                    if let Err(e) = mgr.update_run_completed_if_running(
                        run_id,
                        log_result.result_text.as_deref().unwrap_or(""),
                    ) {
                        tracing::warn!("[drain_stream_json] failed to mark run completed: {e}");
                    }
                }
                return DrainOutcome::Completed;
            }
            _ => {}
        }
    }

    DrainOutcome::NoResult
}

/// Send SIGTERM to the process group rooted at `pid`.
///
/// The negative PID targets the entire process group (agent + any children it
/// spawned). Does not wait — caller is responsible for DB status update.
///
/// NOTE: per RFC 016 Q2, SIGTERM does NOT cause Claude CLI to flush a `result`
/// event. The caller must mark the run as `cancelled` directly after calling
/// this function.
#[cfg(unix)]
pub fn cancel_subprocess(pid: u32) {
    let ret = unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGTERM) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        tracing::warn!("cancel_subprocess: kill(-{pid}, SIGTERM) failed: {err}");
    }
}

/// Build `conductor agent run` args for a headless launch.
///
/// Unlike [`build_agent_args_with_mode`], this always writes the prompt to
/// `std::env::temp_dir()/conductor-prompt-{run_id}.txt` regardless of length.
/// Returns `(args, prompt_file_path)` so the caller can delete the prompt file
/// after [`drain_stream_json`] completes.
///
/// The existing [`build_agent_args_with_mode`] is untouched (tmux path unchanged).
#[allow(clippy::too_many_arguments)]
pub fn build_headless_agent_args(
    run_id: &str,
    working_dir: &str,
    prompt: &str,
    resume_session_id: Option<&str>,
    model: Option<&str>,
    bot_name: Option<&str>,
    permission_mode: Option<&crate::config::AgentPermissionMode>,
    extra_plugin_dirs: &[String],
) -> std::result::Result<(Vec<String>, std::path::PathBuf), String> {
    // Always write to temp dir — no worktree dir leakage, no size threshold.
    let prompt_file_path = std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt"));
    {
        use std::io::Write;
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&prompt_file_path)
                .map_err(|e| {
                    format!(
                        "Failed to write prompt file '{}': {e}",
                        prompt_file_path.display()
                    )
                })?;
            file.write_all(prompt.as_bytes()).map_err(|e| {
                format!(
                    "Failed to write prompt file '{}': {e}",
                    prompt_file_path.display()
                )
            })?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&prompt_file_path, prompt).map_err(|e| {
                format!(
                    "Failed to write prompt file '{}': {e}",
                    prompt_file_path.display()
                )
            })?;
        }
    }

    let mut args: Vec<Cow<'static, str>> = Vec::with_capacity(AGENT_ARGS_CAPACITY + 2);
    args.push(Cow::Borrowed("agent"));
    args.push(Cow::Borrowed("run"));
    args.push(Cow::Borrowed("--run-id"));
    args.push(Cow::Owned(run_id.to_string()));
    args.push(Cow::Borrowed("--worktree-path"));
    args.push(Cow::Owned(working_dir.to_string()));
    args.push(Cow::Borrowed("--prompt-file"));
    args.push(Cow::Owned(prompt_file_path.to_string_lossy().into_owned()));

    push_optional_agent_flags(
        &mut args,
        resume_session_id,
        model,
        bot_name,
        permission_mode,
        extra_plugin_dirs,
    );

    Ok((
        args.into_iter().map(|c| c.into_owned()).collect(),
        prompt_file_path,
    ))
}

/// Spawn a child agent in a new tmux window.
pub fn spawn_child_tmux(
    run_id: &str,
    worktree_path: &str,
    prompt: &str,
    model: Option<&str>,
    window_name: &str,
    bot_name: Option<&str>,
    extra_plugin_dirs: &[String],
) -> std::result::Result<(), String> {
    let args = build_agent_args(
        run_id,
        worktree_path,
        prompt,
        None,
        model,
        bot_name,
        extra_plugin_dirs,
    )?;
    spawn_tmux_window(&args, window_name)
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    #[test]
    fn repo_agent_window_name_basic() {
        let name = super::repo_agent_window_name("my-repo", "01ABCDEF99XYZW");
        assert_eq!(name, "repo-my-repo-01ABCDEF");
    }

    #[test]
    fn repo_agent_window_name_short_run_id() {
        // run_id shorter than 8 chars — should use full run_id
        let name = super::repo_agent_window_name("r", "abc");
        assert_eq!(name, "repo-r-abc");
    }

    #[test]
    fn repo_agent_window_name_exact_8_chars() {
        let name = super::repo_agent_window_name("slug", "12345678");
        assert_eq!(name, "repo-slug-12345678");
    }

    fn assert_inline_prompt(args: &[Cow<'static, str>], prompt: &str) {
        let prompt_idx = args
            .iter()
            .position(|a| a == "--prompt")
            .expect("--prompt flag missing");
        assert_eq!(args[prompt_idx + 1], prompt);
        assert!(
            !args.iter().any(|a| a == "--prompt-file"),
            "--prompt-file should not appear"
        );
    }

    fn assert_file_prompt(args: &[Cow<'static, str>], expected_content: &str, expected_path: &str) {
        let file_idx = args
            .iter()
            .position(|a| a == "--prompt-file")
            .expect("--prompt-file flag missing");
        let file_path: &str = args[file_idx + 1].as_ref();
        assert_eq!(file_path, expected_path, "prompt file path mismatch");
        assert!(
            std::path::Path::new(file_path).exists(),
            "prompt file should have been written"
        );
        assert_eq!(
            std::fs::read_to_string(file_path).unwrap(),
            expected_content
        );
        assert!(
            !args.iter().any(|a| a == "--prompt"),
            "--prompt should not appear"
        );
    }

    #[test]
    fn verify_tmux_window_rejects_nonexistent_window() {
        // Whether or not tmux is running, a bogus window name should fail.
        let err_file = "/tmp/conductor-agent-test-nonexistent.err";
        let result = super::verify_tmux_window("conductor-test-nonexistent-xyz-99999", err_file);
        assert!(result.is_err());
    }

    #[test]
    fn stderr_file_path_format() {
        let path = super::stderr_file_path("my-window-123");
        assert_eq!(path, "/tmp/conductor-agent-my-window-123.err");
    }

    #[test]
    fn stderr_file_path_sanitizes_slashes() {
        let path = super::stderr_file_path("../../etc/passwd");
        assert_eq!(path, "/tmp/conductor-agent-.._.._etc_passwd.err");
        assert!(!path.contains('/') || path.starts_with("/tmp/conductor-agent-"));
    }

    #[test]
    fn stderr_file_path_sanitizes_backslashes() {
        let path = super::stderr_file_path("foo\\bar");
        assert_eq!(path, "/tmp/conductor-agent-foo_bar.err");
    }

    #[test]
    fn stderr_file_path_sanitizes_null_bytes() {
        let path = super::stderr_file_path("foo\0bar");
        assert_eq!(path, "/tmp/conductor-agent-foo_bar.err");
    }

    #[test]
    fn build_shell_command_basic() {
        use std::borrow::Cow;
        let args = vec![
            Cow::Borrowed("agent"),
            Cow::Borrowed("run"),
            Cow::Borrowed("--run-id"),
            Cow::Owned("abc123".to_string()),
        ];
        let cmd = super::build_shell_command("/usr/local/bin/conductor", &args, "/tmp/test.err");
        assert!(cmd.starts_with("'/usr/local/bin/conductor'"));
        assert!(cmd.contains("'agent'"));
        assert!(cmd.contains("'run'"));
        assert!(cmd.contains("'--run-id'"));
        assert!(cmd.contains("'abc123'"));
        assert!(cmd.ends_with("2>'/tmp/test.err'"));
    }

    #[test]
    fn shell_escape_handles_single_quotes() {
        let escaped = super::shell_escape("it's a test");
        assert_eq!(escaped, "'it'\\''s a test'");
    }

    #[test]
    fn verify_tmux_window_includes_stderr_on_failure() {
        // Write a fake stderr file, then verify it's included in the error message.
        let err_file = format!(
            "/tmp/conductor-agent-verify-test-{}.err",
            std::process::id()
        );
        std::fs::write(&err_file, "Error: something broke\n").unwrap();

        let result =
            super::verify_tmux_window("conductor-test-nonexistent-stderr-99999", &err_file);
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(
            msg.contains("Error: something broke"),
            "error should include stderr contents: {msg}"
        );
        // File should have been cleaned up
        assert!(
            !std::path::Path::new(&err_file).exists(),
            "stderr file should be cleaned up after read"
        );
    }

    #[test]
    fn format_spawn_failure_error_with_failure_and_log_path() {
        let stderr = "[conductor] Agent failed: Claude exited with status: exit status: 1\n\
                      [conductor] Agent log saved to /Users/devin/.conductor/agent-logs/01ABC.log\n";
        let result = super::format_spawn_failure_error("my-window", stderr);
        assert!(result.is_some());
        let msg = result.unwrap();
        assert_eq!(
            msg,
            "Agent exited immediately: Claude exited with status: exit status: 1\n\
             See full log: /Users/devin/.conductor/agent-logs/01ABC.log"
        );
    }

    #[test]
    fn format_spawn_failure_error_with_failure_only() {
        let stderr = "[conductor] Agent failed: Claude exited with status: exit status: 1\n";
        let result = super::format_spawn_failure_error("my-window", stderr);
        assert!(result.is_some());
        let msg = result.unwrap();
        assert_eq!(
            msg,
            "Agent exited immediately: Claude exited with status: exit status: 1"
        );
        assert!(!msg.contains("See full log"), "no log path should appear");
    }

    #[test]
    fn format_spawn_failure_error_with_log_only() {
        // Log path alone without failure line → None (not actionable without reason)
        let stderr =
            "[conductor] Agent log saved to /Users/devin/.conductor/agent-logs/01ABC.log\n";
        let result = super::format_spawn_failure_error("my-window", stderr);
        assert!(
            result.is_none(),
            "expected None when no failure line present"
        );
    }

    #[test]
    fn format_spawn_failure_error_empty() {
        let result = super::format_spawn_failure_error("my-window", "");
        assert!(result.is_none(), "expected None for empty stderr");
    }

    #[test]
    fn verify_tmux_window_surfaces_conductor_error() {
        // Write a fake err file with conductor-specific patterns.
        let err_file = format!(
            "/tmp/conductor-agent-conductor-error-test-{}.err",
            std::process::id()
        );
        let stderr_content =
            "[conductor] Agent failed: Claude exited with status: exit status: 1\n\
             [conductor] Agent log saved to /tmp/fake-agent-run.log\n";
        std::fs::write(&err_file, stderr_content).unwrap();

        let result = super::verify_tmux_window("conductor-test-nonexistent-xyz-99998", &err_file);
        assert!(result.is_err());
        let msg = result.unwrap_err();

        // Should show the friendly message, not the raw tmux "not found" message
        assert!(
            msg.contains("Agent exited immediately:"),
            "expected friendly error prefix: {msg}"
        );
        assert!(
            msg.contains("Claude exited with status: exit status: 1"),
            "expected failure reason in message: {msg}"
        );
        assert!(
            msg.contains("See full log:"),
            "expected log path hint: {msg}"
        );
        assert!(
            !msg.contains("tmux window"),
            "should not contain raw tmux error: {msg}"
        );
        assert!(
            !msg.contains("Captured stderr:"),
            "should not contain raw 'Captured stderr:' prefix: {msg}"
        );

        // File should have been cleaned up
        assert!(
            !std::path::Path::new(&err_file).exists(),
            "stderr file should be cleaned up"
        );
    }

    #[test]
    fn build_agent_args_short_prompt_uses_inline() {
        let prompt = "short prompt";
        assert!(prompt.len() <= 512);
        let args =
            super::build_agent_args("run-1", "/tmp/wt", prompt, None, None, None, &[]).unwrap();
        assert_inline_prompt(&args, prompt);
    }

    #[test]
    fn build_agent_args_long_prompt_uses_file() {
        let tmp = std::env::temp_dir().join(format!("conductor-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let worktree = tmp.to_str().unwrap();
        let run_id = "run-long-99";

        let prompt = "x".repeat(513);
        let args =
            super::build_agent_args(run_id, worktree, &prompt, None, None, None, &[]).unwrap();

        let expected_path = format!("{worktree}/.conductor-prompt-{run_id}.txt");
        assert_file_prompt(&args, &prompt, &expected_path);

        // cleanup
        let _ = std::fs::remove_file(&expected_path);
        let _ = std::fs::remove_dir(&tmp);
    }

    #[test]
    fn build_agent_args_file_write_error_propagates() {
        let worktree = "/nonexistent/path/that/does/not/exist";
        let prompt = "x".repeat(513);
        let result =
            super::build_agent_args("run-err-01", worktree, &prompt, None, None, None, &[]);
        assert!(result.is_err(), "expected Err when write fails");
        let msg = result.unwrap_err();
        assert!(
            msg.starts_with("Failed to write prompt file"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn build_agent_args_exact_boundary_prompt_uses_inline() {
        // A prompt of exactly PROMPT_FILE_THRESHOLD bytes must still use --prompt,
        // because the condition is strictly `>`, not `>=`.
        let prompt = "x".repeat(512);
        assert_eq!(prompt.len(), 512);
        let args =
            super::build_agent_args("run-boundary", "/tmp/wt", &prompt, None, None, None, &[])
                .unwrap();
        assert_inline_prompt(&args, &prompt);
    }

    #[test]
    fn build_agent_args_with_resume_sets_flag() {
        let prompt = "short prompt";
        let args = super::build_agent_args(
            "run-1",
            "/tmp/wt",
            prompt,
            Some("sess-abc"),
            None,
            None,
            &[],
        )
        .unwrap();
        let resume_idx = args
            .iter()
            .position(|a| a == "--resume")
            .expect("--resume flag missing");
        assert_eq!(args[resume_idx + 1], "sess-abc");
    }

    #[test]
    fn build_orchestrate_args_basic() {
        let args = super::build_orchestrate_args("run-o1", "/tmp/wt", None, false, None);
        assert_eq!(args[0], "agent");
        assert_eq!(args[1], "orchestrate");
        let run_id_idx = args.iter().position(|a| a == "--run-id").unwrap();
        assert_eq!(args[run_id_idx + 1], "run-o1");
        let wt_idx = args.iter().position(|a| a == "--worktree-path").unwrap();
        assert_eq!(args[wt_idx + 1], "/tmp/wt");
        assert!(!args.iter().any(|a| a == "--model"));
        assert!(!args.iter().any(|a| a == "--fail-fast"));
        assert!(!args.iter().any(|a| a == "--child-timeout-secs"));
    }

    #[test]
    fn build_orchestrate_args_all_flags() {
        let args =
            super::build_orchestrate_args("run-o2", "/tmp/wt", Some("claude-3"), true, Some(120));
        assert!(args.iter().any(|a| a == "--fail-fast"));
        let model_idx = args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(args[model_idx + 1], "claude-3");
        let timeout_idx = args
            .iter()
            .position(|a| a == "--child-timeout-secs")
            .unwrap();
        assert_eq!(args[timeout_idx + 1], "120");
    }

    #[test]
    fn build_agent_args_with_mode_skip_permissions() {
        use crate::config::AgentPermissionMode;
        let args = super::build_agent_args_with_mode(
            "run-1",
            "/tmp/wt",
            "prompt",
            None,
            None,
            None,
            Some(&AgentPermissionMode::SkipPermissions),
            &[],
        )
        .unwrap();
        assert!(
            args.iter().any(|a| a == "--dangerously-skip-permissions"),
            "expected --dangerously-skip-permissions flag"
        );
    }

    #[test]
    fn build_agent_args_with_mode_auto_mode() {
        use crate::config::AgentPermissionMode;
        let args = super::build_agent_args_with_mode(
            "run-1",
            "/tmp/wt",
            "prompt",
            None,
            None,
            None,
            Some(&AgentPermissionMode::AutoMode),
            &[],
        )
        .unwrap();
        assert!(
            args.iter().any(|a| a == "--enable-auto-mode"),
            "expected --enable-auto-mode flag"
        );
    }

    #[test]
    fn build_agent_args_with_mode_plan() {
        use crate::config::AgentPermissionMode;
        let args = super::build_agent_args_with_mode(
            "run-1",
            "/tmp/wt",
            "prompt",
            None,
            None,
            None,
            Some(&AgentPermissionMode::Plan),
            &[],
        )
        .unwrap();
        let idx = args
            .iter()
            .position(|a| a == "--permission-mode")
            .expect("expected --permission-mode flag");
        assert_eq!(args[idx + 1], "plan", "expected 'plan' value after flag");

        // --allowedTools must NOT appear in conductor args — it is passed
        // to the claude CLI subprocess inside run_agent(), not here.
        assert!(
            !args.iter().any(|a| a == "--allowedTools"),
            "conductor args must not contain --allowedTools (it belongs on the claude CLI)"
        );
    }

    #[test]
    fn build_agent_args_with_mode_repo_safe() {
        use crate::config::AgentPermissionMode;
        let args = super::build_agent_args_with_mode(
            "run-1",
            "/tmp/wt",
            "prompt",
            None,
            None,
            None,
            Some(&AgentPermissionMode::RepoSafe),
            &[],
        )
        .unwrap();
        let idx = args
            .iter()
            .position(|a| a == "--permission-mode")
            .expect("expected --permission-mode flag in conductor args");
        assert_eq!(
            args[idx + 1],
            "repo-safe",
            "expected 'repo-safe' value after --permission-mode"
        );

        // --allowedTools must NOT appear in conductor args — it is passed
        // to the claude CLI subprocess inside run_agent(), not here.
        assert!(
            !args.iter().any(|a| a == "--allowedTools"),
            "conductor args must not contain --allowedTools (it belongs on the claude CLI)"
        );
    }

    #[test]
    fn build_agent_args_non_plan_no_allowed_tools() {
        use crate::config::AgentPermissionMode;
        // RepoSafe is excluded: its allowed_tools() is applied in run_agent(), not here.
        for mode in &[
            AgentPermissionMode::SkipPermissions,
            AgentPermissionMode::AutoMode,
        ] {
            let args = super::build_agent_args_with_mode(
                "run-1",
                "/tmp/wt",
                "prompt",
                None,
                None,
                None,
                Some(mode),
                &[],
            )
            .unwrap();
            assert!(
                !args.iter().any(|a| a == "--allowedTools"),
                "expected no --allowedTools for {:?}",
                mode
            );
        }
    }

    #[test]
    fn build_agent_args_with_mode_none() {
        let args = super::build_agent_args_with_mode(
            "run-1",
            "/tmp/wt",
            "prompt",
            None,
            None,
            None,
            None,
            &[],
        )
        .unwrap();
        assert!(
            !args.iter().any(|a| a == "--dangerously-skip-permissions"
                || a == "--enable-auto-mode"
                || a == "--permission-mode"),
            "no permission flag should appear when mode is None"
        );
    }

    #[test]
    fn build_agent_args_with_model_override() {
        let args = super::build_agent_args_with_mode(
            "run-1",
            "/tmp/wt",
            "prompt",
            None,
            Some("claude-sonnet-4-6"),
            None,
            None,
            &[],
        )
        .unwrap();
        let idx = args
            .iter()
            .position(|a| a == "--model")
            .expect("expected --model flag");
        assert_eq!(args[idx + 1], "claude-sonnet-4-6");
    }

    #[test]
    fn build_agent_args_with_bot_name() {
        let args = super::build_agent_args_with_mode(
            "run-1",
            "/tmp/wt",
            "prompt",
            None,
            None,
            Some("my-bot"),
            None,
            &[],
        )
        .unwrap();
        let idx = args
            .iter()
            .position(|a| a == "--bot-name")
            .expect("expected --bot-name flag");
        assert_eq!(args[idx + 1], "my-bot");
    }

    #[test]
    fn build_agent_args_all_options() {
        use crate::config::AgentPermissionMode;
        let args = super::build_agent_args_with_mode(
            "run-all",
            "/tmp/wt",
            "prompt",
            Some("sess-123"),
            Some("claude-opus-4-6"),
            Some("bot-x"),
            Some(&AgentPermissionMode::Plan),
            &[],
        )
        .unwrap();

        // Verify all flags present
        let resume_idx = args
            .iter()
            .position(|a| a == "--resume")
            .expect("--resume missing");
        assert_eq!(args[resume_idx + 1], "sess-123");

        let model_idx = args
            .iter()
            .position(|a| a == "--model")
            .expect("--model missing");
        assert_eq!(args[model_idx + 1], "claude-opus-4-6");

        let bot_idx = args
            .iter()
            .position(|a| a == "--bot-name")
            .expect("--bot-name missing");
        assert_eq!(args[bot_idx + 1], "bot-x");

        let perm_idx = args
            .iter()
            .position(|a| a == "--permission-mode")
            .expect("--permission-mode missing");
        assert_eq!(args[perm_idx + 1], "plan");
    }

    fn test_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        conn
    }

    #[test]
    fn build_headless_agent_args_includes_run_id_and_worktree() {
        let (args, _prompt_file) = super::build_headless_agent_args(
            "run-h-1",
            "/tmp/wt",
            "test prompt",
            None,
            None,
            None,
            None,
            &[],
        )
        .unwrap();
        let pos = args.iter().position(|a| a == "--run-id").unwrap();
        assert_eq!(args[pos + 1], "run-h-1");
        let pos = args.iter().position(|a| a == "--worktree-path").unwrap();
        assert_eq!(args[pos + 1], "/tmp/wt");
    }

    #[test]
    fn build_headless_agent_args_with_all_options() {
        use crate::config::AgentPermissionMode;
        let (args, _prompt_file) = super::build_headless_agent_args(
            "run-h-2",
            "/tmp/wt",
            "test prompt",
            Some("sess-abc"),
            Some("claude-opus-4-6"),
            Some("bot-y"),
            Some(&AgentPermissionMode::Plan),
            &["dir1".to_string()],
        )
        .unwrap();

        let pos = args.iter().position(|a| a == "--resume").unwrap();
        assert_eq!(args[pos + 1], "sess-abc");
        let pos = args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(args[pos + 1], "claude-opus-4-6");
        let pos = args.iter().position(|a| a == "--bot-name").unwrap();
        assert_eq!(args[pos + 1], "bot-y");
        let pos = args.iter().position(|a| a == "--plugin-dir").unwrap();
        assert_eq!(args[pos + 1], "dir1");
    }

    #[test]
    fn build_headless_agent_args_prompt_file_written() {
        let (args, prompt_file) = super::build_headless_agent_args(
            "run-h-3",
            "/tmp/wt",
            "hello world",
            None,
            None,
            None,
            None,
            &[],
        )
        .unwrap();
        assert!(prompt_file.exists());
        let content = std::fs::read_to_string(&prompt_file).unwrap();
        assert_eq!(content, "hello world");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::metadata(&prompt_file).unwrap();
            let mode = metadata.mode() & 0o777;
            assert_eq!(mode, 0o600, "prompt file should be 0o600, got 0o{mode:o}");
        }
        let _ = std::fs::remove_file(&prompt_file);
        // --prompt-file should be in args
        assert!(args.iter().any(|a| a == "--prompt-file"));
    }

    #[test]
    fn drain_stream_json_completed_on_result_event() {
        let conn = test_db();
        let mgr = crate::agent::AgentManager::new(&conn);
        let run = mgr.create_run(None, "test prompt", None, None).unwrap();

        let json_lines = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\",\"model\":\"claude-test\",\"session_id\":\"sess-1\"}\n",
            "{\"type\":\"result\",\"is_error\":false,\"result\":\"done\",\"session_id\":\"sess-1\"}\n",
        );
        let cursor = std::io::Cursor::new(json_lines.as_bytes());
        let outcome = super::drain_stream_json(
            cursor,
            &run.id,
            std::path::Path::new("/dev/null"),
            &mgr,
            |_| {},
        );
        assert!(matches!(outcome, super::DrainOutcome::Completed));
    }

    #[test]
    fn drain_stream_json_no_result_returns_no_result() {
        let conn = test_db();
        let mgr = crate::agent::AgentManager::new(&conn);
        let run = mgr.create_run(None, "test prompt", None, None).unwrap();

        let json_lines =
            "{\"type\":\"system\",\"subtype\":\"init\",\"model\":\"claude-test\",\"session_id\":\"sess-1\"}\n";
        let cursor = std::io::Cursor::new(json_lines.as_bytes());
        let outcome = super::drain_stream_json(
            cursor,
            &run.id,
            std::path::Path::new("/dev/null"),
            &mgr,
            |_| {},
        );
        assert!(matches!(outcome, super::DrainOutcome::NoResult));
    }

    #[test]
    fn drain_stream_json_error_result_event() {
        let conn = test_db();
        let mgr = crate::agent::AgentManager::new(&conn);
        let run = mgr.create_run(None, "test prompt", None, None).unwrap();

        let json_lines =
            "{\"type\":\"result\",\"is_error\":true,\"result\":\"something went wrong\"}\n";
        let cursor = std::io::Cursor::new(json_lines.as_bytes());
        let outcome = super::drain_stream_json(
            cursor,
            &run.id,
            std::path::Path::new("/dev/null"),
            &mgr,
            |_| {},
        );
        assert!(matches!(outcome, super::DrainOutcome::Completed));
        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.status, crate::agent::AgentRunStatus::Failed);
    }

    #[test]
    fn drain_stream_json_token_update() {
        let conn = test_db();
        let mgr = crate::agent::AgentManager::new(&conn);
        let run = mgr.create_run(None, "test prompt", None, None).unwrap();

        let json_lines = concat!(
            "{\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}}\n",
            "{\"type\":\"result\",\"is_error\":false,\"result\":\"done\"}\n",
        );
        let cursor = std::io::Cursor::new(json_lines.as_bytes());
        let _ = super::drain_stream_json(
            cursor,
            &run.id,
            std::path::Path::new("/dev/null"),
            &mgr,
            |_| {},
        );
        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.input_tokens, Some(10));
        assert_eq!(fetched.output_tokens, Some(5));
    }
}
