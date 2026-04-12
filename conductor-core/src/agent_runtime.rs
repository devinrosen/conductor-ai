//! Shared runtime helpers for spawning and polling agent runs.
//!
//! Used by both `orchestrator.rs` (plan-step orchestration) and
//! `workflow.rs` (workflow engine execution).

use std::borrow::Cow;
use std::process::Command;
use std::thread;
use std::time::Duration;

use rusqlite::Connection;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::agent::{AgentManager, AgentRun, AgentRunStatus};

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
/// The prompt is written to a temp file and passed via `--prompt-file`.
/// Returns the argument list ready to pass to [`spawn_headless`].
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

/// Push optional agent flags shared between arg builders.
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
        // Only Plan and RepoSafe have a conductor-level passthrough flag
        // (--permission-mode <value>).  SkipPermissions and AutoMode are
        // the default behaviour; passing their claude-level flags here would
        // cause `conductor agent run` to reject the unknown argument.
        if let Some(val) = mode.cli_flag_value() {
            args.push(Cow::Borrowed("--permission-mode"));
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
///
/// The prompt is always written to `temp_dir()/conductor-prompt-{run_id}.txt` (mode
/// 0o600 on Unix) and passed via `--prompt-file`, keeping it out of the git worktree.
/// The CLI child subprocess reads and deletes the file via
/// `read_and_maybe_cleanup_prompt_file`.
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
    let prompt_file_path = std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&prompt_file_path)
            .and_then(|mut f| {
                use std::io::Write;
                f.write_all(prompt.as_bytes())
            })
            .map_err(|e| {
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

    let mut args: Vec<Cow<'static, str>> = Vec::with_capacity(AGENT_ARGS_CAPACITY);
    args.push(Cow::Borrowed("agent"));
    args.push(Cow::Borrowed("run"));
    args.push(Cow::Borrowed("--run-id"));
    args.push(Cow::Owned(run_id.to_string()));
    args.push(Cow::Borrowed("--worktree-path"));
    args.push(Cow::Owned(working_dir.to_string()));

    args.push(Cow::Borrowed("--prompt-file"));
    args.push(Cow::Owned(prompt_file_path.to_string_lossy().into_owned()));

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
/// All resource access is through safe decomposition methods:
/// - [`HeadlessHandle::pid`] — read the subprocess PID (store via
///   `AgentManager::update_run_subprocess_pid()` immediately after spawn).
/// - [`HeadlessHandle::into_drain_parts`] — simple sequential drain: stdout →
///   [`drain_stream_json`], then finish closure drops stderr and waits.
/// - [`HeadlessHandle::into_stderr_drain_parts`] — concurrent drain: caller
///   owns stderr for a dedicated drain thread; finish only waits.
/// - [`HeadlessHandle::abort`] — kill and reap without deadlocking.
///
/// The `stdout`, `stderr`, and `child` fields are private to prevent callers
/// from bypassing the safe decomposition methods and re-introducing the
/// pipe-buffer deadlock those methods were designed to prevent.
///
/// **Not for use on the TUI main thread** — `drain_stream_json` is blocking.
#[cfg(unix)]
pub struct HeadlessHandle {
    pid: u32,
    stdout: std::process::ChildStdout,
    stderr: std::process::ChildStderr,
    child: std::process::Child,
}

#[cfg(unix)]
impl HeadlessHandle {
    /// Build a `HeadlessHandle` from a freshly-spawned `Child` with piped stdio.
    ///
    /// Extracts `stdout` and `stderr` from the child.  Returns an error if
    /// the pipes are missing (i.e. `Stdio::piped()` was not configured).
    pub fn from_child(mut child: std::process::Child) -> std::result::Result<Self, String> {
        let pid = child.id();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "HeadlessHandle: child has no stdout pipe".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "HeadlessHandle: child has no stderr pipe".to_string())?;
        Ok(Self {
            pid,
            stdout,
            stderr,
            child,
        })
    }

    /// Returns the PID of the headless subprocess.
    ///
    /// Store this immediately after spawn via
    /// `AgentManager::update_run_subprocess_pid()`.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Decompose the handle into stderr, stdout, and a finish closure for
    /// concurrent draining.
    ///
    /// Use this when you need to drain `stderr` on a dedicated thread
    /// concurrently with draining `stdout` — for example when the subprocess
    /// writes many KB to stderr and you must keep the kernel pipe buffer from
    /// filling while draining stdout in a separate thread.
    ///
    /// ```ignore
    /// let (stderr_pipe, stdout_pipe, finish) = handle.into_stderr_drain_parts();
    /// std::thread::spawn(move || drain_stderr(stderr_pipe));
    /// drain_stream_json(stdout_pipe, ...);
    /// finish();  // waits for child exit — does NOT drop stderr (caller owns it)
    /// ```
    ///
    /// The caller **must** drain `stderr` concurrently; the `finish` closure
    /// only calls `child.wait()` and does not drop `stderr`.  For the simple
    /// sequential case, prefer [`into_drain_parts`] instead.
    ///
    /// [`into_drain_parts`]: HeadlessHandle::into_drain_parts
    pub fn into_stderr_drain_parts(
        self,
    ) -> (
        std::process::ChildStderr,
        std::process::ChildStdout,
        impl FnOnce(),
    ) {
        let stderr = self.stderr;
        let stdout = self.stdout;
        let mut child = self.child;
        let finish = move || {
            let _ = child.wait();
        };
        (stderr, stdout, finish)
    }

    /// Decompose the handle into a stdout pipe for draining and a finish closure.
    ///
    /// The typical drain pattern moves `stdout` into [`drain_stream_json`] and
    /// then needs to drop `stderr` **before** calling `child.wait()`.  Because
    /// `drain_stream_json` consumes `stdout` as a value, the `HeadlessHandle` is
    /// partially moved after that call and `self`-consuming methods cannot be
    /// called on it.
    ///
    /// `into_drain_parts` splits the handle before any partial move:
    ///
    /// ```ignore
    /// let (stdout, finish) = handle.into_drain_parts();
    /// drain_stream_json(stdout, &run_id, &log_path, &mgr, on_event);
    /// finish();  // drops stderr, then waits — no deadlock
    /// ```
    ///
    /// The returned `finish` closure drops `stderr` first so the child receives
    /// EPIPE on any pending stderr writes and can exit; `wait()` then returns
    /// immediately.
    pub fn into_drain_parts(self) -> (std::process::ChildStdout, impl FnOnce()) {
        let stdout = self.stdout;
        let stderr = self.stderr;
        let mut child = self.child;
        let finish = move || {
            drop(stderr);
            let _ = child.wait();
        };
        (stdout, finish)
    }

    /// Abort the subprocess without deadlocking on pipe buffers.
    ///
    /// Closes the read ends of stdout and stderr **before** calling `wait()`.
    /// If the child has filled the pipe buffer it is blocked on a write; calling
    /// `wait()` while the read end is still open would deadlock because the child
    /// can never exit.  Dropping the pipes first causes the child's writes to
    /// fail with EPIPE so it can exit, after which `wait()` reaps it immediately.
    pub fn abort(self) {
        drop(self.stdout);
        drop(self.stderr);
        let mut child = self.child;
        // Explicitly kill the process so it terminates immediately rather than
        // relying on EPIPE, which only fires when the child next attempts a write.
        // A compute-heavy child that rarely writes could otherwise run indefinitely.
        // kill() returns an error if the process already exited — safe to ignore.
        let _ = child.kill();
        let _ = child.wait();
    }
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
    args: &[Cow<'static, str>],
    working_dir: &std::path::Path,
) -> std::result::Result<HeadlessHandle, String> {
    use std::process::Stdio;
    let conductor_bin = resolve_conductor_bin();
    let child = Command::new(&conductor_bin)
        .args(args.iter().map(|a| a.as_ref()))
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0) // own process group; survives terminal SIGHUP
        .spawn()
        .map_err(|e| format!("Failed to spawn conductor headless: {e}"))?;

    HeadlessHandle::from_child(child)
}

/// Parameters for spawning a headless agent subprocess.
///
/// Groups the eight shared parameters across [`build_headless_agent_args`] and
/// [`try_spawn_headless_run`] to keep call sites readable and avoid a
/// `#[allow(clippy::too_many_arguments)]` suppression.
pub struct SpawnHeadlessParams<'a> {
    pub run_id: &'a str,
    pub working_dir: &'a str,
    pub prompt: &'a str,
    pub resume_session_id: Option<&'a str>,
    pub model: Option<&'a str>,
    pub bot_name: Option<&'a str>,
    pub permission_mode: Option<&'a crate::config::AgentPermissionMode>,
    pub plugin_dirs: &'a [String],
}

