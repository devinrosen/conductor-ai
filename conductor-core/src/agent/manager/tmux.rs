use std::process::Command;

use crate::error::{ConductorError, Result};

use super::AgentManager;

impl<'a> AgentManager<'a> {
    /// Best-effort capture of tmux scrollback to `~/.conductor/agent-logs/<run_id>.log`.
    pub fn capture_agent_log(&self, run_id: &str, tmux_window: &str) {
        let log_dir = crate::config::agent_log_dir();

        if let Err(e) = std::fs::create_dir_all(&log_dir) {
            tracing::warn!("could not create agent-logs dir: {e}");
            return;
        }

        let log_path = crate::config::agent_log_path(run_id);

        let output = Command::new("tmux")
            .args([
                "capture-pane",
                "-t",
                &format!(":{tmux_window}"),
                "-p",
                "-S",
                "-",
            ])
            .output();

        match output {
            Ok(o) if o.status.success() => {
                if let Err(e) = std::fs::write(&log_path, &o.stdout) {
                    tracing::warn!("could not write agent log: {e}");
                    return;
                }
                let path_str = log_path.to_string_lossy().to_string();
                if let Err(e) = self.update_run_log_file(run_id, &path_str) {
                    tracing::warn!(
                        "captured agent log but failed to record path in DB for run {run_id}: {e}"
                    );
                }
            }
            Ok(o) => {
                tracing::warn!(
                    "tmux capture-pane failed for run {run_id} window {tmux_window}: {}",
                    String::from_utf8_lossy(&o.stderr)
                );
            }
            Err(e) => {
                tracing::warn!("could not execute tmux capture-pane for run {run_id}: {e}");
            }
        }
    }

    /// Switch the current tmux client to the given agent window.
    ///
    /// Runs `tmux select-window -t :{window}` and returns an error (including
    /// tmux's stderr) if the command fails or tmux is unavailable.
    pub fn attach_agent_window(&self, window: &str) -> Result<()> {
        let output = Command::new("tmux")
            .args(["select-window", "-t", &format!(":{window}")])
            .output()
            .map_err(|e| ConductorError::Agent(format!("could not execute tmux: {e}")))?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();

        if stderr.is_empty() {
            Err(ConductorError::Agent(
                "tmux select-window failed".to_string(),
            ))
        } else if stderr.contains("No such file or directory")
            || stderr.contains("error connecting to")
            || stderr.contains("no server running")
        {
            Err(ConductorError::Agent(
                "tmux is not running — start a tmux session first, then relaunch conductor"
                    .to_string(),
            ))
        } else {
            Err(ConductorError::Agent(format!(
                "tmux select-window failed: {stderr}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::setup_db;
    use super::*;

    #[test]
    fn test_attach_agent_window_no_tmux_returns_error() {
        // When tmux is not running (no server), attach_agent_window must return
        // a ConductorError::Agent rather than panicking.
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);
        let result = mgr.attach_agent_window("nonexistent-window-xyz");
        assert!(
            result.is_err(),
            "expected error when tmux server is not running"
        );
        match result.unwrap_err() {
            ConductorError::Agent(_) => {}
            other => panic!("expected ConductorError::Agent, got {other:?}"),
        }
    }

    #[test]
    fn test_capture_agent_log_no_tmux_does_not_panic() {
        // capture_agent_log is best-effort (returns void); it must not panic
        // when tmux is unavailable or the window does not exist.
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);
        // Should complete without panicking even when tmux is not available.
        mgr.capture_agent_log("test-run-id-no-tmux", "nonexistent-window-xyz");
    }

    #[test]
    fn test_capture_agent_log_no_log_file_without_tmux() {
        // After capture_agent_log with no tmux, no log file is written.
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);
        let run_id = "test-run-no-log-file";
        mgr.capture_agent_log(run_id, "window-does-not-exist");
        // No log file should be created when tmux capture-pane fails.
        let log_path = crate::config::agent_log_path(run_id);
        assert!(
            !log_path.exists(),
            "expected no log file when tmux is unavailable"
        );
    }
}
