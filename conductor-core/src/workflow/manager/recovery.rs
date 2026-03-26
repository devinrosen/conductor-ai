use std::collections::HashSet;

use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};

use crate::db::query_collect;
use crate::error::Result;

use super::helpers::{purge_where_clause, row_to_workflow_run};
use super::WorkflowManager;
use crate::workflow::constants::RUN_COLUMNS;
use crate::workflow::status::{WorkflowRunStatus, WorkflowStepStatus};
use crate::workflow::types::{StepKey, WorkflowRun};

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

        let mut reaped = 0usize;
        let now = Utc::now();

        for (run_id, parent_run_id) in waiting_runs {
            // Check if the parent agent run is in a terminal state.
            let parent_status: Option<String> = self
                .conn
                .query_row(
                    "SELECT status FROM agent_runs WHERE id = ?1",
                    params![parent_run_id],
                    |row| row.get(0),
                )
                .optional()?;

            // A missing parent (None) is also treated as dead — if the agent run
            // has been purged from the DB its executor is certainly gone.
            let dead_parent = !matches!(
                parent_status.as_deref(),
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
        let result = self.conn.query_row(
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
        );
        match result {
            Ok(run) => Ok(Some(run)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
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
        let (where_clause, params) = purge_where_clause(statuses, repo_id);
        let sql = format!("DELETE FROM workflow_runs WHERE {where_clause}");
        let params_ref: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        Ok(self.conn.execute(&sql, params_ref.as_slice())?)
    }

    /// Count workflow runs that *would* be deleted by [`purge`] with the same arguments.
    ///
    /// Used by `--dry-run` to preview the deletion without modifying the database.
    pub fn purge_count(&self, repo_id: Option<&str>, statuses: &[&str]) -> Result<usize> {
        if statuses.is_empty() {
            return Ok(0);
        }
        let (where_clause, params) = purge_where_clause(statuses, repo_id);
        let sql = format!("SELECT COUNT(*) FROM workflow_runs WHERE {where_clause}");
        let params_ref: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let count: i64 = self
            .conn
            .query_row(&sql, params_ref.as_slice(), |row| row.get(0))?;
        Ok(count as usize)
    }
}
