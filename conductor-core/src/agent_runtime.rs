//! Shared runtime helpers for spawning and polling agent runs in tmux.
//!
//! Used by both `orchestrator.rs` (plan-step orchestration) and
//! `workflow.rs` (workflow engine execution).

use std::borrow::Cow;
use std::process::Command;
use std::thread;
use std::time::Duration;

use rusqlite::Connection;

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
    std::env::current_exe()
        .ok()
        .and_then(|p| {
            let sibling = p.parent()?.join("conductor");
            sibling
                .exists()
                .then(|| sibling.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "conductor".to_string())
}

/// Spawn a new tmux window running `conductor <args>`, then verify it is alive.
///
/// `args` are the arguments passed to the `conductor` binary (e.g.
/// `["agent", "run", "--run-id", …]`).  `window_name` is used as the tmux
/// window name (`-n`) and for post-spawn verification.
///
/// If no tmux server is running, a detached session named `conductor` is
/// created automatically so agents can run without a pre-existing tmux session.
pub fn spawn_tmux_window(
    args: &[Cow<'static, str>],
    window_name: &str,
) -> std::result::Result<(), String> {
    let conductor_bin = resolve_conductor_bin();

    let mut tmux_args: Vec<Cow<'static, str>> = vec![
        Cow::Borrowed("new-window"),
        Cow::Borrowed("-d"),
        Cow::Borrowed("-n"),
        Cow::Owned(window_name.to_string()),
        Cow::Borrowed("--"),
        Cow::Owned(conductor_bin.clone()),
    ];
    tmux_args.extend_from_slice(args);

    let result = Command::new("tmux")
        .args(tmux_args.iter().map(|a| a.as_ref()))
        .output()
        .map_err(|e| format!("Failed to spawn tmux: {e}"))?;

    if result.status.success() {
        return verify_tmux_window(window_name);
    }

    // No tmux server running — create a detached session and retry.
    // tmux error messages for a missing server vary across versions and platforms
    // ("no server running on …", "error connecting to …", "No such file or directory"),
    // so we attempt the session fallback on any new-window failure.
    let mut session_args: Vec<Cow<'static, str>> = vec![
        Cow::Borrowed("new-session"),
        Cow::Borrowed("-d"),
        Cow::Borrowed("-s"),
        Cow::Borrowed("conductor"),
        Cow::Borrowed("-n"),
        Cow::Owned(window_name.to_string()),
        Cow::Borrowed("--"),
        Cow::Owned(conductor_bin),
    ];
    session_args.extend_from_slice(args);

    let retry = Command::new("tmux")
        .args(session_args.iter().map(|a| a.as_ref()))
        .output()
        .map_err(|e| format!("Failed to start tmux session: {e}"))?;

    if retry.status.success() {
        return verify_tmux_window(window_name);
    }
    let retry_stderr = String::from_utf8_lossy(&retry.stderr);
    Err(format!("Failed to start tmux session: {retry_stderr}"))
}

/// After a successful `tmux new-window`, wait briefly and verify the window
/// actually exists. Returns `Ok(())` if the window is alive, or an `Err`
/// describing the failure.
fn verify_tmux_window(window_name: &str) -> std::result::Result<(), String> {
    // Give tmux a moment to register the window.
    thread::sleep(Duration::from_millis(100));

    let live = list_live_tmux_windows();
    if live.contains(window_name) {
        Ok(())
    } else {
        Err(format!(
            "tmux window '{window_name}' not found after spawn — agent process may have exited immediately"
        ))
    }
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

        thread::sleep(poll_interval);
    }
}

/// Maximum number of CLI arguments produced by `build_agent_args`:
/// 2 subcommands + 4 fixed flags + 2 for prompt/prompt-file + 2 optional resume
/// + 2 optional model + 2 optional bot_name + 2 optional permission-mode.
const AGENT_ARGS_CAPACITY: usize = 16;

/// Build the `conductor agent run` argument list for a child agent.
///
/// If the prompt exceeds the safe tmux command-length threshold, it is written
/// to a temp file (`<working_dir>/.conductor-prompt-<run_id>.txt`) and
/// `--prompt-file` is used instead of `--prompt`.  Returns the argument list
/// ready to pass to [`spawn_tmux_window`].
///
/// `permission_mode` optionally overrides the configured permission mode
/// (e.g. `Some(AgentPermissionMode::Plan)` for repo-scoped read-only agents).
pub fn build_agent_args(
    run_id: &str,
    worktree_path: &str,
    prompt: &str,
    resume_session_id: Option<&str>,
    model: Option<&str>,
    bot_name: Option<&str>,
) -> std::result::Result<Vec<Cow<'static, str>>, String> {
    build_agent_args_with_mode(
        run_id,
        worktree_path,
        prompt,
        resume_session_id,
        model,
        bot_name,
        None,
    )
}

/// Like [`build_agent_args`] but accepts an optional permission mode override.
///
/// When `permission_mode` is `Some(AgentPermissionMode::Plan)`, the agent run
/// will use `--permission-mode plan` instead of the configured default.
#[allow(clippy::too_many_arguments)]
pub fn build_agent_args_with_mode(
    run_id: &str,
    working_dir: &str,
    prompt: &str,
    resume_session_id: Option<&str>,
    model: Option<&str>,
    bot_name: Option<&str>,
    permission_mode: Option<&crate::config::AgentPermissionMode>,
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

/// Spawn a child agent in a new tmux window.
pub fn spawn_child_tmux(
    run_id: &str,
    worktree_path: &str,
    prompt: &str,
    model: Option<&str>,
    window_name: &str,
    bot_name: Option<&str>,
) -> std::result::Result<(), String> {
    let args = build_agent_args(run_id, worktree_path, prompt, None, model, bot_name)?;
    spawn_tmux_window(&args, window_name)
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

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
        let result = super::verify_tmux_window("conductor-test-nonexistent-xyz-99999");
        assert!(result.is_err());
    }

    #[test]
    fn build_agent_args_short_prompt_uses_inline() {
        let prompt = "short prompt";
        assert!(prompt.len() <= 512);
        let args = super::build_agent_args("run-1", "/tmp/wt", prompt, None, None, None).unwrap();
        assert_inline_prompt(&args, prompt);
    }

    #[test]
    fn build_agent_args_long_prompt_uses_file() {
        let tmp = std::env::temp_dir().join(format!("conductor-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let worktree = tmp.to_str().unwrap();
        let run_id = "run-long-99";

        let prompt = "x".repeat(513);
        let args = super::build_agent_args(run_id, worktree, &prompt, None, None, None).unwrap();

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
        let result = super::build_agent_args("run-err-01", worktree, &prompt, None, None, None);
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
            super::build_agent_args("run-boundary", "/tmp/wt", &prompt, None, None, None).unwrap();
        assert_inline_prompt(&args, &prompt);
    }

    #[test]
    fn build_agent_args_with_resume_sets_flag() {
        let prompt = "short prompt";
        let args =
            super::build_agent_args("run-1", "/tmp/wt", prompt, Some("sess-abc"), None, None)
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
}
