use crate::db::{active_workflow_parent_run_ids, query_collect};
use crate::error::Result;

use super::super::db::{row_to_agent_run, AGENT_RUN_SELECT};
use super::super::log_parsing::try_recover_from_log;
use super::AgentManager;

impl<'a> AgentManager<'a> {
    /// Attempt log recovery for a run, falling back to marking it failed.
    ///
    /// Tries `try_recover_from_log` first; if no result is found in the log,
    /// marks the run as `failed` with `fail_msg`.
    /// Either way, cascades the failure to any associated `workflow_runs` whose
    /// `parent_run_id` points to this agent run.
    fn reap_one(&self, run_id: &str, fail_msg: &str) -> crate::error::Result<()> {
        if try_recover_from_log(self, run_id).is_some() {
            tracing::info!("reap_orphaned_runs: recovered result from log for run {run_id}");
        } else {
            tracing::warn!(
                "reap_orphaned_runs: no log recovery for run {run_id}, marking as failed"
            );
            self.update_run_failed(run_id, fail_msg)?;
        }
        // Cascade to any associated workflow runs, regardless of recovery outcome.
        let wf_reaped = self.fail_child_workflow_runs(run_id)?;
        if wf_reaped > 0 {
            tracing::warn!(
                "reap_orphaned_runs: failed {wf_reaped} workflow run(s) whose parent agent run {run_id} was reaped"
            );
        }
        Ok(())
    }

    /// Mark non-terminal `workflow_runs` whose `parent_run_id` matches the given
    /// agent run as `failed`. This is called after an agent run is reaped so that
    /// any associated workflow runs do not remain stuck in a non-terminal state.
    fn fail_child_workflow_runs(&self, agent_run_id: &str) -> Result<usize> {
        let now = chrono::Utc::now().to_rfc3339();
        let changed = self.conn.execute(
            "UPDATE workflow_runs \
             SET status = 'failed', ended_at = :ended_at, blocked_on = NULL, \
                 error = 'parent agent run was orphaned and reaped' \
             WHERE parent_run_id = :agent_run_id \
               AND status IN ('running', 'waiting', 'pending')",
            rusqlite::named_params! { ":ended_at": now, ":agent_run_id": agent_run_id },
        )?;
        Ok(changed)
    }

    /// Sweep for workflow runs that are stuck in an active state because their
    /// parent agent run has already reached a terminal state (failed, completed,
    /// or cancelled).
    ///
    /// This covers the case where the `active_wf_parent_ids` guard in
    /// `reap_orphaned_runs()` prevented the parent agent run from being reaped
    /// while the workflow was nominally active, but the workflow executor
    /// subsequently died without updating the workflow run status.
    ///
    /// Returns the number of workflow runs transitioned to `failed`.
    pub fn reap_workflow_runs_with_dead_parent(&self) -> Result<usize> {
        let now = chrono::Utc::now().to_rfc3339();
        let changed = self.conn.execute(
            "UPDATE workflow_runs \
             SET status = 'failed', ended_at = :ended_at, blocked_on = NULL, \
                 error = 'parent agent run reached terminal state without completing the workflow' \
             WHERE status IN ('running', 'waiting', 'pending') \
               AND parent_run_id IS NOT NULL \
               AND parent_run_id IN ( \
                   SELECT id FROM agent_runs \
                   WHERE status IN ('failed', 'completed', 'cancelled') \
               )",
            rusqlite::named_params! { ":ended_at": now },
        )?;
        if changed > 0 {
            tracing::warn!(
                "reap_workflow_runs_with_dead_parent: failed {changed} workflow run(s) whose parent agent run is terminal"
            );
        }
        Ok(changed)
    }

