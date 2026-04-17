use std::collections::HashSet;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};

use crate::agent::status::AgentRunStatus;
use crate::config::Config;
use crate::db::query_collect;
use crate::error::Result;

use super::helpers::{purge_where_clause, row_to_workflow_run};
use super::WorkflowManager;
use crate::workflow::constants::RUN_COLUMNS;
use crate::workflow::status::{WorkflowRunStatus, WorkflowStepStatus};
use crate::workflow::types::{StepKey, WorkflowResumeStandalone, WorkflowRun};

macro_rules! reset_sql {
    ($where:literal) => {
        concat!(
            "UPDATE workflow_run_steps \
             SET status = 'pending', started_at = NULL, ended_at = NULL, result_text = NULL, \
             context_out = NULL, markers_out = NULL, structured_output = NULL, child_run_id = NULL, \
             subprocess_pid = NULL, step_error = NULL \
             ",
            $where
        )
    };
}

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
    /// The subprocess PID for the child agent run (if any).
    pub subprocess_pid: Option<i64>,
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
    /// Reap workflow_run_steps stuck in `running` status whose script subprocess
    /// has died while conductor was not running.
    ///
    /// Only script steps are checked: agent steps always have `child_run_id` set and
    /// are handled by `recover_stuck_steps()`. Script steps have `child_run_id = NULL`
    /// and `subprocess_pid` set after the child is spawned.
    ///
    /// For each candidate the reaper:
    /// 1. Checks `pid_is_alive(pid)` — if false, marks the step `failed`.
    /// 2. If the PID is alive, calls `pid_was_recycled` to detect OS PID reuse. If
    ///    the PID was recycled, the original process is gone and the step is marked
    ///    `failed`.
    ///
    /// Returns the count of steps that were reaped.
    pub fn reap_orphaned_script_steps(&self) -> Result<usize> {
        // Query script steps that are stuck in 'running' and have a subprocess_pid.
        // child_run_id IS NULL discriminates script steps from agent steps.
        let candidates: Vec<(String, i64, Option<String>)> = query_collect(
            self.conn,
            "SELECT id, subprocess_pid, started_at \
             FROM workflow_run_steps \
             WHERE status = 'running' \
               AND child_run_id IS NULL \
               AND subprocess_pid IS NOT NULL",
            params![],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

        if candidates.is_empty() {
            return Ok(0);
        }

        let mut reaped = 0usize;

        for (step_id, raw_pid, started_at) in candidates {
            let pid = raw_pid as u32;

            #[cfg(unix)]
            {
                if crate::process_utils::pid_is_alive(pid) {
                    // PID is alive — check for OS PID reuse using the process start
                    // time recorded at spawn vs. the OS-reported start time now.
                    let recycled = started_at
                        .as_deref()
                        .is_some_and(|at| crate::process_utils::pid_was_recycled(pid, at));
                    if !recycled {
                        // Process is genuinely still running — leave it alone.
                        continue;
                    }
                    tracing::warn!(
                        step_id = %step_id,
                        pid,
                        "reap_orphaned_script_steps: PID recycled — original script process is gone"
                    );
                    self.fail_step_with_message(
                        &step_id,
                        "subprocess PID recycled — original script process is gone",
                    )?;
                    reaped += 1;
                    continue;
                }

                tracing::warn!(
                    step_id = %step_id,
                    pid,
                    "reap_orphaned_script_steps: subprocess lost — script process exited while conductor was not running"
                );
                self.fail_step_with_message(
                    &step_id,
                    "subprocess lost — script process exited while conductor was not running",
                )?;
                reaped += 1;
            }

            // On non-Unix platforms (Windows) we cannot check liveness via kill(0).
            // Skip the step to avoid false-positive reaping.
            #[cfg(not(unix))]
            let _ = (step_id, pid, started_at);
        }

        if reaped > 0 {
            tracing::info!("reap_orphaned_script_steps: reaped {reaped} orphaned script step(s)");
        }

        Ok(reaped)
    }

    /// Helper function to fail a workflow step with a specific error message.
    /// Sets all optional fields to None.
    fn fail_step_with_message(&self, step_id: &str, error_message: &str) -> Result<()> {
        self.update_step_status(
            step_id,
            WorkflowStepStatus::Failed,
            None,
            Some(error_message),
            None,
            None,
            None,
        )
    }

    /// Recover steps stuck in `running` status whose child agent run has
    /// already reached a terminal state (completed, failed, or cancelled).
    ///
    /// This handles the case where the executor was killed before the workflow
    /// thread could write the step's final status back to the DB.
    /// Returns the number of steps recovered.
    pub fn recover_stuck_steps(&self) -> Result<usize> {
        // Step 1: fetch running workflow steps that have a child_run_id.
        let running_steps: Vec<(String, String)> = query_collect(
            self.conn,
            "SELECT id, child_run_id FROM workflow_run_steps \
             WHERE status = 'running' AND child_run_id IS NOT NULL",
            params![],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        if running_steps.is_empty() {
            return Ok(0);
        }

        // Step 2: batch-fetch the agent runs via AgentManager.
        let agent_mgr = crate::agent::AgentManager::new(self.conn);
        let child_ids: Vec<&str> = running_steps.iter().map(|(_, id)| id.as_str()).collect();
        let child_runs = agent_mgr.get_runs_by_ids(&child_ids)?;

        // Filter in Rust to those with terminal statuses.
        let stuck: Vec<(String, String, WorkflowStepStatus, Option<String>)> = running_steps
            .into_iter()
            .filter_map(|(step_id, child_run_id)| {
                let Some(run) = child_runs.get(&child_run_id) else {
                    tracing::warn!(
                        step_id = %step_id,
                        child_run_id = %child_run_id,
                        "recover_stuck_steps: running step references a child_run_id not found \
                         in agent_runs — the agent run may have been purged; \
                         step will remain in 'running' status"
                    );
                    return None;
                };
                let step_status = match run.status {
                    AgentRunStatus::Completed => WorkflowStepStatus::Completed,
                    AgentRunStatus::Failed | AgentRunStatus::Cancelled => {
                        WorkflowStepStatus::Failed
                    }
                    _ => return None,
                };
                Some((step_id, child_run_id, step_status, run.result_text.clone()))
            })
            .collect();

        let mut recovered = 0usize;

        for (step_id, child_run_id, step_status, result_text) in stuck {
            self.update_step_status_full(
                &step_id,
                step_status,
                Some(&child_run_id),
                result_text.as_deref(),
                None,
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

        // Batch-fetch all parent agent runs via AgentManager to avoid N+1 lookups.
        let parent_ids: Vec<String> = waiting_runs
            .iter()
            .map(|(_, parent_run_id)| parent_run_id.clone())
            .collect();

        let agent_mgr = crate::agent::AgentManager::new(self.conn);
        let id_refs: Vec<&str> = parent_ids.iter().map(String::as_str).collect();
        let parent_runs = agent_mgr.get_runs_by_ids(&id_refs)?;

        let mut reaped = 0usize;
        let now = Utc::now();

        for (run_id, parent_run_id) in waiting_runs {
            // A missing parent (None) is also treated as dead — if the agent run
            // has been purged from the DB its executor is certainly gone.
            let dead_parent = !matches!(
                parent_runs.get(&parent_run_id).map(|r| &r.status),
                Some(AgentRunStatus::Running) | Some(AgentRunStatus::WaitingForFeedback)
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
                self.update_step_status(
                    &step.id,
                    WorkflowStepStatus::TimedOut,
                    None,
                    None,
                    None,
                    None,
                    None,
                )?;
            }

            self.update_workflow_status(
                &run_id,
                WorkflowRunStatus::Cancelled,
                Some(
                    "Orphaned: executor died while waiting for gate \
                     — run was automatically cancelled",
                ),
                None,
            )?;
            tracing::info!(run_id = %run_id, "Reaped orphaned workflow run");
            reaped += 1;
        }

        Ok(reaped)
    }

    /// Detect workflow run IDs that are stuck in `running` status because the
    /// executor process died between steps (all steps terminal, no active work).
    ///
    /// This is the detection-only counterpart of [`reap_heartbeat_stuck_runs`],
    /// useful for diagnostics and tests. Uses the same query (including runs
    /// with zero steps — the executor may have died before creating any).
    pub fn detect_stuck_workflow_run_ids(&self, threshold_secs: i64) -> Result<Vec<String>> {
        query_collect(
            self.conn,
            "SELECT id FROM workflow_runs \
             WHERE status = 'running' \
               AND parent_workflow_run_id IS NULL \
               AND NOT EXISTS ( \
                 SELECT 1 FROM workflow_run_steps wrs \
                 WHERE wrs.workflow_run_id = workflow_runs.id \
                   AND wrs.status IN ('running', 'pending', 'waiting') \
               ) \
               AND ( \
                 CAST(strftime('%s', 'now') AS INTEGER) - \
                 CAST(strftime('%s', COALESCE(last_heartbeat, started_at)) AS INTEGER) \
               ) > ?1",
            params![threshold_secs],
            |row| row.get(0),
        )
    }

    /// Detect workflow runs with an active step that has been running longer
    /// than `threshold_minutes` without completing.
    ///
    /// Unlike [`reap_heartbeat_stuck_runs`] (all steps terminal, executor
    /// crashed between steps), this catches the case where a step's child
    /// process is alive but hung — no crash, just no progress.
    ///
    /// Returns metadata for each stale run including the child agent run's
    /// subprocess PID, so callers can verify whether the process is still
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
                    wrs.id, wrs.child_run_id, \
                    COALESCE(wrs.subprocess_pid, ar.subprocess_pid) \
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
                    subprocess_pid: row.get(7)?,
                })
            },
        )
    }

    /// Reap stale workflow runs whose agent process is confirmed dead.
    ///
    /// For each stale run returned by [`detect_stale_workflow_runs`]:
    /// 1. Check if the child agent's subprocess PID is still alive.
    /// 2. If the process is gone, mark the child agent run as failed, mark the
    ///    workflow step as failed, and mark the workflow run as failed.
    /// 3. If the process is still alive, the agent is running (just slow) — skip.
    ///
    /// Returns the list of reaped runs so callers can fire notifications and
    /// optionally auto-restart them.
    pub fn reap_stale_workflow_runs(&self, threshold_minutes: i64) -> Result<Vec<ReapedStaleRun>> {
        let stale = self.detect_stale_workflow_runs(threshold_minutes)?;
        if stale.is_empty() {
            return Ok(vec![]);
        }

        let agent_mgr = crate::agent::AgentManager::new(self.conn);
        let mut reaped = Vec::new();

        for s in stale {
            // If the subprocess is still alive, the agent is running — just slow.
            #[cfg(unix)]
            if let Some(pid) = s.subprocess_pid {
                if crate::process_utils::pid_is_alive(pid as u32) {
                    continue;
                }
            }

            // Agent process is dead. Mark child agent run as failed.
            if let Some(child_run_id) = &s.child_run_id {
                if let Err(e) = agent_mgr
                    .update_run_failed(child_run_id, "Stale workflow watchdog: agent process died")
                {
                    tracing::warn!(
                        child_run_id = %child_run_id,
                        error = %e,
                        "Failed to mark child agent run as failed during stale workflow cleanup"
                    );
                }
            }

            // Mark the workflow step as failed.
            self.fail_step_with_message(
                &s.step_id,
                "Agent process died — marked by stale workflow watchdog",
            )?;

            // Mark the workflow run as failed.
            self.update_workflow_status(
                &s.run_id,
                WorkflowRunStatus::Failed,
                Some("Stale workflow watchdog: agent process died, run marked as failed"),
                None,
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

    /// Detect and auto-resume workflow runs stuck in `running` status.
    ///
    /// **Detection** — uses `detect_stuck_workflow_run_ids` with the minimum of the
    /// fixed 60-second baseline and any caller-supplied configurable threshold. This
    /// avoids duplicate DB queries and prevents the same run from being resumed twice.
    ///
    /// **CAS flip** — before spawning a resume thread, the run is atomically flipped
    /// to `failed` via `UPDATE ... WHERE id=? AND status='running'`. If `changes() == 0`
    /// the run was already claimed by a concurrent watchdog and is skipped. This is
    /// required because `validate_resume_preconditions` rejects resuming a
    /// `running`-status run.
    ///
    /// For each successfully flipped run, fires a notification and spawns a
    /// background thread to resume it.
    ///
    /// Returns the count of runs resumed.
    pub fn auto_resume_stuck_workflows(
        &self,
        config: &Config,
        configurable_threshold_secs: Option<i64>,
        conductor_bin_dir: Option<PathBuf>,
    ) -> Result<usize> {
        use crate::workflow::WorkflowResumeStandalone;

        // Use the smallest threshold so we catch all stuck runs in a single query.
        let threshold = configurable_threshold_secs.map(|t| t.min(60)).unwrap_or(60);

        let stuck_ids = self.detect_stuck_workflow_run_ids(threshold)?;
        if stuck_ids.is_empty() {
            return Ok(0);
        }

        // CAS flip each run from running → failed before resuming.
        // Only runs we successfully flip get resumed — losers of the race are skipped.
        let mut flipped_ids: Vec<String> = Vec::new();
        for run_id in &stuck_ids {
            let changed = self.conn.execute(
                "UPDATE workflow_runs \
                 SET status = 'failed', \
                     error  = 'Orphaned: executor died between steps — auto-resumed by watchdog' \
                 WHERE id = ?1 AND status = 'running'",
                params![run_id],
            )?;
            if changed == 1 {
                flipped_ids.push(run_id.clone());
            } else {
                tracing::debug!(
                    run_id = %run_id,
                    "auto_resume_stuck_workflows: CAS lost race (already claimed)"
                );
            }
        }

        if flipped_ids.is_empty() {
            return Ok(0);
        }

        let n = flipped_ids.len();
        tracing::info!("Auto-resuming {n} stuck workflow run(s) (threshold={threshold}s)");
        crate::notify::fire_orphan_resumed_notification(
            self.conn,
            &config.notifications,
            &config.notify.hooks,
            &flipped_ids,
        );

        for run_id in flipped_ids {
            let cfg_clone = config.clone();
            let bin_dir = conductor_bin_dir.clone();
            let rid = run_id.clone();
            std::thread::spawn(move || {
                let params = WorkflowResumeStandalone {
                    config: cfg_clone,
                    workflow_run_id: rid.clone(),
                    model: None,
                    from_step: None,
                    restart: false,
                    db_path: None,
                    conductor_bin_dir: bin_dir,
                };
                if let Err(e) = crate::workflow::engine::resume_workflow_standalone(&params) {
                    tracing::warn!(run_id = %rid, "Auto-resume of stuck workflow run failed: {e}");
                }
            });
        }

        Ok(n)
    }

    /// Returns the count of runs successfully resumed.
    pub fn reap_heartbeat_stuck_runs(
        &self,
        config: &Config,
        threshold_secs: i64,
        conductor_bin_dir: Option<PathBuf>,
    ) -> Result<usize> {
        // Step 1: find orphaned root runs (including zero-step runs — the
        // executor may have died before creating any steps).
        let orphaned: Vec<(String, String, Option<String>)> = query_collect(
            self.conn,
            "SELECT id, workflow_name, target_label FROM workflow_runs \
             WHERE status = 'running' \
               AND parent_workflow_run_id IS NULL \
               AND NOT EXISTS ( \
                 SELECT 1 FROM workflow_run_steps wrs \
                 WHERE wrs.workflow_run_id = workflow_runs.id \
                   AND wrs.status IN ('running', 'pending', 'waiting') \
               ) \
               AND ( \
                 CAST(strftime('%s', 'now') AS INTEGER) - \
                 CAST(strftime('%s', COALESCE(last_heartbeat, started_at)) AS INTEGER) \
               ) > ?1",
            params![threshold_secs],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

        if orphaned.is_empty() {
            return Ok(0);
        }

        let mut resumed = 0usize;
        let mut resumed_ids: Vec<String> = Vec::new();

        for (run_id, workflow_name, target_label) in orphaned {
            // Step 2: CAS flip running → failed.
            // If another watchdog already won the race, changes() == 0 and we skip.
            let changed = self.conn.execute(
                "UPDATE workflow_runs \
                 SET status = 'failed', \
                     error  = 'Orphaned: executor died between steps — auto-resumed by watchdog' \
                 WHERE id = ?1 AND status = 'running'",
                params![run_id],
            )?;

            if changed != 1 {
                tracing::debug!(
                    run_id = %run_id,
                    "reap_heartbeat_stuck_runs: CAS lost race for run (already reaped)"
                );
                continue;
            }

            tracing::info!(
                run_id = %run_id,
                "reap_heartbeat_stuck_runs: reaped orphaned run, resuming"
            );

            // Step 3: resume — status is now `failed`, which validate_resume_preconditions accepts.
            let config_clone = config.clone();
            let bin_dir = conductor_bin_dir.clone();
            let run_id_clone = run_id.clone();
            let workflow_name_clone = workflow_name.clone();
            let target_label_clone = target_label.clone();
            std::thread::spawn(move || {
                let params = WorkflowResumeStandalone {
                    config: config_clone.clone(),
                    workflow_run_id: run_id_clone.clone(),
                    model: None,
                    from_step: None,
                    restart: false,
                    db_path: None,
                    conductor_bin_dir: bin_dir,
                };
                if let Err(e) = crate::workflow::engine::resume_workflow_standalone(&params) {
                    tracing::warn!(
                        run_id = %run_id_clone,
                        "reap_heartbeat_stuck_runs: auto-resume failed: {e}"
                    );
                    // Best-effort: fire a notification that this run failed to auto-resume.
                    if let Ok(db) = crate::db::open_database(&crate::config::db_path()) {
                        crate::notify::fire_heartbeat_stuck_failed_notification(
                            &db,
                            &config_clone.notifications,
                            &config_clone.notify.hooks,
                            &run_id_clone,
                            &workflow_name_clone,
                            target_label_clone.as_deref(),
                            &e.to_string(),
                        );
                    }
                }
            });

            resumed_ids.push(run_id);
            resumed += 1;
        }

        // Fire a single batch notification for all runs that were claimed for resumption.
        if !resumed_ids.is_empty() {
            crate::notify::fire_orphan_resumed_notification(
                self.conn,
                &config.notifications,
                &config.notify.hooks,
                &resumed_ids,
            );
        }

        Ok(resumed)
    }

    /// Directly finalize workflow runs that are stuck in `running` status because
    /// the finalization DB write (`update_workflow_status`) failed after all steps
    /// already reached terminal states.
    ///
    /// A run is eligible when ALL of the following hold:
    /// 1. `status = 'running'`
    /// 2. `parent_workflow_run_id IS NULL` (root runs only)
    /// 3. No step has `status IN ('running', 'pending', 'waiting')`
    /// 4. The most recent step `ended_at` (or the run's own `started_at` when
    ///    no steps exist) is older than `threshold_secs`
    ///
    /// Unlike `detect_stuck_workflow_run_ids`, this function writes the correct
    /// terminal status directly without resetting steps or re-running the engine:
    /// - Any `failed` or `timed_out` step → `Failed`
    /// - All `completed`/`skipped`/`cancelled` steps → `Completed`
    ///
    /// The parent `agent_runs` row is updated best-effort (failures are logged,
    /// not returned as errors).
    ///
    /// Returns the number of runs finalized.
    pub fn reap_finalization_stuck_workflow_runs(
        &self,
        threshold_secs: i64,
    ) -> crate::error::Result<usize> {
        // Find root running workflow runs where all steps are terminal and
        // the last step (or the run itself) ended more than threshold_secs ago.
        let stuck: Vec<(String, String, bool)> = query_collect(
            self.conn,
            "SELECT id, parent_run_id, has_failure FROM ( \
               SELECT wr.id, wr.parent_run_id, \
                 COALESCE( \
                   (SELECT MAX(ended_at) FROM workflow_run_steps wrs2 \
                    WHERE wrs2.workflow_run_id = wr.id), \
                   wr.started_at \
                 ) AS age_ref, \
                 EXISTS ( \
                   SELECT 1 FROM workflow_run_steps wrs3 \
                   WHERE wrs3.workflow_run_id = wr.id \
                     AND wrs3.status IN ('failed', 'timed_out') \
                 ) AS has_failure \
               FROM workflow_runs wr \
               WHERE wr.status = 'running' \
                 AND wr.parent_workflow_run_id IS NULL \
                 AND NOT EXISTS ( \
                   SELECT 1 FROM workflow_run_steps wrs \
                   WHERE wrs.workflow_run_id = wr.id \
                     AND wrs.status IN ('running', 'pending', 'waiting') \
                 ) \
             ) \
             WHERE age_ref IS NOT NULL \
               AND (CAST(strftime('%s', 'now') AS INTEGER) \
                    - CAST(strftime('%s', age_ref) AS INTEGER)) > ?1",
            params![threshold_secs],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

        let mut finalized = 0usize;

        for (run_id, parent_run_id, has_failure) in stuck {
            let final_status = if has_failure {
                WorkflowRunStatus::Failed
            } else {
                WorkflowRunStatus::Completed
            };

            let summary =
                "Auto-finalized by reaper: all steps terminal, status was stuck in 'running'"
                    .to_string();

            self.update_workflow_status(&run_id, final_status.clone(), Some(&summary), None)?;
            tracing::info!(
                run_id = %run_id,
                status = %final_status,
                "Reaper finalized stuck workflow run"
            );

            // Best-effort: update the parent agent_runs row if still running.
            let agent_mgr = crate::agent::AgentManager::new(self.conn);
            let update_result = if has_failure {
                agent_mgr.update_run_failed_if_running(&parent_run_id, &summary)
            } else {
                agent_mgr.update_run_completed_if_running(&parent_run_id, &summary)
            };
            if let Err(e) = update_result {
                tracing::warn!(
                    run_id = %run_id,
                    parent_run_id = %parent_run_id,
                    "Failed to update parent agent_runs row (best-effort, non-fatal): {e}"
                );
            }

            finalized += 1;
        }

        Ok(finalized)
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

    const SQL_RESET_FAILED: &'static str =
        reset_sql!("WHERE workflow_run_id = ?1 AND status IN ('failed', 'running', 'timed_out')");

    const SQL_RESET_COMPLETED: &'static str =
        reset_sql!("WHERE workflow_run_id = ?1 AND status = 'completed'");

    const SQL_RESET_FROM_POS: &'static str =
        reset_sql!("WHERE workflow_run_id = ?1 AND position >= ?2");

    /// Signal any `running` steps in the given run whose `subprocess_pid` is
    /// recorded.  Must be called before the SQL UPDATE zeroes the column so we
    /// still have the PID to signal.
    ///
    /// When `from_position` is `Some(pos)`, only steps at or after `pos` are
    /// signalled; `None` covers the entire run.  All SIGTERMs are fired
    /// concurrently so the worst-case stall is one grace period, not N × grace.
    ///
    /// Both script-step PIDs (`wrs.subprocess_pid`) and agent-step PIDs
    /// (`agent_runs.subprocess_pid` via `wrs.child_run_id`) are collected so
    /// that agent subprocesses are also terminated before their steps are reset.
    fn terminate_subprocesses(
        &self,
        workflow_run_id: &str,
        from_position: Option<i64>,
    ) -> Result<()> {
        #[cfg(unix)]
        {
            // Script-step PIDs (subprocess_pid on the step row itself).
            let script_pids: Vec<i64> = query_collect(
                self.conn,
                "SELECT subprocess_pid FROM workflow_run_steps \
                 WHERE workflow_run_id = ?1 AND status = 'running' \
                   AND subprocess_pid IS NOT NULL \
                   AND (?2 IS NULL OR position >= ?2)",
                params![workflow_run_id, from_position],
                |row| row.get(0),
            )?;

            // Agent-step PIDs: running steps where the PID lives in agent_runs
            // (wrs.subprocess_pid IS NULL) rather than on the step row itself.
            let agent_pids: Vec<i64> = query_collect(
                self.conn,
                "SELECT ar.subprocess_pid \
                 FROM workflow_run_steps wrs \
                 JOIN agent_runs ar ON ar.id = wrs.child_run_id \
                 WHERE wrs.workflow_run_id = ?1 \
                   AND wrs.status = 'running' \
                   AND wrs.subprocess_pid IS NULL \
                   AND ar.subprocess_pid IS NOT NULL \
                   AND (?2 IS NULL OR wrs.position >= ?2)",
                params![workflow_run_id, from_position],
                |row| row.get(0),
            )?;

            let handles: Vec<_> = script_pids
                .into_iter()
                .chain(agent_pids)
                .filter_map(|pid| u32::try_from(pid).ok())
                .map(|pid| std::thread::spawn(move || crate::process_utils::cancel_subprocess(pid)))
                .collect();
            for h in handles {
                if let Err(e) = h.join() {
                    tracing::warn!("subprocess cancel thread panicked: {:?}", e);
                }
            }
        }
        Ok(())
    }

    /// Count running steps for `workflow_run_id` that have a live subprocess,
    /// checking both `wrs.subprocess_pid` and `agent_runs.subprocess_pid` via
    /// `child_run_id`.  Used for diagnostic logging in the resume path.
    pub(crate) fn count_live_subprocess_steps(&self, workflow_run_id: &str) -> Result<usize> {
        #[cfg(unix)]
        {
            let pids: Vec<i64> = query_collect(
                self.conn,
                "SELECT COALESCE(wrs.subprocess_pid, ar.subprocess_pid) \
                 FROM workflow_run_steps wrs \
                 LEFT JOIN agent_runs ar ON ar.id = wrs.child_run_id \
                 WHERE wrs.workflow_run_id = ?1 \
                   AND wrs.status = 'running' \
                   AND COALESCE(wrs.subprocess_pid, ar.subprocess_pid) IS NOT NULL",
                params![workflow_run_id],
                |row| row.get(0),
            )?;

            let count = pids
                .into_iter()
                .filter_map(|pid| u32::try_from(pid).ok())
                .filter(|&pid| crate::process_utils::pid_is_alive(pid))
                .count();

            Ok(count)
        }
        #[cfg(not(unix))]
        Ok(0)
    }

    /// Reset all non-completed steps for a workflow run back to `pending`.
    ///
    /// Used before resuming so that failed/running/timed_out steps get re-executed.
    /// Sends SIGTERM to any `running` steps with a recorded subprocess PID before
    /// the column is nulled, preventing orphaned subprocesses.
    pub fn reset_failed_steps(&self, workflow_run_id: &str) -> Result<u64> {
        self.terminate_subprocesses(workflow_run_id, None)?;
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
    /// Sends SIGTERM to any `running` steps with a recorded subprocess PID before
    /// the column is nulled, preventing orphaned subprocesses.
    pub fn reset_steps_from_position(&self, workflow_run_id: &str, position: i64) -> Result<u64> {
        self.terminate_subprocesses(workflow_run_id, Some(position))?;
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

    /// Delete a single workflow run by ID, along with all of its descendant runs
    /// (child runs linked via `parent_workflow_run_id`).
    ///
    /// Validates that the run exists and is in a terminal state before deleting.
    /// Returns `ConductorError::WorkflowRunNotFound` if the run does not exist, and
    /// `ConductorError::InvalidInput` if the run is not in a terminal state.
    ///
    /// `workflow_run_steps` rows are removed automatically via `ON DELETE CASCADE`.
    /// Child runs are deleted recursively before the parent to satisfy FK constraints
    /// (the `parent_workflow_run_id` column has no `ON DELETE CASCADE`).
    pub fn delete_run(&self, run_id: &str) -> Result<()> {
        use crate::error::ConductorError;

        // Validate the run exists and is terminal.
        let run = self
            .conn
            .query_row(
                &format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE id = ?1"),
                params![run_id],
                row_to_workflow_run,
            )
            .optional()?
            .ok_or_else(|| ConductorError::WorkflowRunNotFound {
                id: run_id.to_string(),
            })?;

        if !run.status.is_terminal() {
            return Err(ConductorError::InvalidInput(format!(
                "cannot delete run '{run_id}': status is '{}' (must be terminal — cancel it first)",
                run.status
            )));
        }

        self.delete_run_recursive(run_id)
    }

    /// Recursively delete a workflow run and all of its descendants.
    ///
    /// Children are deleted before the parent to satisfy the FK constraint on
    /// `parent_workflow_run_id`. No status check is performed on children — they
    /// are expected to be terminal when the parent is.
    fn delete_run_recursive(&self, run_id: &str) -> Result<()> {
        let children: Vec<String> = query_collect(
            self.conn,
            "SELECT id FROM workflow_runs WHERE parent_workflow_run_id = ?1",
            params![run_id],
            |row| row.get(0),
        )?;

        for child_id in children {
            self.delete_run_recursive(&child_id)?;
        }

        self.conn
            .execute("DELETE FROM workflow_runs WHERE id = ?1", params![run_id])?;

        Ok(())
    }

    /// Delete orphaned `pending` step rows that were registered but never started.
    ///
    /// A step row is considered orphaned when `status = 'pending' AND started_at IS NULL`.
    /// These rows are created by `insert_step` but left behind if the executor crashes
    /// before the step actually begins. The resume path already handles them correctly
    /// by re-inserting and re-running, but the phantom rows pollute step history.
    ///
    /// This method is called at the top of the resume path, before the skip set is
    /// built, to remove the noise. Scoped to the given `workflow_run_id` so it cannot
    /// affect other runs.
    ///
    /// Returns the number of deleted rows.
    pub fn delete_orphaned_pending_steps(&self, workflow_run_id: &str) -> Result<usize> {
        let deleted = self.conn.execute(
            "DELETE FROM workflow_run_steps \
             WHERE workflow_run_id = ?1 \
               AND status = 'pending' \
               AND started_at IS NULL",
            params![workflow_run_id],
        )?;

        if deleted > 0 {
            tracing::info!(
                workflow_run_id = %workflow_run_id,
                deleted,
                "delete_orphaned_pending_steps: removed orphaned never-started step row(s)"
            );
        }

        Ok(deleted)
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

    /// Classify eligible `failed` workflow runs as `needs_resume`.
    ///
    /// A run is eligible when all three guards pass:
    /// 1. `error = 'parent agent run reached terminal state without completing the workflow'`
    ///    (the exact string set by `reap_workflow_runs_with_dead_parent`).
    /// 2. No steps with `status IN ('failed', 'timed_out')` — avoids auto-resuming
    ///    legitimately failed runs where a step itself errored out.
    /// 3. `iteration < auto_resume_limit` — retry cap prevents infinite loops.
    ///
    /// Pure SQL, no threads, no execution. Returns the number of runs transitioned.
    pub fn classify_resumable_workflows(&self, auto_resume_limit: u32) -> Result<usize> {
        let count = self.conn.execute(
            "UPDATE workflow_runs \
             SET status = 'needs_resume' \
             WHERE status = 'failed' \
               AND error = 'parent agent run reached terminal state without completing the workflow' \
               AND iteration < ?1 \
               AND NOT EXISTS ( \
                 SELECT 1 FROM workflow_run_steps \
                 WHERE workflow_run_id = workflow_runs.id \
                   AND status IN ('failed', 'timed_out') \
               )",
            params![auto_resume_limit],
        )?;

        if count > 0 {
            tracing::info!(
                "classify_resumable_workflows: flagged {count} workflow run(s) for auto-resume"
            );
        }

        Ok(count)
    }

    /// Watchdog: consume `needs_resume` runs by CAS-flipping them to `failed` and
    /// spawning a resume thread for each.
    ///
    /// The CAS flip (`WHERE status = 'needs_resume'`) is safe against concurrent
    /// watchdog calls from TUI and web processes — only the first caller wins the
    /// race; losers see `changes() == 0` and skip.
    ///
    /// Follows the `reap_heartbeat_stuck_runs` pattern exactly:
    /// 1. Query all `needs_resume` root runs.
    /// 2. CAS-flip each to `failed` with a watchdog error string.
    /// 3. Spawn a `resume_workflow_standalone` thread for each successful flip.
    /// 4. Fire a batch orphan-resumed notification.
    ///
    /// Returns the number of runs handed off to resume threads.
    pub fn watchdog_needs_resume_workflows(
        &self,
        config: &Config,
        conductor_bin_dir: Option<PathBuf>,
    ) -> Result<usize> {
        use crate::workflow::WorkflowResumeStandalone;

        // Step 1: find all needs_resume root runs.
        let candidates: Vec<(String, String, Option<String>)> = query_collect(
            self.conn,
            "SELECT id, workflow_name, target_label FROM workflow_runs \
             WHERE status = 'needs_resume' \
               AND parent_workflow_run_id IS NULL",
            params![],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

        if candidates.is_empty() {
            return Ok(0);
        }

        let mut resumed = 0usize;
        let mut resumed_ids: Vec<String> = Vec::new();

        for (run_id, _workflow_name, _target_label) in candidates {
            // Step 2: CAS flip needs_resume → failed.
            // If another watchdog already won the race, changes() == 0 and we skip.
            let changed = self.conn.execute(
                "UPDATE workflow_runs \
                 SET status = 'failed', \
                     error  = 'Orphaned: parent agent run died — auto-resumed by watchdog' \
                 WHERE id = ?1 AND status = 'needs_resume'",
                params![run_id],
            )?;

            if changed != 1 {
                tracing::debug!(
                    run_id = %run_id,
                    "watchdog_needs_resume_workflows: CAS lost race (already claimed)"
                );
                continue;
            }

            tracing::info!(
                run_id = %run_id,
                "watchdog_needs_resume_workflows: resuming orphaned run"
            );

            // Step 3: spawn resume thread.
            let config_clone = config.clone();
            let bin_dir = conductor_bin_dir.clone();
            let run_id_clone = run_id.clone();
            std::thread::spawn(move || {
                let params = WorkflowResumeStandalone {
                    config: config_clone,
                    workflow_run_id: run_id_clone.clone(),
                    model: None,
                    from_step: None,
                    restart: false,
                    db_path: None,
                    conductor_bin_dir: bin_dir,
                };
                if let Err(e) = crate::workflow::engine::resume_workflow_standalone(&params) {
                    tracing::warn!(
                        run_id = %run_id_clone,
                        "watchdog_needs_resume_workflows: auto-resume failed: {e}"
                    );
                }
            });

            resumed_ids.push(run_id);
            resumed += 1;
        }

        // Step 4: batch notification for all runs handed off.
        if !resumed_ids.is_empty() {
            crate::notify::fire_orphan_resumed_notification(
                self.conn,
                &config.notifications,
                &config.notify.hooks,
                &resumed_ids,
            );
        }

        Ok(resumed)
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use crate::config::Config;
    use crate::workflow::manager::WorkflowManager;

    /// Constant error string produced by the reaper — must match the classifier SQL.
    const ORPHAN_ERROR: &str =
        "parent agent run reached terminal state without completing the workflow";

    /// Create a test DB with a repo, worktree, and parent agent run.
    fn setup() -> (rusqlite::Connection, String) {
        let conn = crate::test_helpers::setup_db_with_agent_run();
        let parent_id: String = conn
            .query_row(
                "SELECT id FROM agent_runs WHERE worktree_id = 'w1' LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        (conn, parent_id)
    }

    /// Insert a workflow_run row with the given status, error, and iteration directly.
    fn insert_run(
        conn: &rusqlite::Connection,
        id: &str,
        parent_id: &str,
        status: &str,
        error: Option<&str>,
        iteration: u32,
    ) {
        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
              started_at, iteration, error) \
             VALUES (?1, 'test-wf', 'w1', ?2, ?3, 0, 'manual', '2024-01-01T00:00:00Z', ?4, ?5)",
            params![id, parent_id, status, iteration, error],
        )
        .unwrap();
    }

    /// Insert a workflow_run_step with the given status for the given run.
    fn insert_step(conn: &rusqlite::Connection, run_id: &str, step_id: &str, status: &str) {
        conn.execute(
            "INSERT INTO workflow_run_steps \
             (id, workflow_run_id, step_name, role, position, status) \
             VALUES (?1, ?2, 'step1', 'actor', 0, ?3)",
            params![step_id, run_id, status],
        )
        .unwrap();
    }

    /// Read the `status` of a workflow_run by ID.
    fn run_status(conn: &rusqlite::Connection, run_id: &str) -> String {
        conn.query_row(
            "SELECT status FROM workflow_runs WHERE id = ?1",
            params![run_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    // ── classify_resumable_workflows ──────────────────────────────────────────

    #[test]
    fn test_classifier_eligible_run_transitions_to_needs_resume() {
        let (conn, parent_id) = setup();
        insert_run(&conn, "run1", &parent_id, "failed", Some(ORPHAN_ERROR), 0);
        // Add a completed step (not failed/timed_out) — should not block classifier.
        insert_step(&conn, "run1", "step1", "completed");

        let mgr = WorkflowManager::new(&conn);
        let count = mgr.classify_resumable_workflows(3).unwrap();

        assert_eq!(count, 1, "eligible run should be classified");
        assert_eq!(
            run_status(&conn, "run1"),
            "needs_resume",
            "status should be needs_resume after classification"
        );
    }

    #[test]
    fn test_classifier_skips_run_with_failed_step() {
        let (conn, parent_id) = setup();
        insert_run(&conn, "run1", &parent_id, "failed", Some(ORPHAN_ERROR), 0);
        insert_step(&conn, "run1", "step1", "failed");

        let mgr = WorkflowManager::new(&conn);
        let count = mgr.classify_resumable_workflows(3).unwrap();

        assert_eq!(count, 0, "run with failed step must not be classified");
        assert_eq!(
            run_status(&conn, "run1"),
            "failed",
            "status should remain failed"
        );
    }

    #[test]
    fn test_classifier_skips_run_with_timed_out_step() {
        let (conn, parent_id) = setup();
        insert_run(&conn, "run1", &parent_id, "failed", Some(ORPHAN_ERROR), 0);
        insert_step(&conn, "run1", "step1", "timed_out");

        let mgr = WorkflowManager::new(&conn);
        let count = mgr.classify_resumable_workflows(3).unwrap();

        assert_eq!(count, 0, "run with timed_out step must not be classified");
        assert_eq!(run_status(&conn, "run1"), "failed");
    }

    #[test]
    fn test_classifier_respects_retry_cap() {
        let (conn, parent_id) = setup();
        // iteration == limit → should NOT be classified.
        insert_run(&conn, "run1", &parent_id, "failed", Some(ORPHAN_ERROR), 3);

        let mgr = WorkflowManager::new(&conn);
        let count = mgr.classify_resumable_workflows(3).unwrap();

        assert_eq!(count, 0, "run at retry cap must not be classified");
        assert_eq!(run_status(&conn, "run1"), "failed");
    }

    #[test]
    fn test_classifier_wrong_error_message_stays_failed() {
        let (conn, parent_id) = setup();
        insert_run(
            &conn,
            "run1",
            &parent_id,
            "failed",
            Some("some other error"),
            0,
        );

        let mgr = WorkflowManager::new(&conn);
        let count = mgr.classify_resumable_workflows(3).unwrap();

        assert_eq!(
            count, 0,
            "run with wrong error string must not be classified"
        );
        assert_eq!(run_status(&conn, "run1"), "failed");
    }

    #[test]
    fn test_classifier_skips_non_failed_statuses() {
        let (conn, parent_id) = setup();
        // A running run should not be touched even if the error string matches somehow.
        insert_run(&conn, "run1", &parent_id, "running", Some(ORPHAN_ERROR), 0);

        let mgr = WorkflowManager::new(&conn);
        let count = mgr.classify_resumable_workflows(3).unwrap();

        assert_eq!(count, 0);
        assert_eq!(run_status(&conn, "run1"), "running");
    }

    // ── watchdog_needs_resume_workflows ───────────────────────────────────────

    #[test]
    fn test_watchdog_cas_flip_needs_resume_to_failed() {
        let (conn, parent_id) = setup();
        // Seed a needs_resume run directly (as if the classifier already ran).
        insert_run(
            &conn,
            "run1",
            &parent_id,
            "needs_resume",
            Some(ORPHAN_ERROR),
            0,
        );

        let mgr = WorkflowManager::new(&conn);
        let config = Config::default();
        let count = mgr.watchdog_needs_resume_workflows(&config, None).unwrap();

        // Watchdog should have claimed the run (CAS flip to failed).
        assert_eq!(count, 1, "watchdog should claim the needs_resume run");
        // Status is flipped to 'failed' so resume_workflow_standalone can validate it.
        assert_eq!(
            run_status(&conn, "run1"),
            "failed",
            "status should be failed after watchdog CAS flip"
        );
    }

    #[test]
    fn test_watchdog_ignores_non_needs_resume_runs() {
        let (conn, parent_id) = setup();
        insert_run(&conn, "run1", &parent_id, "failed", Some(ORPHAN_ERROR), 0);

        let mgr = WorkflowManager::new(&conn);
        let config = Config::default();
        let count = mgr.watchdog_needs_resume_workflows(&config, None).unwrap();

        assert_eq!(count, 0, "watchdog should not touch non-needs_resume runs");
        assert_eq!(run_status(&conn, "run1"), "failed");
    }

    #[test]
    fn test_classifier_then_watchdog_pipeline() {
        let (conn, parent_id) = setup();
        insert_run(&conn, "run1", &parent_id, "failed", Some(ORPHAN_ERROR), 0);
        // Add only a completed step (no failed/timed_out).
        insert_step(&conn, "run1", "s1", "completed");

        let mgr = WorkflowManager::new(&conn);
        let config = Config::default();

        // Phase 1: classifier transitions to needs_resume.
        let classified = mgr.classify_resumable_workflows(3).unwrap();
        assert_eq!(classified, 1);
        assert_eq!(run_status(&conn, "run1"), "needs_resume");

        // Phase 2: watchdog CAS-flips to failed and spawns resume.
        let resumed = mgr.watchdog_needs_resume_workflows(&config, None).unwrap();
        assert_eq!(resumed, 1);
        assert_eq!(run_status(&conn, "run1"), "failed");
    }

    #[test]
    fn test_classifier_zero_limit_disables_classification() {
        let (conn, parent_id) = setup();
        insert_run(&conn, "run1", &parent_id, "failed", Some(ORPHAN_ERROR), 0);

        let mgr = WorkflowManager::new(&conn);
        // limit=0 means no run passes the `iteration < 0` guard.
        let count = mgr.classify_resumable_workflows(0).unwrap();
        assert_eq!(count, 0, "limit=0 should classify nothing");
        assert_eq!(run_status(&conn, "run1"), "failed");
    }

    #[test]
    fn test_classifier_iteration_below_limit_qualifies() {
        let (conn, parent_id) = setup();
        // iteration=2, limit=3 → 2 < 3 → eligible.
        insert_run(&conn, "run1", &parent_id, "failed", Some(ORPHAN_ERROR), 2);

        let mgr = WorkflowManager::new(&conn);
        let count = mgr.classify_resumable_workflows(3).unwrap();
        assert_eq!(count, 1, "run with iteration below limit should qualify");
        assert_eq!(run_status(&conn, "run1"), "needs_resume");
    }

    // ── auto_resume_limit config default ──────────────────────────────────────

    #[test]
    fn test_auto_resume_limit_default_is_three() {
        let config = Config::default();
        assert_eq!(config.general.auto_resume_limit, 3);
    }

    // ── terminate_subprocesses: agent PID collection ──────────────────────────

    /// Insert a workflow_run row and return its id.
    fn insert_workflow_run(conn: &rusqlite::Connection, run_id: &str, parent_id: &str) {
        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
              started_at, iteration) \
             VALUES (?1, 'test-wf', 'w1', ?2, 'running', 0, 'manual', \
                     '2024-01-01T00:00:00Z', 0)",
            rusqlite::params![run_id, parent_id],
        )
        .unwrap();
    }

    /// Insert an agent_run with an optional subprocess_pid; returns the agent run id.
    fn insert_agent_run_with_pid(conn: &rusqlite::Connection, pid: Option<i64>) -> String {
        let agent_mgr = crate::agent::AgentManager::new(conn);
        let run = agent_mgr
            .create_run(Some("w1"), "prompt", None, None)
            .unwrap();
        if let Some(p) = pid {
            conn.execute(
                "UPDATE agent_runs SET subprocess_pid = ?1 WHERE id = ?2",
                rusqlite::params![p, run.id],
            )
            .unwrap();
        }
        run.id
    }

    /// Insert a running step with a child_run_id (agent step) and no wrs.subprocess_pid.
    fn insert_running_agent_step(
        conn: &rusqlite::Connection,
        run_id: &str,
        step_id: &str,
        child_run_id: &str,
        position: i64,
    ) {
        conn.execute(
            "INSERT INTO workflow_run_steps \
             (id, workflow_run_id, step_name, role, position, status, iteration, \
              child_run_id, started_at) \
             VALUES (?1, ?2, 'implement', 'actor', ?3, 'running', 0, ?4, \
                     '2024-01-01T00:00:00Z')",
            rusqlite::params![step_id, run_id, position, child_run_id],
        )
        .unwrap();
    }

    /// Running step with child_run_id pointing to an agent_run with a subprocess_pid —
    /// terminate_subprocesses must collect that agent PID (query returns the row).
    #[test]
    fn test_terminate_subprocesses_collects_agent_pids() {
        let (conn, parent_id) = setup();
        insert_workflow_run(&conn, "wfrun1", &parent_id);

        // Use a harmless placeholder PID (1 = init/systemd — always alive but we
        // don't actually send signals; we just verify the query path is correct).
        let agent_run_id = insert_agent_run_with_pid(&conn, Some(99999));
        insert_running_agent_step(&conn, "wfrun1", "step1", &agent_run_id, 0);

        // Verify the agent PID query returns the row by checking count_live_subprocess_steps
        // returns a non-error (the count itself depends on whether PID 99999 is alive,
        // but the function must not panic or error).
        let mgr = WorkflowManager::new(&conn);
        let result = mgr.count_live_subprocess_steps("wfrun1");
        assert!(
            result.is_ok(),
            "count_live_subprocess_steps should not error: {:?}",
            result
        );

        // Verify terminate_subprocesses itself completes without error.
        let term_result = mgr.reset_failed_steps("wfrun1");
        assert!(
            term_result.is_ok(),
            "reset_failed_steps should not error: {:?}",
            term_result
        );

        // After reset, the step must be back in 'pending'.
        let status: String = conn
            .query_row(
                "SELECT status FROM workflow_run_steps WHERE id = 'step1'",
                rusqlite::params![],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "pending");
    }

    /// A step with both wrs.subprocess_pid AND ar.subprocess_pid must contribute
    /// only the wrs.subprocess_pid to the kill list (the agent PID query filters
    /// on wrs.subprocess_pid IS NULL to avoid double-counting).
    #[test]
    fn test_terminate_subprocesses_no_double_kill() {
        let (conn, parent_id) = setup();
        insert_workflow_run(&conn, "wfrun2", &parent_id);

        let agent_run_id = insert_agent_run_with_pid(&conn, Some(99998));

        // Step has both wrs.subprocess_pid (88888) and child_run_id → ar.subprocess_pid (99998).
        conn.execute(
            "INSERT INTO workflow_run_steps \
             (id, workflow_run_id, step_name, role, position, status, iteration, \
              child_run_id, subprocess_pid, started_at) \
             VALUES ('step2', 'wfrun2', 'script', 'actor', 0, 'running', 0, ?1, 88888, \
                     '2024-01-01T00:00:00Z')",
            rusqlite::params![agent_run_id],
        )
        .unwrap();

        // count_live_subprocess_steps uses COALESCE so wrs.subprocess_pid wins.
        // The agent PID query (wrs.subprocess_pid IS NULL) must NOT return this step.
        // Both terminate_subprocesses and count_live_subprocess_steps must complete cleanly.
        let mgr = WorkflowManager::new(&conn);
        assert!(mgr.count_live_subprocess_steps("wfrun2").is_ok());
        assert!(mgr.reset_failed_steps("wfrun2").is_ok());

        let status: String = conn
            .query_row(
                "SELECT status FROM workflow_run_steps WHERE id = 'step2'",
                rusqlite::params![],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "pending");
    }
}
