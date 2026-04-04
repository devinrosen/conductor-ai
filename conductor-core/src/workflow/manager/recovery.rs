use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};

use crate::db::{query_collect, with_in_clause};
use crate::error::Result;

use super::helpers::{purge_where_clause, row_to_workflow_run};
use super::WorkflowManager;
use crate::workflow::constants::RUN_COLUMNS;
use crate::workflow::status::{WorkflowRunStatus, WorkflowStepStatus};
use crate::workflow::types::{StepKey, WorkflowRun};

/// A workflow run whose active step has been running longer than the
/// configured threshold without progress.
#[derive(Debug, Clone)]
pub struct StaleWorkflowRun {
    pub run_id: String,
    pub workflow_name: String,
    pub target_label: Option<String>,
    pub step_name: String,
    /// How many minutes the step has been running.
    pub running_minutes: i64,
    /// The workflow_run_steps row ID (needed to mark the step as failed).
    pub step_id: String,
    /// The child agent_run ID for this step (if any).
    pub child_run_id: Option<String>,
    /// The tmux window name for the child agent run (if any).
    pub tmux_window: Option<String>,
}

/// Result of reaping a stale workflow run whose agent process is confirmed dead.
#[derive(Debug, Clone)]
pub struct ReapedStaleRun {
    pub run_id: String,
    pub workflow_name: String,
    pub target_label: Option<String>,
    pub step_name: String,
    pub running_minutes: i64,
}