    /// Reap orphaned agent runs whose subprocess has exited.
    ///
    /// Queries all runs with an active status (`running` or `waiting_for_feedback`),
    /// checks whether their subprocess is still alive, and for any orphans:
    /// 1. Attempts log-file recovery via `try_recover_from_log()` (the agent may
    ///    have completed but the handler didn't fire).
    /// 2. If no result is found in the log, marks the run as `failed`.
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
            // No active agent runs — still run the workflow sweep in case a previous
            // reap cycle left workflow_runs stuck with a now-terminal parent.
            return self.reap_workflow_runs_with_dead_parent();
        }

        tracing::debug!(
            "reap_orphaned_runs: checking {} active agent run(s)",
            active_runs.len()
        );

        // Fetch parent_run_ids of active (non-terminal) workflow runs.
        // Workflow parent runs are created without a subprocess_pid by design
        // and must not be reaped while their workflow is still active.
        let active_wf_parent_ids = active_workflow_parent_run_ids(self.conn)?;

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
                    // process start time against run.started_at.
                    // If start times differ by >60 s, the PID was recycled by the OS after
                    // the original subprocess exited and should be reaped.
                    if crate::process_utils::pid_was_recycled(pid as u32, &run.started_at) {
                        tracing::warn!(
                            "reap_orphaned_runs: PID {pid} recycled for run {} (started_at={})",
                            run.id,
                            run.started_at,
                        );
                        self.reap_one(
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
                self.reap_one(
                    &run.id,
                    "subprocess exited unexpectedly — agent may have completed but result was not captured",
                )?;
                reaped += 1;
                continue;
            }

            // 3. No subprocess_pid.
            //
            // Guard against a race between the workflow executor spawning the subprocess
            // and storing the PID in the DB:
            //   t0: executor creates run (status=running, subprocess_pid=None)
            //   t1: executor spawns subprocess
            //   t2: subprocess starts and calls reap_orphaned_runs() ← here
            //   t3: executor calls update_run_subprocess_pid()        ← not yet
            //
            // At t2, the run looks orphaned (no PID) but it is alive.
            // We detect this by checking whether the run is a child of an active workflow
            // (i.e. its parent_run_id is a current workflow parent run).  If so, skip it —
            // the PID will be written momentarily by the executor.
            if let Some(ref parent_id) = run.parent_run_id {
                if active_wf_parent_ids.contains(parent_id) {
                    tracing::debug!(
                        "reap_orphaned_runs: skipping run {} — child of active workflow (PID not yet stored)",
                        run.id,
                    );
                    continue;
                }
            }

            tracing::warn!(
                "reap_orphaned_runs: run {} has no subprocess_pid (started_at={}, worktree={:?})",
                run.id,
                run.started_at,
                run.worktree_id,
            );
            self.reap_one(
                &run.id,
                "agent process gone — may have completed but result was not captured",
            )?;
            reaped += 1;
        }

        if reaped > 0 {
            tracing::info!("reap_orphaned_runs: reaped {reaped} orphaned run(s)");
        }

        // Separately sweep for workflow runs whose parent agent run is already
        // terminal (handles the guard-deadlock case where the parent agent run
        // cannot be reaped while the workflow_run is still active).
        reaped += self.reap_workflow_runs_with_dead_parent()?;

        Ok(reaped)
    }
}

#[cfg(test)]
mod tests {
    use super::super::setup_db;
    use super::super::AgentManager;
    use crate::agent::status::AgentRunStatus;
    use rusqlite::named_params;

