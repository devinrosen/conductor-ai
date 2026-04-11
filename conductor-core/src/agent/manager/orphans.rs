use std::time::{Duration, SystemTime};

use crate::db::{active_workflow_parent_run_ids, query_collect};
use crate::error::Result;

use super::super::db::{row_to_agent_run, AGENT_RUN_SELECT};
use super::super::log_parsing::try_recover_from_log;
use super::super::tmux::list_live_tmux_windows;
use super::AgentManager;

/// Remove stale `/tmp/conductor-agent-*.err` files older than 1 hour.
///
/// Best-effort: silently ignores any I/O errors (permissions, concurrent delete, etc.).
fn cleanup_stale_stderr_files() {
    let one_hour = Duration::from_secs(3600);
    let Ok(entries) = std::fs::read_dir("/tmp") else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with("conductor-agent-") || !name_str.ends_with(".err") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if SystemTime::now()
            .duration_since(modified)
            .unwrap_or(Duration::ZERO)
            > one_hour
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

impl<'a> AgentManager<'a> {
    /// Reap orphaned agent runs whose tmux windows have disappeared.
    ///
    /// Queries all runs with an active status (`running` or `waiting_for_feedback`),
    /// checks whether their tmux window still exists, and for any orphans:
    /// 1. Attempts log-file recovery via `try_recover_from_log()` (the agent may
    ///    have completed but the handler didn't fire).
    /// 2. If no result is found in the log, marks the run as `failed`.
    ///
    /// Also cleans up stale stderr capture files older than 1 hour.
    ///
    /// Returns the number of orphaned runs that were reaped.
    pub fn reap_orphaned_runs(&self) -> Result<usize> {
        let active_runs = query_collect(
            self.conn,
            &format!("{AGENT_RUN_SELECT} WHERE status IN ('running', 'waiting_for_feedback')"),
            [],
            row_to_agent_run,
        )?;

        if active_runs.is_empty() {
            return Ok(0);
        }

        tracing::debug!(
            "reap_orphaned_runs: checking {} active agent run(s)",
            active_runs.len()
        );

        // Fetch parent_run_ids of active (non-terminal) workflow runs.
        // Workflow parent runs are created with tmux_window = None by design
        // and must not be reaped while their workflow is still active.
        let active_wf_parent_ids = active_workflow_parent_run_ids(self.conn)?;

        // Fetch all live tmux window names once (avoids N+1 subprocess spawns).
        let live_windows = list_live_tmux_windows();
        tracing::debug!(
            "reap_orphaned_runs: {} live tmux window(s)",
            live_windows.len()
        );

        let mut reaped = 0;
        for run in &active_runs {
            // 1. Skip runs that are parent runs of active workflows.
            if active_wf_parent_ids.contains(&run.id) {
                continue;
            }

            // 2. Headless subprocess run — check PID liveness via kill(0).
            #[cfg(unix)]
            if let Some(pid) = run.subprocess_pid {
                if crate::process_utils::pid_is_alive(pid as u32) {
                    // PID is alive — guard against PID reuse by comparing the OS-recorded
                    // process start time against run.started_at (macOS only).
                    // If start times differ by >60 s, the PID was recycled by the OS after
                    // the original subprocess exited and should be reaped.
                    #[cfg(target_os = "macos")]
                    let pid_recycled = {
                        crate::process_utils::process_started_at(pid as u32)
                            .and_then(|proc_start| {
                                let run_start =
                                    chrono::DateTime::parse_from_rfc3339(&run.started_at).ok()?;
                                let proc_secs = proc_start
                                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                                    .map(|d| d.as_secs() as i64)
                                    .unwrap_or(0);
                                Some((proc_secs - run_start.timestamp()).abs() > 60)
                            })
                            .unwrap_or(false)
                    };
                    #[cfg(not(target_os = "macos"))]
                    let pid_recycled = false;

                    if pid_recycled {
                        tracing::warn!(
                            "reap_orphaned_runs: PID {pid} recycled for run {} (started_at={})",
                            run.id,
                            run.started_at,
                        );
                        // Fall through to reap with recycled-PID message.
                        if try_recover_from_log(self, &run.id).is_some() {
                            tracing::info!(
                                "reap_orphaned_runs: recovered result from log for run {}",
                                run.id
                            );
                            reaped += 1;
                            continue;
                        }
                        self.update_run_failed(
                            &run.id,
                            "subprocess PID recycled — agent may have completed but result was not captured",
                        )?;
                        reaped += 1;
                        continue;
                    }
                    // Start time is consistent — process is genuinely still running.
                    continue;
                }
                tracing::warn!(
                    "reap_orphaned_runs: subprocess pid {pid} gone for run {} (started_at={}, worktree={:?})",
                    run.id,
                    run.started_at,
                    run.worktree_id,
                );
                // PID is dead — try log recovery first, then mark failed.
                if try_recover_from_log(self, &run.id).is_some() {
                    tracing::info!(
                        "reap_orphaned_runs: recovered result from log for run {}",
                        run.id
                    );
                    reaped += 1;
                    continue;
                }
                tracing::warn!(
                    "reap_orphaned_runs: no log recovery for run {}, marking as failed",
                    run.id
                );
                self.update_run_failed(
                    &run.id,
                    "subprocess exited unexpectedly — agent may have completed but result was not captured",
                )?;
                reaped += 1;
                continue;
            }

            // 3. tmux-based run — check window liveness.
            if let Some(ref name) = run.tmux_window {
                if live_windows.contains(name.as_str()) {
                    continue;
                }
                tracing::warn!(
                    "reap_orphaned_runs: tmux window {name:?} gone for run {} (started_at={}, worktree={:?})",
                    run.id,
                    run.started_at,
                    run.worktree_id,
                );
            } else {
                // 4. Neither subprocess_pid nor tmux_window — always reap.
                tracing::warn!(
                    "reap_orphaned_runs: run {} has no tmux_window (started_at={}, worktree={:?})",
                    run.id,
                    run.started_at,
                    run.worktree_id,
                );
            }
            // Window is gone — try to recover result from log file
            if try_recover_from_log(self, &run.id).is_some() {
                tracing::info!(
                    "reap_orphaned_runs: recovered result from log for run {}",
                    run.id
                );
                reaped += 1;
                continue;
            }
            // No result in log — mark as failed
            tracing::warn!(
                "reap_orphaned_runs: no log recovery possible for run {}, marking as failed",
                run.id
            );
            self.update_run_failed(
                &run.id,
                "tmux session lost — agent may have completed but result was not captured",
            )?;
            reaped += 1;
        }

        if reaped > 0 {
            tracing::info!("reap_orphaned_runs: reaped {reaped} orphaned run(s)");
        }

        // Best-effort cleanup of stale stderr capture files (older than 1 hour).
        cleanup_stale_stderr_files();

        Ok(reaped)
    }
}

#[cfg(test)]
mod tests {
    use super::super::setup_db;
    use super::super::AgentManager;
    use crate::agent::status::AgentRunStatus;
    use rusqlite::params;

    #[test]
    fn test_reap_orphaned_runs_no_tmux_window() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "test prompt", None, None)
            .unwrap();
        assert_eq!(run.status, AgentRunStatus::Running);

        let reaped = mgr.reap_orphaned_runs().unwrap();
        assert_eq!(reaped, 1);

        let updated = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(updated.status, AgentRunStatus::Failed);
        assert!(updated
            .result_text
            .as_deref()
            .unwrap()
            .contains("tmux session lost"));
        assert!(updated.ended_at.is_some());
    }

    #[test]
    fn test_reap_orphaned_runs_nonexistent_tmux_window() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(
                Some("w1"),
                "test prompt",
                Some("nonexistent-window-xyz-999"),
                None,
            )
            .unwrap();
        assert_eq!(run.status, AgentRunStatus::Running);

        let reaped = mgr.reap_orphaned_runs().unwrap();
        assert_eq!(reaped, 1);

        let updated = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(updated.status, AgentRunStatus::Failed);
    }

    #[test]
    fn test_reap_orphaned_runs_skips_completed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "test prompt", None, None)
            .unwrap();
        mgr.update_run_completed(
            &run.id,
            None,
            Some("Done"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let reaped = mgr.reap_orphaned_runs().unwrap();
        assert_eq!(reaped, 0);
    }

    /// A run that is the parent_run_id of an active workflow run must NOT be
    /// reaped, even if it has no tmux_window. Workflow parent runs are created
    /// without a tmux window by design and are long-lived while the workflow
    /// executes.
    #[test]
    fn test_reap_orphaned_runs_skips_active_workflow_parent() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Create an agent run with no tmux window (would normally be reaped).
        let parent_run = mgr
            .create_run(Some("w1"), "workflow parent", None, None)
            .unwrap();
        assert_eq!(parent_run.status, AgentRunStatus::Running);

        // Insert an active workflow run referencing this agent run as its parent.
        let wf_run_id = crate::new_id();
        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, started_at) \
             VALUES (?1, 'test-wf', NULL, ?2, 'running', 0, 'manual', '2025-01-01T00:00:00Z')",
            params![wf_run_id, parent_run.id],
        )
        .unwrap();

        // The parent run should be skipped by the reaper.
        let reaped = mgr.reap_orphaned_runs().unwrap();
        assert_eq!(reaped, 0, "active workflow parent run must not be reaped");

        let after = mgr.get_run(&parent_run.id).unwrap().unwrap();
        assert_eq!(
            after.status,
            AgentRunStatus::Running,
            "status must remain running"
        );
    }

    #[test]
    fn test_reap_orphaned_runs_multiple() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let r1 = mgr.create_run(Some("w1"), "prompt 1", None, None).unwrap();
        let r2 = mgr.create_run(Some("w1"), "prompt 2", None, None).unwrap();
        let r3 = mgr.create_run(Some("w1"), "prompt 3", None, None).unwrap();
        mgr.update_run_completed(
            &r3.id,
            None,
            Some("Done"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let reaped = mgr.reap_orphaned_runs().unwrap();
        assert_eq!(reaped, 2);

        assert_eq!(
            mgr.get_run(&r1.id).unwrap().unwrap().status,
            AgentRunStatus::Failed
        );
        assert_eq!(
            mgr.get_run(&r2.id).unwrap().unwrap().status,
            AgentRunStatus::Failed
        );
        assert_eq!(
            mgr.get_run(&r3.id).unwrap().unwrap().status,
            AgentRunStatus::Completed
        );
    }

    /// A run with a subprocess_pid pointing to a dead process should be reaped.
    #[cfg(unix)]
    #[test]
    fn test_reap_orphaned_runs_subprocess_pid_dead() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Spawn a short-lived child, record its PID, wait for it to exit.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = child.id();
        // Wait for it to finish so the PID is definitely gone.
        child.wait().unwrap();
        // Give the OS a moment to fully reap the child.
        std::thread::sleep(std::time::Duration::from_millis(50));

        let run = mgr
            .create_run(Some("w1"), "headless task", None, None)
            .unwrap();
        // Set subprocess_pid to the dead PID.
        conn.execute(
            "UPDATE agent_runs SET subprocess_pid = ?1 WHERE id = ?2",
            params![dead_pid as i64, run.id],
        )
        .unwrap();

        let reaped = mgr.reap_orphaned_runs().unwrap();
        assert_eq!(reaped, 1);

        let updated = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(updated.status, AgentRunStatus::Failed);
        assert!(updated
            .result_text
            .as_deref()
            .unwrap()
            .contains("subprocess exited unexpectedly"));
    }

    /// A run with a subprocess_pid pointing to a live process whose start time is years in the
    /// past (simulating PID reuse) must be reaped with the "PID recycled" message.
    #[cfg(all(test, target_os = "macos"))]
    #[test]
    fn test_reap_orphaned_runs_subprocess_pid_recycled() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Spawn a long-lived child — its PID is alive, but we'll tell the reaper
        // it started years ago to simulate OS PID reuse.
        let mut child = std::process::Command::new("sleep")
            .arg("600")
            .spawn()
            .unwrap();
        let live_pid = child.id();

        let run = mgr
            .create_run(Some("w1"), "headless task recycled", None, None)
            .unwrap();

        // Backdate started_at to 2020 — far outside the 60-second tolerance.
        conn.execute(
            "UPDATE agent_runs SET subprocess_pid = ?1, started_at = ?2 WHERE id = ?3",
            rusqlite::params![live_pid as i64, "2020-01-01T00:00:00Z", run.id],
        )
        .unwrap();

        let reaped = mgr.reap_orphaned_runs().unwrap();

        // Always kill the child, even if the assertion below panics.
        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(reaped, 1, "recycled PID run should have been reaped");

        let updated = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(updated.status, AgentRunStatus::Failed);
        assert!(
            updated
                .result_text
                .as_deref()
                .unwrap()
                .contains("PID recycled"),
            "result_text should mention PID recycled"
        );
    }

    /// A run with a subprocess_pid pointing to the current (live) process must NOT be reaped.
    #[cfg(unix)]
    #[test]
    fn test_reap_orphaned_runs_subprocess_pid_alive_skips() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let live_pid = std::process::id();

        let run = mgr
            .create_run(Some("w1"), "headless task alive", None, None)
            .unwrap();
        conn.execute(
            "UPDATE agent_runs SET subprocess_pid = ?1 WHERE id = ?2",
            params![live_pid as i64, run.id],
        )
        .unwrap();

        let reaped = mgr.reap_orphaned_runs().unwrap();
        assert_eq!(reaped, 0, "live subprocess must not be reaped");

        let after = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(after.status, AgentRunStatus::Running);
    }
}