/// Build headless args and spawn the conductor subprocess in one step.
///
/// Combines [`build_headless_agent_args`] and [`spawn_headless`] into a single
/// call.  On spawn failure the prompt file is cleaned up before returning the
/// error string so the caller doesn't need to manage it.
#[cfg(unix)]
pub fn try_spawn_headless_run(
    params: &SpawnHeadlessParams<'_>,
) -> std::result::Result<(HeadlessHandle, std::path::PathBuf), String> {
    let (args, pf) = build_headless_agent_args(params)
        .map_err(|e| format!("failed to prepare agent args: {e}"))?;
    let h = spawn_headless(&args, std::path::Path::new(params.working_dir)).map_err(|e| {
        let _ = std::fs::remove_file(&pf);
        format!("spawn failed: {e}")
    })?;
    Ok((h, pf))
}

/// Result of draining a headless subprocess stdout stream.
#[derive(Copy, Clone)]
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
                if log_result.is_error {
                    let error_msg = log_result
                        .result_text
                        .as_deref()
                        .unwrap_or(crate::agent::status::DEFAULT_AGENT_ERROR_MSG);
                    if let Err(e) = mgr.update_run_failed_with_session(
                        run_id,
                        error_msg,
                        log_result.session_id.as_deref(),
                    ) {
                        tracing::warn!("[drain_stream_json] failed to mark run failed: {e}");
                    }
                } else {
                    // Use the if_running variant to avoid clobbering a value already written
                    // by the subprocess itself (double-write safety). Persist all result-event
                    // fields (cost_usd, num_turns, duration_ms, final token counts).
                    if let Err(e) = mgr.update_run_completed_if_running_full(run_id, &log_result) {
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

/// Send SIGTERM to the process group rooted at `pid`, wait up to 5 seconds
/// for graceful exit, then escalate to SIGKILL if still alive.
///
/// The negative PID targets the entire process group (agent + any children it
/// spawned). `pid_is_alive` checks the positive PID (the group leader); if the
/// leader is dead the group is effectively terminated.
///
/// This call **blocks** for up to 5 seconds. Call from a background thread or
/// inside `tokio::task::spawn_blocking` — never from the TUI main thread or an
/// async task directly.
///
/// NOTE: per RFC 016 Q2, SIGTERM does NOT cause Claude CLI to flush a `result`
/// event. The caller must mark the run as `cancelled` in the DB before calling
/// this function to prevent a concurrent drain from overwriting the status.
///
/// Implementation lives in [`crate::process_utils::cancel_subprocess`].
///
/// # Deprecated
/// Use [`crate::process_utils::cancel_subprocess`] directly instead.
#[deprecated(
    since = "0.1.0",
    note = "use conductor_core::process_utils::cancel_subprocess instead"
)]
#[cfg(unix)]
pub fn cancel_subprocess(pid: u32) {
    crate::process_utils::cancel_subprocess(pid);
}