    #[test]
    fn test_reap_orphaned_runs_no_subprocess_pid() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "test prompt", None).unwrap();
        assert_eq!(run.status, AgentRunStatus::Running);

        let reaped = mgr.reap_orphaned_runs().unwrap();
        assert_eq!(reaped, 1);

        let updated = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(updated.status, AgentRunStatus::Failed);
        assert!(updated
            .result_text
            .as_deref()
            .unwrap()
            .contains("agent process gone"));
        assert!(updated.ended_at.is_some());
    }

    #[test]
    fn test_reap_orphaned_runs_skips_completed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "test prompt", None).unwrap();
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
    /// reaped. Workflow parent runs are created without a subprocess PID by
    /// design and are long-lived while the workflow executes.
    #[test]
    fn test_reap_orphaned_runs_skips_active_workflow_parent() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Create an agent run with no subprocess PID (would normally be reaped).
        let parent_run = mgr.create_run(Some("w1"), "workflow parent", None).unwrap();
        assert_eq!(parent_run.status, AgentRunStatus::Running);

        // Insert an active workflow run referencing this agent run as its parent.
        let wf_run_id = crate::new_id();
        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, started_at) \
             VALUES (:id, 'test-wf', NULL, :parent_run_id, 'running', 0, 'manual', '2025-01-01T00:00:00Z')",
            named_params! { ":id": wf_run_id, ":parent_run_id": parent_run.id },
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

        let r1 = mgr.create_run(Some("w1"), "prompt 1", None).unwrap();
        let r2 = mgr.create_run(Some("w1"), "prompt 2", None).unwrap();
        let r3 = mgr.create_run(Some("w1"), "prompt 3", None).unwrap();
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

        let run = mgr.create_run(Some("w1"), "headless task", None).unwrap();
        // Set subprocess_pid to the dead PID.
        conn.execute(
            "UPDATE agent_runs SET subprocess_pid = :pid WHERE id = :id",
            named_params! { ":pid": dead_pid as i64, ":id": run.id },
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
            .create_run(Some("w1"), "headless task recycled", None)
            .unwrap();

        // Backdate started_at to 2020 — far outside the 60-second tolerance.
        conn.execute(
            "UPDATE agent_runs SET subprocess_pid = :pid, started_at = :started_at WHERE id = :id",
            rusqlite::named_params! { ":pid": live_pid as i64, ":started_at": "2020-01-01T00:00:00Z", ":id": run.id },
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

    /// A child run of an active workflow whose subprocess_pid has not yet been stored
    /// (the spawn-vs-PID-persist race) must NOT be reaped.
    ///
    /// Scenario reproduced by the real workflow failure 01KP0R81BP6J6FMXTR9WR7BKY3:
    ///   1. Workflow executor creates child run (status=running, subprocess_pid=None).
    ///   2. Executor spawns `conductor agent run` subprocess.
    ///   3. Subprocess calls reap_orphaned_runs() before executor writes the PID.
    ///   4. Without this fix the subprocess would reap its own run.
    #[test]
    fn test_reap_orphaned_runs_skips_active_workflow_child_no_pid() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Parent run of an active workflow (no subprocess_pid by design).
        let parent_run = mgr.create_run(Some("w1"), "workflow parent", None).unwrap();

        // Insert an active workflow run whose parent is the run above.
        let wf_run_id = crate::new_id();
        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, started_at) \
             VALUES (:id, 'test-wf', NULL, :parent_run_id, 'running', 0, 'manual', '2025-01-01T00:00:00Z')",
            rusqlite::named_params! { ":id": wf_run_id, ":parent_run_id": parent_run.id },
        )
        .unwrap();

        // Child run (e.g. plan step) — subprocess_pid not yet stored.
        let child_run = mgr
            .create_child_run(Some("w1"), "plan prompt", None, &parent_run.id, None)
            .unwrap();
        assert_eq!(child_run.status, AgentRunStatus::Running);
        assert!(child_run.subprocess_pid.is_none());

        // Neither the parent nor the child should be reaped.
        let reaped = mgr.reap_orphaned_runs().unwrap();
        assert_eq!(
            reaped, 0,
            "active workflow child run (no PID yet) must not be reaped"
        );

        let child_after = mgr.get_run(&child_run.id).unwrap().unwrap();
        assert_eq!(
            child_after.status,
            AgentRunStatus::Running,
            "child run status must remain running"
        );
    }

    /// Helper: create a terminal agent_run (with `parent_status`) and a workflow_run
    /// pointing at it (with `wf_status`). Returns the workflow_run id.
    ///
    /// Used by the dead-parent cascade tests so they share identical setup boilerplate
    /// without copy-pasting the ~10-line block into each test body.
    fn setup_dead_parent_with_wf_run(
        conn: &rusqlite::Connection,
        mgr: &AgentManager,
        parent_status: &str,
        wf_status: &str,
    ) -> String {
        let run = mgr.create_run(Some("w1"), "test prompt", None).unwrap();
        conn.execute(
            "UPDATE agent_runs SET status = :status, ended_at = '2025-01-01T00:01:00Z' WHERE id = :id",
            rusqlite::named_params! { ":status": parent_status, ":id": run.id },
        )
        .unwrap();
        let wf_run_id = crate::new_id();
        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, started_at) \
             VALUES (:id, 'test-wf', NULL, :parent_run_id, :wf_status, 0, 'manual', '2025-01-01T00:00:00Z')",
            rusqlite::named_params! { ":id": wf_run_id, ":parent_run_id": run.id, ":wf_status": wf_status },
        )
        .unwrap();
        wf_run_id
    }

    /// A workflow_run in `running` status whose parent agent_run is already
    /// terminal (failed) must be transitioned to `failed` by
    /// `reap_workflow_runs_with_dead_parent` (called inside `reap_orphaned_runs`).
    ///
    /// Note: the `active_wf_parent_ids` guard prevents agent_runs from being
    /// reaped while they are the parent of a non-terminal workflow_run. This test
    /// therefore pre-terminates the agent_run directly in the DB to simulate the
    /// scenario where the parent died without the workflow_run being updated.
    #[test]
    fn test_reap_orphaned_runs_fails_associated_workflow_run() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let wf_run_id = setup_dead_parent_with_wf_run(&conn, &mgr, "failed", "running");

        // reap_orphaned_runs calls reap_workflow_runs_with_dead_parent internally.
        let reaped = mgr.reap_orphaned_runs().unwrap();
        assert_eq!(reaped, 1, "one workflow_run should be reaped");

        let wf_status: String = conn
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = :id",
                rusqlite::named_params! { ":id": wf_run_id },
                |r: &rusqlite::Row<'_>| r.get("status"),
            )
            .unwrap();
        assert_eq!(wf_status, "failed");

        let wf_error: Option<String> = conn
            .query_row(
                "SELECT error FROM workflow_runs WHERE id = :id",
                rusqlite::named_params! { ":id": wf_run_id },
                |r: &rusqlite::Row<'_>| r.get("error"),
            )
            .unwrap();
        assert!(wf_error
            .as_deref()
            .unwrap()
            .contains("parent agent run reached terminal state"));
    }

    /// A workflow_run in `waiting` status (which may carry a `blocked_on` value)
    /// must be transitioned to `failed` and have `blocked_on` cleared.
    #[test]
    fn test_reap_orphaned_runs_fails_waiting_workflow_run() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let wf_run_id = setup_dead_parent_with_wf_run(&conn, &mgr, "failed", "waiting");
        // Simulate a stale blocked_on value left by the workflow executor.
        conn.execute(
            "UPDATE workflow_runs SET blocked_on = '{\"type\":\"stale\"}' WHERE id = :id",
            rusqlite::named_params! { ":id": wf_run_id },
        )
        .unwrap();

        let reaped = mgr.reap_orphaned_runs().unwrap();
        assert_eq!(reaped, 1);

        let (wf_status, blocked_on): (String, Option<String>) = conn
            .query_row(
                "SELECT status, blocked_on FROM workflow_runs WHERE id = :id",
                rusqlite::named_params! { ":id": wf_run_id },
                |r| Ok((r.get("status")?, r.get("blocked_on")?)),
            )
            .unwrap();
        assert_eq!(wf_status, "failed");
        assert!(blocked_on.is_none(), "blocked_on must be cleared on reap");
    }

    /// A workflow_run in `pending` status whose parent agent_run is terminal must
    /// be reaped — `pending` appears in both SQL IN-lists but previously had no test.
    #[test]
    fn test_reap_orphaned_runs_fails_pending_workflow_run() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let wf_run_id = setup_dead_parent_with_wf_run(&conn, &mgr, "failed", "pending");

        let reaped = mgr.reap_orphaned_runs().unwrap();
        assert_eq!(reaped, 1, "pending workflow_run should be reaped");

        let wf_status: String = conn
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = :id",
                rusqlite::named_params! { ":id": wf_run_id },
                |r: &rusqlite::Row<'_>| r.get("status"),
            )
            .unwrap();
        assert_eq!(wf_status, "failed");
    }

    /// `reap_workflow_runs_with_dead_parent` must cascade for a `completed` parent,
    /// not just a `failed` one.
    #[test]
    fn test_reap_wf_runs_dead_parent_completed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let wf_run_id = setup_dead_parent_with_wf_run(&conn, &mgr, "completed", "running");

        let reaped = mgr.reap_workflow_runs_with_dead_parent().unwrap();
        assert_eq!(
            reaped, 1,
            "workflow_run with completed parent should be reaped"
        );

        let wf_status: String = conn
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = :id",
                rusqlite::named_params! { ":id": wf_run_id },
                |r: &rusqlite::Row<'_>| r.get("status"),
            )
            .unwrap();
        assert_eq!(wf_status, "failed");
    }

    /// `reap_workflow_runs_with_dead_parent` must cascade for a `cancelled` parent.
    #[test]
    fn test_reap_wf_runs_dead_parent_cancelled() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let wf_run_id = setup_dead_parent_with_wf_run(&conn, &mgr, "cancelled", "running");

        let reaped = mgr.reap_workflow_runs_with_dead_parent().unwrap();
        assert_eq!(
            reaped, 1,
            "workflow_run with cancelled parent should be reaped"
        );

        let wf_status: String = conn
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = :id",
                rusqlite::named_params! { ":id": wf_run_id },
                |r: &rusqlite::Row<'_>| r.get("status"),
            )
            .unwrap();
        assert_eq!(wf_status, "failed");
    }

    /// A workflow_run that is already in a terminal state (`completed`) must NOT
    /// be touched even if its parent agent_run is also terminal.
    #[test]
    fn test_reap_orphaned_runs_does_not_affect_terminal_workflow_run() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let wf_run_id = setup_dead_parent_with_wf_run(&conn, &mgr, "failed", "completed");
        // Mark the workflow_run as ended (terminal state).
        conn.execute(
            "UPDATE workflow_runs SET ended_at = '2025-01-01T01:00:00Z' WHERE id = :id",
            rusqlite::named_params! { ":id": wf_run_id },
        )
        .unwrap();

        let reaped = mgr.reap_orphaned_runs().unwrap();
        assert_eq!(reaped, 0, "completed workflow_run must not be reaped");

        let wf_status: String = conn
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = :id",
                rusqlite::named_params! { ":id": wf_run_id },
                |r: &rusqlite::Row<'_>| r.get("status"),
            )
            .unwrap();
        assert_eq!(wf_status, "completed");
    }

    /// A run with a subprocess_pid pointing to the current (live) process must NOT be reaped.
    #[cfg(unix)]
    #[test]
    fn test_reap_orphaned_runs_subprocess_pid_alive_skips() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let live_pid = std::process::id();

        let run = mgr
            .create_run(Some("w1"), "headless task alive", None)
            .unwrap();
        conn.execute(
            "UPDATE agent_runs SET subprocess_pid = :pid WHERE id = :id",
            rusqlite::named_params! { ":pid": live_pid as i64, ":id": run.id },
        )
        .unwrap();

        // On macOS the PID reuse guard computes abs(process_started_at(pid) - run.started_at).
        // The cargo test process has been running for much longer than the 60s threshold, so
        // create_run()'s "now()" started_at would trigger a false-positive reap. Fix: update
        // started_at to the kernel-reported start time of this exact PID so the delta is <1s.
        #[cfg(target_os = "macos")]
        {
            if let Some(proc_start) = crate::process_utils::process_started_at(live_pid) {
                let started_at_str = chrono::DateTime::<chrono::Utc>::from(proc_start).to_rfc3339();
                conn.execute(
                    "UPDATE agent_runs SET started_at = :started_at WHERE id = :id",
                    rusqlite::named_params! { ":started_at": started_at_str, ":id": run.id },
                )
                .unwrap();
            }
        }

        let reaped = mgr.reap_orphaned_runs().unwrap();
        assert_eq!(reaped, 0, "live subprocess must not be reaped");

        let after = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(after.status, AgentRunStatus::Running);
    }
}
