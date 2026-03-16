//! Shared runtime helpers for spawning and polling agent runs in tmux.
//!
//! Used by both `orchestrator.rs` (plan-step orchestration) and
//! `workflow.rs` (workflow engine execution).

use std::process::Command;
use std::thread;
use std::time::Duration;

use rusqlite::Connection;

use crate::agent::{list_live_tmux_windows, AgentManager, AgentRun, AgentRunStatus};

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
pub fn spawn_tmux_window(args: &[String], window_name: &str) -> std::result::Result<(), String> {
    let conductor_bin = resolve_conductor_bin();

    let mut tmux_args = vec![
        "new-window".to_string(),
        "-d".to_string(),
        "-n".to_string(),
        window_name.to_string(),
        "--".to_string(),
        conductor_bin.clone(),
    ];
    tmux_args.extend_from_slice(args);

    let result = Command::new("tmux")
        .args(&tmux_args)
        .output()
        .map_err(|e| format!("Failed to spawn tmux: {e}"))?;

    if result.status.success() {
        return verify_tmux_window(window_name);
    }

    // No tmux server running — create a detached session and retry.
    // tmux error messages for a missing server vary across versions and platforms
    // ("no server running on …", "error connecting to …", "No such file or directory"),
    // so we attempt the session fallback on any new-window failure.
    let mut session_args = vec![
        "new-session".to_string(),
        "-d".to_string(),
        "-s".to_string(),
        "conductor".to_string(),
        "-n".to_string(),
        window_name.to_string(),
        "--".to_string(),
        conductor_bin,
    ];
    session_args.extend_from_slice(args);

    let retry = Command::new("tmux")
        .args(&session_args)
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
pub fn poll_child_completion(
    conn: &Connection,
    child_run_id: &str,
    poll_interval: Duration,
    timeout: Duration,
    shutdown: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> std::result::Result<AgentRun, PollError> {
    let start = std::time::Instant::now();

    loop {
        if let Some(flag) = shutdown {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(PollError::Shutdown);
            }
        }

        if start.elapsed() > timeout {
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
                AgentRunStatus::Running | AgentRunStatus::WaitingForFeedback => {}
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

/// Spawn a child agent in a new tmux window.
pub fn spawn_child_tmux(
    run_id: &str,
    worktree_path: &str,
    prompt: &str,
    model: Option<&str>,
    window_name: &str,
    bot_name: Option<&str>,
) -> std::result::Result<(), String> {
    // tmux has a hard limit on command-line length (~2 KB depending on version).
    // For prompts that exceed a safe threshold, write to a file and pass
    // --prompt-file instead so we never hit that limit.
    const PROMPT_FILE_THRESHOLD: usize = 512;

    let prompt_file_path: Option<String> = if prompt.len() > PROMPT_FILE_THRESHOLD {
        let path = format!("{worktree_path}/.conductor-prompt-{run_id}.txt");
        std::fs::write(&path, prompt)
            .map_err(|e| format!("Failed to write prompt file '{path}': {e}"))?;
        Some(path)
    } else {
        None
    };

    let mut args = vec![
        "agent".to_string(),
        "run".to_string(),
        "--run-id".to_string(),
        run_id.to_string(),
        "--worktree-path".to_string(),
        worktree_path.to_string(),
    ];

    if let Some(path) = prompt_file_path {
        args.push("--prompt-file".to_string());
        args.push(path);
    } else {
        args.push("--prompt".to_string());
        args.push(prompt.to_string());
    }

    if let Some(m) = model {
        args.push("--model".to_string());
        args.push(m.to_string());
    }

    if let Some(b) = bot_name {
        args.push("--bot-name".to_string());
        args.push(b.to_string());
    }

    spawn_tmux_window(&args, window_name)
}

#[cfg(test)]
mod tests {
    #[test]
    fn verify_tmux_window_rejects_nonexistent_window() {
        // Whether or not tmux is running, a bogus window name should fail.
        let result = super::verify_tmux_window("conductor-test-nonexistent-xyz-99999");
        assert!(result.is_err());
    }
}