/// Build `conductor agent run` args for a headless launch.
///
/// Unlike [`build_agent_args_with_mode`], this always writes the prompt to
/// `std::env::temp_dir()/conductor-prompt-{run_id}.txt` regardless of length.
/// Returns `(args, prompt_file_path)` so the caller can delete the prompt file
/// after [`drain_stream_json`] completes.
///
/// The existing [`build_agent_args_with_mode`] always writes the prompt to the temp dir.
pub fn build_headless_agent_args(
    params: &SpawnHeadlessParams<'_>,
) -> std::result::Result<(Vec<Cow<'static, str>>, std::path::PathBuf), String> {
    let run_id = params.run_id;
    let working_dir = params.working_dir;
    let prompt = params.prompt;
    let resume_session_id = params.resume_session_id;
    let model = params.model;
    let bot_name = params.bot_name;
    let permission_mode = params.permission_mode;
    let extra_plugin_dirs = params.plugin_dirs;
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

    Ok((args, prompt_file_path))
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

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
    fn build_agent_args_short_prompt_uses_file() {
        let run_id = "run-short-1";
        let prompt = "short prompt";
        let args =
            super::build_agent_args(run_id, "/tmp/wt", prompt, None, None, None, &[]).unwrap();
        let expected_path = std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt"));
        assert_file_prompt(&args, prompt, expected_path.to_str().unwrap());
        let _ = std::fs::remove_file(&expected_path);
    }

    #[test]
    fn build_agent_args_long_prompt_uses_file() {
        let run_id = "run-long-99";
        let prompt = "x".repeat(513);
        let args =
            super::build_agent_args(run_id, "/tmp/wt", &prompt, None, None, None, &[]).unwrap();
        let expected_path = std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt"));
        assert_file_prompt(&args, &prompt, expected_path.to_str().unwrap());
        let _ = std::fs::remove_file(&expected_path);
    }

    #[test]
    fn build_agent_args_prompt_file_in_temp_dir() {
        let run_id = "run-tempdir-01";
        let prompt = "any length prompt";
        let args =
            super::build_agent_args(run_id, "/tmp/wt", prompt, None, None, None, &[]).unwrap();

        // --prompt-file must be present, --prompt must not
        let file_idx = args
            .iter()
            .position(|a| a == "--prompt-file")
            .expect("--prompt-file flag missing");
        assert!(
            !args.iter().any(|a| a == "--prompt"),
            "--prompt should not appear"
        );

        let file_path = std::path::PathBuf::from(args[file_idx + 1].as_ref());

        // File must be inside temp_dir()
        assert_eq!(
            file_path.parent(),
            Some(std::env::temp_dir().as_path()),
            "prompt file must be in temp_dir()"
        );

        // File must exist with correct content
        assert!(file_path.exists(), "prompt file should exist");
        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), prompt);

        // Consumer cleanup: read_and_maybe_cleanup_prompt_file should delete it
        // (tested via conductor-cli helpers, verified here end-to-end)
        let _ = std::fs::remove_file(&file_path);
        assert!(!file_path.exists(), "file should be removed after cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn build_agent_args_prompt_file_mode_0o600() {
        use std::os::unix::fs::MetadataExt;
        let run_id = "run-perm-600-01";
        let args =
            super::build_agent_args(run_id, "/tmp/wt", "secret prompt", None, None, None, &[])
                .unwrap();
        let file_idx = args
            .iter()
            .position(|a| a == "--prompt-file")
            .expect("--prompt-file flag missing");
        let file_path = std::path::Path::new(args[file_idx + 1].as_ref());
        let mode = std::fs::metadata(file_path)
            .expect("prompt file must exist")
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "prompt file must have mode 0o600, got {:#o}",
            mode & 0o777
        );
        let _ = std::fs::remove_file(file_path);
    }

    #[test]
    fn build_agent_args_with_resume_sets_flag() {
        let run_id = "run-resume-sets-flag";
        let prompt = "short prompt";
        let args =
            super::build_agent_args(run_id, "/tmp/wt", prompt, Some("sess-abc"), None, None, &[])
                .unwrap();
        let resume_idx = args
            .iter()
            .position(|a| a == "--resume")
            .expect("--resume flag missing");
        assert_eq!(args[resume_idx + 1], "sess-abc");
        let _ = std::fs::remove_file(
            std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt")),
        );
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
        // SkipPermissions and AutoMode must NOT inject their claude-level flags
        // (--dangerously-skip-permissions / --enable-auto-mode) into the
        // `conductor agent run` arg list — those flags are unknown to conductor
        // and would cause an "unexpected argument" clap error.  The flags are
        // applied later inside run_agent() when the claude subprocess is spawned.
        use crate::config::AgentPermissionMode;
        let run_id = "run-mode-skip-perms";
        let args = super::build_agent_args_with_mode(
            run_id,
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
            !args.iter().any(|a| a == "--dangerously-skip-permissions"),
            "conductor args must not contain --dangerously-skip-permissions (belongs on claude CLI)"
        );
        assert!(
            !args.iter().any(|a| a == "--permission-mode"),
            "SkipPermissions should not emit --permission-mode"
        );
        let _ = std::fs::remove_file(
            std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt")),
        );
    }

    #[test]
    fn build_agent_args_with_mode_auto_mode() {
        use crate::config::AgentPermissionMode;
        let run_id = "run-mode-auto-mode";
        let args = super::build_agent_args_with_mode(
            run_id,
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
            !args.iter().any(|a| a == "--enable-auto-mode"),
            "conductor args must not contain --enable-auto-mode (belongs on claude CLI)"
        );
        assert!(
            !args.iter().any(|a| a == "--permission-mode"),
            "AutoMode should not emit --permission-mode"
        );
        let _ = std::fs::remove_file(
            std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt")),
        );
    }

    #[test]
    fn build_agent_args_with_mode_plan() {
        use crate::config::AgentPermissionMode;
        let run_id = "run-mode-plan-01";
        let args = super::build_agent_args_with_mode(
            run_id,
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
        let _ = std::fs::remove_file(
            std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt")),
        );
    }

    #[test]
    fn build_agent_args_with_mode_repo_safe() {
        use crate::config::AgentPermissionMode;
        let run_id = "run-mode-repo-safe";
        let args = super::build_agent_args_with_mode(
            run_id,
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
        let _ = std::fs::remove_file(
            std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt")),
        );
    }

    #[test]
    fn build_agent_args_non_plan_no_allowed_tools() {
        use crate::config::AgentPermissionMode;
        // RepoSafe is excluded: its allowed_tools() is applied in run_agent(), not here.
        for (mode, run_id) in &[
            (AgentPermissionMode::SkipPermissions, "run-no-tools-skip"),
            (AgentPermissionMode::AutoMode, "run-no-tools-auto"),
        ] {
            let args = super::build_agent_args_with_mode(
                run_id,
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
            let _ = std::fs::remove_file(
                std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt")),
            );
        }
    }

    #[test]
    fn build_agent_args_with_mode_none() {
        let run_id = "run-mode-none-01";
        let args = super::build_agent_args_with_mode(
            run_id,
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
        let _ = std::fs::remove_file(
            std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt")),
        );
    }

    #[test]
    fn build_agent_args_with_model_override() {
        let run_id = "run-model-override";
        let args = super::build_agent_args_with_mode(
            run_id,
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
        let _ = std::fs::remove_file(
            std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt")),
        );
    }

    #[test]
    fn build_agent_args_with_bot_name() {
        let run_id = "run-bot-name-01";
        let args = super::build_agent_args_with_mode(
            run_id,
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
        let _ = std::fs::remove_file(
            std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt")),
        );
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
        let _ = std::fs::remove_file(std::env::temp_dir().join("conductor-prompt-run-all.txt"));
    }

    fn test_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        conn
    }

    #[test]
    fn build_headless_agent_args_includes_run_id_and_worktree() {
        let (args, _prompt_file) = super::build_headless_agent_args(&super::SpawnHeadlessParams {
            run_id: "run-h-1",
            working_dir: "/tmp/wt",
            prompt: "test prompt",
            resume_session_id: None,
            model: None,
            bot_name: None,
            permission_mode: None,
            plugin_dirs: &[],
        })
        .unwrap();
        let pos = args.iter().position(|a| a == "--run-id").unwrap();
        assert_eq!(args[pos + 1], "run-h-1");
        let pos = args.iter().position(|a| a == "--worktree-path").unwrap();
        assert_eq!(args[pos + 1], "/tmp/wt");
    }

    #[test]
    fn build_headless_agent_args_with_all_options() {
        use crate::config::AgentPermissionMode;
        let (args, _prompt_file) = super::build_headless_agent_args(&super::SpawnHeadlessParams {
            run_id: "run-h-2",
            working_dir: "/tmp/wt",
            prompt: "test prompt",
            resume_session_id: Some("sess-abc"),
            model: Some("claude-opus-4-6"),
            bot_name: Some("bot-y"),
            permission_mode: Some(&AgentPermissionMode::Plan),
            plugin_dirs: &["dir1".to_string()],
        })
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
        let (args, prompt_file) = super::build_headless_agent_args(&super::SpawnHeadlessParams {
            run_id: "run-h-3",
            working_dir: "/tmp/wt",
            prompt: "hello world",
            resume_session_id: None,
            model: None,
            bot_name: None,
            permission_mode: None,
            plugin_dirs: &[],
        })
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

    #[test]
    fn drain_stream_json_result_persists_cost_turns_duration() {
        let conn = test_db();
        let mgr = crate::agent::AgentManager::new(&conn);
        let run = mgr.create_run(None, "test prompt", None, None).unwrap();

        // Result event with cost, turns, duration, and final token usage
        let json_lines = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\",\"model\":\"claude-test\",\"session_id\":\"sess-drain-1\"}\n",
            "{\"type\":\"result\",\"is_error\":false,\"result\":\"task complete\",\"session_id\":\"sess-drain-1\",",
            "\"total_cost_usd\":0.05,\"num_turns\":3,\"duration_ms\":5000,",
            "\"usage\":{\"input_tokens\":200,\"output_tokens\":100,",
            "\"cache_read_input_tokens\":40,\"cache_creation_input_tokens\":20}}\n",
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

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.status, crate::agent::AgentRunStatus::Completed);
        assert_eq!(fetched.result_text.as_deref(), Some("task complete"));
        assert_eq!(fetched.claude_session_id.as_deref(), Some("sess-drain-1"));
        assert_eq!(fetched.cost_usd, Some(0.05));
        assert_eq!(fetched.num_turns, Some(3));
        assert_eq!(fetched.duration_ms, Some(5000));
        assert_eq!(fetched.input_tokens, Some(200));
        assert_eq!(fetched.output_tokens, Some(100));
        assert_eq!(fetched.cache_read_input_tokens, Some(40));
        assert_eq!(fetched.cache_creation_input_tokens, Some(20));
    }

    /// Verify that `abort()` terminates a non-I/O-blocked child (e.g. `sleep 60`)
    /// promptly.  Without `kill()` before `wait()`, `abort()` would block until
    /// the child exits naturally — i.e. for 60 seconds.
    #[cfg(unix)]
    #[test]
    fn abort_kills_non_io_blocked_child() {
        use std::process::{Command, Stdio};
        use std::time::Instant;

        let child = Command::new("sleep")
            .arg("60")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn sleep 60");

        let handle = super::HeadlessHandle::from_child(child).expect("from_child failed");

        let start = Instant::now();
        handle.abort();
        let elapsed = start.elapsed();

        assert!(
            elapsed.as_secs() < 5,
            "abort() took {:?} — kill() before wait() is required to avoid blocking",
            elapsed
        );
    }

    /// Verify that `abort()` terminates a child that is filling its stdout pipe
    /// (e.g. `yes`) promptly, even when the caller never reads from stdout.
    /// The pipe buffer fills and the child blocks on write; `kill()` must be sent
    /// so the child can exit rather than waiting for the buffer to drain.
    #[cfg(unix)]
    #[test]
    fn abort_kills_pipe_filling_child() {
        use std::process::{Command, Stdio};
        use std::time::Instant;

        let child = Command::new("/bin/sh")
            .args(["-c", "yes"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn yes via /bin/sh");

        let handle = super::HeadlessHandle::from_child(child).expect("from_child failed");

        // Do NOT read stdout — let the pipe buffer fill so the child blocks on write.
        let start = Instant::now();
        handle.abort();
        let elapsed = start.elapsed();

        assert!(
            elapsed.as_secs() < 5,
            "abort() took {:?} on a pipe-filling child — kill() before wait() is required",
            elapsed
        );
    }
}

// cancel_subprocess tests have moved to crate::process_utils (the canonical home
// for OS-level process utilities). See process_utils.rs for the test coverage.
