//! Shared runtime helpers for spawning and polling agent runs in tmux.
//!
//! Used by both `orchestrator.rs` (plan-step orchestration) and
//! `workflow.rs` (workflow engine execution).

use std::process::Command;
use std::thread;
use std::time::Duration;

use rusqlite::Connection;

use crate::agent::{AgentManager, AgentRun, AgentRunStatus};

/// After a successful `tmux new-window`, wait briefly and verify the window
/// actually exists. Returns `Ok(())` if the window is alive, or an `Err`
/// describing the failure.
pub fn verify_tmux_window(window_name: &str) -> std::result::Result<(), String> {
    // Give tmux a moment to register the window.
    thread::sleep(Duration::from_millis(100));

    let output = Command::new("tmux")
        .args(["list-windows", "-a", "-F", "#{window_name}"])
        .output()
        .map_err(|e| format!("Failed to list tmux windows: {e}"))?;

    if !output.status.success() {
        return Err("tmux list-windows failed — tmux may not be running".to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.lines().any(|line| line.trim() == window_name) {
        Ok(())
    } else {
        Err(format!(
            "tmux window '{window_name}' not found after spawn — agent process may have exited immediately"
        ))
    }
}

/// Poll the database for a child run to reach a terminal status.
pub fn poll_child_completion(
    conn: &Connection,
    child_run_id: &str,
    poll_interval: Duration,
    timeout: Duration,
) -> std::result::Result<AgentRun, String> {
    let start = std::time::Instant::now();

    loop {
        if start.elapsed() > timeout {
            return Err(format!(
                "Child run {} timed out after {:.0}s",
                child_run_id,
                timeout.as_secs_f64()
            ));
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
                return Err(format!("Child run {child_run_id} not found in database"));
            }
            Err(e) => {
                return Err(format!("Database error polling child run: {e}"));
            }
        }

        thread::sleep(poll_interval);
    }
}

/// Spawn a child agent in a new tmux window.
pub fn spawn_child_tmux(
    conductor_bin: &str,
    run_id: &str,
    worktree_path: &str,
    prompt: &str,
    model: Option<&str>,
    window_name: &str,
) -> std::result::Result<(), String> {
    let mut args = vec![
        "agent".to_string(),
        "run".to_string(),
        "--run-id".to_string(),
        run_id.to_string(),
        "--worktree-path".to_string(),
        worktree_path.to_string(),
        "--prompt".to_string(),
        prompt.to_string(),
    ];

    if let Some(m) = model {
        args.push("--model".to_string());
        args.push(m.to_string());
    }

    let mut tmux_args = vec![
        "new-window".to_string(),
        "-d".to_string(),
        "-n".to_string(),
        window_name.to_string(),
        "--".to_string(),
        conductor_bin.to_string(),
    ];
    tmux_args.extend(args);

    let result = Command::new("tmux")
        .args(&tmux_args)
        .output()
        .map_err(|e| format!("Failed to spawn tmux: {e}"))?;

    if result.status.success() {
        // Verify the window actually exists after spawn.
        verify_tmux_window(window_name)
    } else {
        let stderr = String::from_utf8_lossy(&result.stderr);
        Err(format!("tmux failed: {stderr}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_tmux_window_rejects_nonexistent_window() {
        // Whether or not tmux is running, a bogus window name should fail.
        let result = verify_tmux_window("conductor-test-nonexistent-xyz-99999");
        assert!(result.is_err());
    }
}