impl<'a> WorkflowManager<'a> {
    /// Recover steps stuck in `running` status whose child agent run has
    /// already reached a terminal state (completed, failed, or cancelled).
    ///
    /// This handles the case where the executor was killed before the workflow
    /// thread could write the step's final status back to the DB.
    /// Returns the number of steps recovered.
    pub fn recover_stuck_steps(&self) -> Result<usize> {
        // Single JOIN query: avoids N+1 per-step lookups and skips the
        // per-run plan-step fetch that AgentManager::get_run() would do.
        let stuck: Vec<(String, String, String, Option<String>)> = query_collect(
            self.conn,
            "SELECT wrs.id, ar.id, ar.status, ar.result_text \
             FROM workflow_run_steps wrs \
             JOIN agent_runs ar ON ar.id = wrs.child_run_id \
             WHERE wrs.status = 'running' \
               AND ar.status IN ('completed', 'failed', 'cancelled')",
            params![],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;

        let mut recovered = 0usize;

        for (step_id, child_run_id, ar_status, result_text) in stuck {
            let step_status = match ar_status.as_str() {
                "completed" => WorkflowStepStatus::Completed,
                _ => WorkflowStepStatus::Failed,
            };

            self.update_step_status_full(
                &step_id,
                step_status,
                Some(&child_run_id),
                result_text.as_deref(),
                None,
                None,
                None,
                None,
            )?;
            recovered += 1;
        }

        Ok(recovered)
    }

    /// Reap workflow runs that are stuck in `waiting` status because the executor
    /// process died while polling a gate.
    ///
    /// A root run (`parent_workflow_run_id IS NULL`) is considered orphaned when:
    /// - Its parent agent run is in a terminal state (`completed`, `failed`, or
    ///   `cancelled`), meaning the executor loop that owned this run is gone, OR
    /// - The active gate step's timeout has elapsed based on wall-clock time since
    ///   `started_at`.
    ///
    /// Orphaned runs have their active gate step marked `timed_out` and the run
    /// itself marked `cancelled` with a descriptive summary.
    pub fn reap_orphaned_workflow_runs(&self) -> Result<usize> {
        // Query all root runs in 'waiting' status.
        let waiting_runs: Vec<(String, String)> = query_collect(
            self.conn,
            "SELECT id, parent_run_id FROM workflow_runs \
             WHERE status = 'waiting' AND parent_workflow_run_id IS NULL",
            params![],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        if waiting_runs.is_empty() {
            return Ok(0);
        }

        // Batch-fetch all parent agent run statuses in a single IN-clause query
        // to avoid N+1 per-run lookups.
        let parent_ids: Vec<String> = waiting_runs
            .iter()
            .map(|(_, parent_run_id)| parent_run_id.clone())
            .collect();

        let parent_statuses: HashMap<String, String> = with_in_clause(
            "SELECT id, status FROM agent_runs WHERE id IN",
            &[],
            &parent_ids,
            |sql, params| -> Result<HashMap<String, String>> {
                let mut stmt = self.conn.prepare(sql)?;
                let mut rows = stmt.query(params)?;
                let mut map = HashMap::new();
                while let Some(row) = rows.next()? {
                    let id: String = row.get(0)?;
                    let status: String = row.get(1)?;
                    map.insert(id, status);
                }
                Ok(map)
            },
        )?;

        let mut reaped = 0usize;
        let now = Utc::now();

        for (run_id, parent_run_id) in waiting_runs {
            // A missing parent (None) is also treated as dead — if the agent run
            // has been purged from the DB its executor is certainly gone.
            let dead_parent = !matches!(
                parent_statuses.get(&parent_run_id).map(String::as_str),
                Some("running") | Some("waiting_for_feedback")
            );

            // Check if the active gate step's timeout has elapsed.
            let gate_step = self.find_waiting_gate(&run_id)?;
            let gate_timed_out = gate_step.as_ref().is_some_and(|step| {
                let timeout_secs = step.gate_timeout.as_deref().and_then(|s| {
                    match crate::workflow_dsl::parse_duration_str(s) {
                        Ok(n) => i64::try_from(n).ok(),
                        Err(_) => {
                            tracing::warn!(
                                run_id = %run_id,
                                gate_timeout = %s,
                                "gate_timeout value could not be parsed — timeout will not be enforced"
                            );
                            None
                        }
                    }
                });
                let started_at = step.started_at.as_deref().and_then(|s| {
                    match DateTime::parse_from_rfc3339(s) {
                        Ok(dt) => Some(dt.with_timezone(&Utc)),
                        Err(_) => {
                            tracing::warn!(
                                run_id = %run_id,
                                started_at = %s,
                                "gate step started_at could not be parsed — timeout will not be enforced"
                            );
                            None
                        }
                    }
                });
                match (timeout_secs, started_at) {
                    (Some(secs), Some(start)) => (now - start).num_seconds() >= secs,
                    _ => false,
                }
            });

            if !dead_parent && !gate_timed_out {
                continue;
            }

            // Mark the active gate step as timed_out.
            if let Some(ref step) = gate_step {
                let now_str = now.to_rfc3339();
                self.conn.execute(
                    "UPDATE workflow_run_steps SET status = 'timed_out', ended_at = ?1 \
                     WHERE id = ?2",
                    params![now_str, step.id],
                )?;
            }

            self.update_workflow_status(
                &run_id,
                WorkflowRunStatus::Cancelled,
                Some(
                    "Orphaned: executor died while waiting for gate \
                     — run was automatically cancelled",
                ),
            )?;
            tracing::info!(run_id = %run_id, "Reaped orphaned workflow run");
            reaped += 1;
        }

        Ok(reaped)
    }

    /// Detect workflow runs that are stuck in `running` status because the
    /// executor process died between steps.
    ///
    /// A run is "stuck" when ALL of the following hold:
    /// 1. `status = 'running'`
    /// 2. `parent_workflow_run_id IS NULL` (root runs only — sub-workflows are
    ///    driven by their parent engine loop)
    /// 3. No step has `status IN ('running', 'pending', 'waiting')` — all
    ///    current steps are terminal
    /// 4. The most recent step's `ended_at` is older than `threshold_secs`
    ///
    /// Returns the IDs of all stuck runs. Callers are responsible for resuming
    /// them (e.g. by spawning a thread per ID and calling
    /// `resume_workflow_standalone`).
    pub fn detect_stuck_workflow_run_ids(&self, threshold_secs: i64) -> Result<Vec<String>> {
        query_collect(
            self.conn,
            "SELECT id FROM ( \
               SELECT wr.id, \
                 (SELECT MAX(ended_at) \
                  FROM workflow_run_steps wrs2 \
                  WHERE wrs2.workflow_run_id = wr.id) AS last_step_ended \
               FROM workflow_runs wr \
               WHERE wr.status = 'running' \
                 AND wr.parent_workflow_run_id IS NULL \
                 AND NOT EXISTS ( \
                   SELECT 1 FROM workflow_run_steps wrs \
                   WHERE wrs.workflow_run_id = wr.id \
                     AND wrs.status IN ('running', 'pending', 'waiting') \
                 ) \
             ) \
             WHERE last_step_ended IS NOT NULL \
               AND (CAST(strftime('%s', 'now') AS INTEGER) \
                    - CAST(strftime('%s', last_step_ended) AS INTEGER)) > ?1",
            params![threshold_secs],
            |row| row.get(0),
        )
    }

    /// Detect workflow runs with an active step that has been running longer
    /// than `threshold_minutes` without completing.
    ///
    /// Unlike [`detect_stuck_workflow_run_ids`] (all steps terminal, executor
    /// crashed between steps), this catches the case where a step's child
    /// process is alive but hung — no crash, just no progress.
    ///
    /// Returns metadata for each stale run including the child agent run's
    /// tmux window name, so callers can verify whether the process is still
    /// alive before taking action.
    pub fn detect_stale_workflow_runs(
        &self,
        threshold_minutes: i64,
    ) -> Result<Vec<StaleWorkflowRun>> {
        if threshold_minutes <= 0 {
            return Ok(vec![]);
        }
        query_collect(
            self.conn,
            "SELECT wr.id, wr.workflow_name, wr.target_label, \
                    wrs.step_name, \
                    (CAST(strftime('%s', 'now') AS INTEGER) \
                     - CAST(strftime('%s', wrs.started_at) AS INTEGER)) / 60, \
                    wrs.id, wrs.child_run_id, ar.tmux_window \
             FROM workflow_runs wr \
             JOIN workflow_run_steps wrs ON wrs.workflow_run_id = wr.id \
             LEFT JOIN agent_runs ar ON ar.id = wrs.child_run_id \
             WHERE wr.status = 'running' \
               AND wr.parent_workflow_run_id IS NULL \
               AND wrs.status = 'running' \
               AND wrs.started_at IS NOT NULL \
               AND (CAST(strftime('%s', 'now') AS INTEGER) \
                    - CAST(strftime('%s', wrs.started_at) AS INTEGER)) > ?1 * 60",
            params![threshold_minutes],
            |row| {
                Ok(StaleWorkflowRun {
                    run_id: row.get(0)?,
                    workflow_name: row.get(1)?,
                    target_label: row.get(2)?,
                    step_name: row.get(3)?,
                    running_minutes: row.get(4)?,
                    step_id: row.get(5)?,
                    child_run_id: row.get(6)?,
                    tmux_window: row.get(7)?,
                })
            },
        )
    }

    /// Reap stale workflow runs whose agent process is confirmed dead.
    ///
    /// For each stale run returned by [`detect_stale_workflow_runs`]:
    /// 1. Check if the child agent's tmux window still exists.
    /// 2. If the window is gone, mark the child agent run as failed, mark the
    ///    workflow step as failed, and mark the workflow run as failed.
    /// 3. If the window is still alive, the agent is running (just slow) — skip.
    ///
    /// Returns the list of reaped runs so callers can fire notifications and
    /// optionally auto-restart them.
    pub fn reap_stale_workflow_runs(
        &self,
        threshold_minutes: i64,
        live_tmux_windows: &std::collections::HashSet<String>,
    ) -> Result<Vec<ReapedStaleRun>> {
        let stale = self.detect_stale_workflow_runs(threshold_minutes)?;
        self.reap_detected_stale_runs(stale, live_tmux_windows)
    }

    /// Reap a pre-detected list of stale workflow runs whose agent process is
    /// confirmed dead.
    ///
    /// Like [`reap_stale_workflow_runs`] but accepts an already-fetched list of
    /// stale runs, avoiding a redundant [`detect_stale_workflow_runs`] call when
    /// the caller has already queried them (e.g. to send informational alerts).
    pub fn reap_detected_stale_runs(
        &self,
        stale: Vec<StaleWorkflowRun>,
        live_tmux_windows: &std::collections::HashSet<String>,
    ) -> Result<Vec<ReapedStaleRun>> {
        if stale.is_empty() {
            return Ok(vec![]);
        }

        let agent_mgr = crate::agent::AgentManager::new(self.conn);
        let now_str = Utc::now().to_rfc3339();
        let mut reaped = Vec::new();

        for s in stale {
            // If the tmux window is still alive, the agent is running — just slow.
            if let Some(ref window) = s.tmux_window {
                if live_tmux_windows.contains(window.as_str()) {
                    continue;
                }
            }

            // Agent process is dead. Mark child agent run as failed.
            if let Some(ref child_run_id) = s.child_run_id {
                if let Err(e) = agent_mgr.update_run_failed(
                    child_run_id,
                    "Stale workflow watchdog: agent process died (tmux session lost)",
                ) {
                    tracing::warn!(
                        child_run_id = %child_run_id,
                        "Failed to mark child agent run as failed: {e}"
                    );
                }
            }

            // Mark the workflow step as failed.
            self.conn.execute(
                "UPDATE workflow_run_steps SET status = 'failed', ended_at = ?1, \
                 result_text = 'Agent process died — marked by stale workflow watchdog' \
                 WHERE id = ?2",
                params![now_str, s.step_id],
            )?;

            // Mark the workflow run as failed.
            self.update_workflow_status(
                &s.run_id,
                WorkflowRunStatus::Failed,
                Some("Stale workflow watchdog: agent process died, run marked as failed"),
            )?;

            tracing::info!(
                run_id = %s.run_id,
                step_name = %s.step_name,
                running_minutes = s.running_minutes,
                "Reaped stale workflow run — agent process was dead"
            );

            reaped.push(ReapedStaleRun {
                run_id: s.run_id,
                workflow_name: s.workflow_name,
                target_label: s.target_label,
                step_name: s.step_name,
                running_minutes: s.running_minutes,
            });
        }

        Ok(reaped)
    }

    /// Find the most-recently-started child workflow run that can be resumed:
    /// failed, pending, waiting, or timed_out status for the given parent + child
    /// workflow name. Returns `None` if no such run exists.
    ///
    /// `running` is excluded to avoid interfering with a genuinely-active child.
    /// `completed` and `cancelled` are excluded as they are terminal or irrecoverable.
    pub fn find_resumable_child_run(
        &self,
        parent_workflow_run_id: &str,
        child_workflow_name: &str,
    ) -> Result<Option<WorkflowRun>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {RUN_COLUMNS} FROM workflow_runs \
                     WHERE parent_workflow_run_id = ?1 \
                       AND workflow_name = ?2 \
                       AND status IN ('failed', 'pending', 'waiting', 'timed_out') \
                     ORDER BY started_at DESC \
                     LIMIT 1"
                ),
                params![parent_workflow_run_id, child_workflow_name],
                row_to_workflow_run,
            )
            .optional()?)
    }

    const SQL_RESET_FAILED: &'static str = "UPDATE workflow_run_steps \
         SET status = 'pending', started_at = NULL, ended_at = NULL, result_text = NULL, \
         context_out = NULL, markers_out = NULL, structured_output = NULL, child_run_id = NULL \
         WHERE workflow_run_id = ?1 AND status IN ('failed', 'running', 'timed_out')";

    const SQL_RESET_COMPLETED: &'static str = "UPDATE workflow_run_steps \
         SET status = 'pending', started_at = NULL, ended_at = NULL, result_text = NULL, \
         context_out = NULL, markers_out = NULL, structured_output = NULL, child_run_id = NULL \
         WHERE workflow_run_id = ?1 AND status = 'completed'";

    const SQL_RESET_FROM_POS: &'static str = "UPDATE workflow_run_steps \
         SET status = 'pending', started_at = NULL, ended_at = NULL, result_text = NULL, \
         context_out = NULL, markers_out = NULL, structured_output = NULL, child_run_id = NULL \
         WHERE workflow_run_id = ?1 AND position >= ?2";

    /// Reset all non-completed steps for a workflow run back to `pending`.
    ///
    /// Used before resuming so that failed/running/timed_out steps get re-executed.
    pub fn reset_failed_steps(&self, workflow_run_id: &str) -> Result<u64> {
        let count = self
            .conn
            .execute(Self::SQL_RESET_FAILED, params![workflow_run_id])?;
        Ok(count as u64)
    }

    /// Reset all completed steps for a workflow run back to `pending`.
    ///
    /// Used for full restart (--restart) to re-run from scratch.
    pub fn reset_completed_steps(&self, workflow_run_id: &str) -> Result<u64> {
        let count = self
            .conn
            .execute(Self::SQL_RESET_COMPLETED, params![workflow_run_id])?;
        Ok(count as u64)
    }

    /// Reset all steps at or after a given position back to `pending`.
    ///
    /// Used for --from-step to re-run from a specific step onwards.
    pub fn reset_steps_from_position(&self, workflow_run_id: &str, position: i64) -> Result<u64> {
        let count = self
            .conn
            .execute(Self::SQL_RESET_FROM_POS, params![workflow_run_id, position])?;
        Ok(count as u64)
    }

    /// Return the set of completed step keys as `(step_name, iteration)` pairs.
    ///
    /// Used to build the skip set for resume.
    pub fn get_completed_step_keys(&self, workflow_run_id: &str) -> Result<HashSet<StepKey>> {
        let steps = self.get_workflow_steps(workflow_run_id)?;
        Ok(crate::workflow::engine::completed_keys_from_steps(&steps))
    }

    /// Build the purge where-clause and bind params, then pass them to a caller-provided
    /// closure.  Deduplicates the empty-check, where-clause build, and `params_ref`
    /// construction shared by `purge` and `purge_count`.
    fn with_purge_params<T>(
        &self,
        repo_id: Option<&str>,
        statuses: &[&str],
        f: impl FnOnce(&str, &[&dyn rusqlite::ToSql]) -> Result<T>,
    ) -> Result<T> {
        let (where_clause, params) = purge_where_clause(statuses, repo_id);
        let params_ref: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        f(&where_clause, params_ref.as_slice())
    }

    /// Delete workflow runs with the given statuses, optionally scoped to a repo.
    ///
    /// `statuses` should be a non-empty slice of terminal status strings
    /// (`"completed"`, `"failed"`, `"cancelled"`). `workflow_run_steps` rows are
    /// removed automatically via `ON DELETE CASCADE`.
    ///
    /// Returns the number of deleted rows.
    pub fn purge(&self, repo_id: Option<&str>, statuses: &[&str]) -> Result<usize> {
        if statuses.is_empty() {
            return Ok(0);
        }
        self.with_purge_params(repo_id, statuses, |where_clause, params_ref| {
            let sql = format!("DELETE FROM workflow_runs WHERE {where_clause}");
            Ok(self.conn.execute(&sql, params_ref)?)
        })
    }

    /// Count workflow runs that *would* be deleted by [`purge`] with the same arguments.
    ///
    /// Used by `--dry-run` to preview the deletion without modifying the database.
    pub fn purge_count(&self, repo_id: Option<&str>, statuses: &[&str]) -> Result<usize> {
        if statuses.is_empty() {
            return Ok(0);
        }
        self.with_purge_params(repo_id, statuses, |where_clause, params_ref| {
            let sql = format!("SELECT COUNT(*) FROM workflow_runs WHERE {where_clause}");
            let count: i64 = self.conn.query_row(&sql, params_ref, |row| row.get(0))?;
            Ok(count as usize)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use rusqlite::params;

    use crate::agent::AgentManager;
    use crate::workflow::WorkflowManager;

    fn setup_db() -> rusqlite::Connection {
        crate::test_helpers::setup_db()
    }

    fn make_stale_running_step(
        conn: &rusqlite::Connection,
        run_id: &str,
        step_id: &str,
        started_at: &str,
    ) {
        // Insert a workflow step in `running` state, started in the past, with no child agent run.
        conn.execute(
            "INSERT INTO workflow_run_steps \
             (id, workflow_run_id, step_name, role, can_commit, status, position, iteration, started_at) \
             VALUES (?1, ?2, 'my-step', 'actor', 0, 'running', 0, 0, ?3)",
            params![step_id, run_id, started_at],
        )
        .unwrap();
    }

    /// A step that entered `running` state but never spawned a child agent run
    /// (child_run_id IS NULL, tmux_window IS NULL) must still be reaped:
    /// the step and workflow run must both be marked `failed`.
    #[test]
    fn reap_stale_run_with_no_child_run_id_marks_step_and_run_failed() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let agent_mgr = AgentManager::new(&conn);

        // Create a parent agent run (needed by create_workflow_run).
        let parent_run_id = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap()
            .id;

        // Create the workflow run.
        let run = mgr
            .create_workflow_run("test-wf", Some("w1"), &parent_run_id, false, "manual", None)
            .unwrap();

        // The run starts as `pending`; advance it to `running` so detect_stale can find it.
        conn.execute(
            "UPDATE workflow_runs SET status = 'running' WHERE id = ?1",
            params![run.id],
        )
        .unwrap();

        // Insert a step started 2 hours ago with no child_run_id.
        let step_id = crate::new_id();
        let two_hours_ago = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        make_stale_running_step(&conn, &run.id, &step_id, &two_hours_ago);

        // Reap with threshold of 30 minutes and no live tmux windows.
        let live_windows: HashSet<String> = HashSet::new();
        let reaped = mgr
            .reap_stale_workflow_runs(30, &live_windows)
            .expect("reap_stale_workflow_runs must not fail");

        assert_eq!(reaped.len(), 1, "one run must be reaped");
        assert_eq!(reaped[0].run_id, run.id);

        // The step must be marked failed.
        let step_status: String = conn
            .query_row(
                "SELECT status FROM workflow_run_steps WHERE id = ?1",
                params![step_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            step_status, "failed",
            "step must be marked failed (child_run_id=None path)"
        );

        // The workflow run must also be marked failed.
        let run_status: String = conn
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = ?1",
                params![run.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(run_status, "failed", "workflow run must be marked failed");
    }

    /// A step with a live tmux window must NOT be reaped — the agent is slow, not dead.
    #[test]
    fn reap_stale_run_skips_alive_tmux_window() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let agent_mgr = AgentManager::new(&conn);

        let parent_run_id = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap()
            .id;
        let run = mgr
            .create_workflow_run("test-wf", Some("w1"), &parent_run_id, false, "manual", None)
            .unwrap();

        // Advance run to `running` so detect_stale can find it.
        conn.execute(
            "UPDATE workflow_runs SET status = 'running' WHERE id = ?1",
            params![run.id],
        )
        .unwrap();

        // Insert a step with a tmux_window value.
        let step_id = crate::new_id();
        let two_hours_ago = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        conn.execute(
            "INSERT INTO workflow_run_steps \
             (id, workflow_run_id, step_name, role, can_commit, status, position, iteration, started_at) \
             VALUES (?1, ?2, 'slow-step', 'actor', 0, 'running', 0, 0, ?3)",
            params![step_id, run.id, two_hours_ago],
        )
        .unwrap();
        // Create an agent run that has a tmux_window set.
        let child_run = agent_mgr
            .create_run(Some("w1"), "agent", None, None)
            .unwrap();
        conn.execute(
            "UPDATE agent_runs SET tmux_window = 'alive-window' WHERE id = ?1",
            params![child_run.id],
        )
        .unwrap();
        conn.execute(
            "UPDATE workflow_run_steps SET child_run_id = ?1 WHERE id = ?2",
            params![child_run.id, step_id],
        )
        .unwrap();

        // The window is alive.
        let mut live_windows: HashSet<String> = HashSet::new();
        live_windows.insert("alive-window".to_string());

        let reaped = mgr
            .reap_stale_workflow_runs(30, &live_windows)
            .expect("reap must not fail");

        assert!(reaped.is_empty(), "alive-window run must not be reaped");
    }
}
