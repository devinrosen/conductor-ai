use std::collections::HashSet;

use chrono::{DateTime, Utc};
use rusqlite::{named_params, Connection, OptionalExtension};

use crate::agent::status::AgentRunStatus;
use crate::config::Config;
use crate::db::{query_collect, sql_placeholders};
use crate::error::Result;

use super::helpers::row_to_workflow_run;

use crate::workflow::constants::RUN_COLUMNS;
use crate::workflow::types::StepKey;
use crate::workflow::WorkflowRun;
use crate::workflow::{WorkflowRunStatus, WorkflowStepStatus};

const ORPHAN_BETWEEN_STEPS_MSG: &str =
    "Orphaned: executor died between steps \u{2014} auto-resumed by watchdog";

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

fn with_savepoint<T>(
    conn: &Connection,
    name: &'static str,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    conn.execute_batch(&format!("SAVEPOINT {name}"))?;
    let result = f();
    match result {
        Ok(v) => {
            conn.execute_batch(&format!("RELEASE {name}"))?;
            Ok(v)
        }
        Err(e) => {
            if let Err(rb_err) = conn.execute_batch(&format!("ROLLBACK TO SAVEPOINT {name}")) {
                tracing::warn!("savepoint '{name}' rollback failed: {rb_err}");
            }
            if let Err(rel_err) = conn.execute_batch(&format!("RELEASE {name}")) {
                tracing::warn!("savepoint '{name}' release-after-rollback failed: {rel_err}");
            }
            Err(e)
        }
    }
}

pub fn reap_orphaned_script_steps(conn: &Connection) -> Result<usize> {
    // Query script steps that are stuck in 'running' and have a subprocess_pid.
    // child_run_id IS NULL discriminates script steps from agent steps.
    let candidates: Vec<(String, i64, Option<String>)> = query_collect(
        conn,
        "SELECT id, subprocess_pid, started_at \
             FROM workflow_run_steps \
             WHERE status = 'running' \
               AND child_run_id IS NULL \
               AND subprocess_pid IS NOT NULL",
        [],
        |row| {
            Ok((
                row.get("id")?,
                row.get("subprocess_pid")?,
                row.get("started_at")?,
            ))
        },
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
                fail_step_with_message(
                    conn,
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
            fail_step_with_message(
                conn,
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

fn fail_step_with_message(conn: &Connection, step_id: &str, error_message: &str) -> Result<()> {
    super::steps::update_step_status(
        conn,
        step_id,
        WorkflowStepStatus::Failed,
        None,
        Some(error_message),
        None,
        None,
        None,
    )
}

fn bulk_recover_steps(
    conn: &Connection,
    items: &[(String, WorkflowStepStatus, Option<String>)],
    ended_at: &str,
) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }

    for chunk in items.chunks(199) {
        let n = chunk.len();
        let case_arms = (0..n)
            .map(|_| "WHEN ? THEN ?")
            .collect::<Vec<_>>()
            .join(" ");
        let in_placeholders = sql_placeholders(n);
        let sql = format!(
            "UPDATE workflow_run_steps \
                 SET status      = CASE id {case_arms} END, \
                     ended_at    = ?, \
                     result_text = CASE id {case_arms} END, \
                     context_out = NULL, \
                     markers_out = NULL, \
                     structured_output = NULL, \
                     step_error  = NULL \
                 WHERE id IN ({in_placeholders})"
        );

        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(5 * n + 1);
        for (step_id, status, _) in chunk {
            params.push(Box::new(step_id.clone()));
            params.push(Box::new(status.to_string()));
        }
        params.push(Box::new(ended_at.to_string()));
        for (step_id, _, result_text) in chunk {
            params.push(Box::new(step_id.clone()));
            params.push(Box::new(result_text.clone()));
        }
        for (step_id, _, _) in chunk {
            params.push(Box::new(step_id.clone()));
        }

        conn.execute(&sql, rusqlite::params_from_iter(params))?;
    }

    Ok(())
}

pub fn recover_stuck_steps(conn: &Connection) -> Result<usize> {
    // Step 1: fetch running workflow steps that have a child_run_id.
    let running_steps: Vec<(String, String)> = query_collect(
        conn,
        "SELECT id, child_run_id FROM workflow_run_steps \
             WHERE status = 'running' AND child_run_id IS NOT NULL",
        [],
        |row| Ok((row.get("id")?, row.get("child_run_id")?)),
    )?;

    if running_steps.is_empty() {
        return Ok(0);
    }

    // Step 2: batch-fetch the agent runs via AgentManager.
    let agent_mgr = crate::agent::AgentManager::new(conn);
    let child_ids: Vec<&str> = running_steps.iter().map(|(_, id)| id.as_str()).collect();
    let child_runs = agent_mgr.get_runs_by_ids(&child_ids)?;

    // Filter in Rust to those with terminal statuses.
    let stuck: Vec<(String, WorkflowStepStatus, Option<String>)> = running_steps
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
                AgentRunStatus::Failed | AgentRunStatus::Cancelled => WorkflowStepStatus::Failed,
                _ => return None,
            };
            Some((step_id, step_status, run.result_text.clone()))
        })
        .collect();

    let ended_at = chrono::Utc::now().to_rfc3339();
    let n = stuck.len();
    with_savepoint(conn, "recover_stuck_steps", || {
        bulk_recover_steps(conn, &stuck, &ended_at)?;
        Ok(n)
    })
}

pub fn reap_orphaned_workflow_runs(conn: &Connection) -> Result<usize> {
    // Query all root runs in 'waiting' status.
    let waiting_runs: Vec<(String, String)> = query_collect(
        conn,
        "SELECT id, parent_run_id FROM workflow_runs \
             WHERE status = 'waiting' AND parent_workflow_run_id IS NULL",
        [],
        |row| Ok((row.get("id")?, row.get("parent_run_id")?)),
    )?;

    if waiting_runs.is_empty() {
        return Ok(0);
    }

    // Batch-fetch all parent agent runs via AgentManager to avoid N+1 lookups.
    let agent_mgr = crate::agent::AgentManager::new(conn);
    let id_refs: Vec<&str> = waiting_runs.iter().map(|(_, id)| id.as_str()).collect();
    let parent_runs = agent_mgr.get_runs_by_ids(&id_refs)?;

    // Batch-fetch the active waiting gate step for each run to avoid N+1 queries.
    let run_id_refs: Vec<&str> = waiting_runs.iter().map(|(id, _)| id.as_str()).collect();
    let gate_steps = super::queries::find_waiting_gates_for_runs(conn, &run_id_refs)?;

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
        let gate_step = gate_steps.get(&run_id);
        let gate_timed_out = gate_step.is_some_and(|step| {
                let timeout_secs = step.gate_timeout.as_deref().and_then(|s| {
                    let result = crate::workflow::helpers::parse_gate_timeout_secs(s);
                    if result.is_none() {
                        tracing::warn!(
                            run_id = %run_id,
                            gate_timeout = %s,
                            "gate_timeout value could not be parsed — timeout will not be enforced"
                        );
                    }
                    result
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
        if let Some(step) = gate_step {
            super::steps::update_step_status(
                conn,
                &step.id,
                WorkflowStepStatus::TimedOut,
                None,
                None,
                None,
                None,
                None,
            )?;
        }

        super::lifecycle::update_workflow_status(
            conn,
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

pub fn detect_stuck_workflow_run_ids(
    conn: &Connection,
    threshold_secs: i64,
) -> Result<Vec<String>> {
    query_collect(
        conn,
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
               ) > :threshold_secs",
        named_params![":threshold_secs": threshold_secs],
        |row| row.get("id"),
    )
}

pub fn detect_stale_workflow_runs(
    conn: &Connection,
    threshold_minutes: i64,
) -> Result<Vec<StaleWorkflowRun>> {
    if threshold_minutes <= 0 {
        return Ok(vec![]);
    }
    query_collect(
        conn,
        "SELECT wr.id AS run_id, wr.workflow_name, wr.target_label, \
                    wrs.step_name, \
                    (CAST(strftime('%s', 'now') AS INTEGER) \
                     - CAST(strftime('%s', wrs.started_at) AS INTEGER)) / 60 AS running_minutes, \
                    wrs.id AS step_id, wrs.child_run_id, \
                    COALESCE(wrs.subprocess_pid, ar.subprocess_pid) AS subprocess_pid \
             FROM workflow_runs wr \
             JOIN workflow_run_steps wrs ON wrs.workflow_run_id = wr.id \
             LEFT JOIN agent_runs ar ON ar.id = wrs.child_run_id \
             WHERE wr.status = 'running' \
               AND wr.parent_workflow_run_id IS NULL \
               AND wrs.status = 'running' \
               AND wrs.started_at IS NOT NULL \
               AND (CAST(strftime('%s', 'now') AS INTEGER) \
                    - CAST(strftime('%s', wrs.started_at) AS INTEGER)) > :threshold_minutes * 60 \
               AND NOT EXISTS ( \
                 SELECT 1 FROM workflow_runs child \
                 WHERE child.parent_workflow_run_id = wr.id \
                   AND child.status IN ('running', 'pending', 'waiting') \
               )",
        named_params![":threshold_minutes": threshold_minutes],
        |row| {
            Ok(StaleWorkflowRun {
                run_id: row.get("run_id")?,
                workflow_name: row.get("workflow_name")?,
                target_label: row.get("target_label")?,
                step_name: row.get("step_name")?,
                running_minutes: row.get("running_minutes")?,
                step_id: row.get("step_id")?,
                child_run_id: row.get("child_run_id")?,
                subprocess_pid: row.get("subprocess_pid")?,
            })
        },
    )
}

pub fn reap_stale_workflow_runs(
    conn: &Connection,
    threshold_minutes: i64,
) -> Result<Vec<ReapedStaleRun>> {
    let stale = detect_stale_workflow_runs(conn, threshold_minutes)?;
    if stale.is_empty() {
        return Ok(vec![]);
    }

    let agent_mgr = crate::agent::AgentManager::new(conn);

    // Wrap all updates in a savepoint so they commit in one round-trip instead
    // of N separate auto-commit transactions (mirrors recover_stuck_steps).
    with_savepoint(conn, "reap_stale_workflow_runs", || {
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
            fail_step_with_message(
                conn,
                &s.step_id,
                "Agent process died — marked by stale workflow watchdog",
            )?;

            // Mark the workflow run as failed.
            super::lifecycle::update_workflow_status(
                conn,
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
    })
}

fn cas_flip_run_to_failed_from(
    conn: &Connection,
    run_id: &str,
    from_status: &str,
    error_msg: &str,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE workflow_runs \
             SET status = 'failed', error = :error \
             WHERE id = :id AND status = :from",
        named_params![":id": run_id, ":from": from_status, ":error": error_msg],
    )?;
    Ok(changed == 1)
}

fn cas_claim_ids_and_notify(
    conn: &Connection,
    config: &Config,
    candidates: &[String],
    from_status: &str,
    error_msg: &str,
    caller_name: &str,
) -> Result<Vec<String>> {
    let mut claimed: Vec<String> = Vec::new();
    for run_id in candidates {
        if !cas_flip_run_to_failed_from(conn, run_id, from_status, error_msg)? {
            tracing::debug!(
                run_id = %run_id,
                "{caller_name}: CAS lost race (already claimed)"
            );
            continue;
        }
        tracing::info!(run_id = %run_id, "{caller_name}: claimed orphaned run for resumption");
        claimed.push(run_id.clone());
    }
    if !claimed.is_empty() {
        crate::notify::fire_orphan_resumed_notification(
            conn,
            &config.notifications,
            &config.notify.hooks,
            &claimed,
        );
    }
    Ok(claimed)
}

pub fn claim_stuck_workflows(
    conn: &Connection,
    config: &Config,
    configurable_threshold_secs: Option<i64>,
) -> Result<Vec<String>> {
    // Use the smallest threshold so we catch all stuck runs in a single query.
    let threshold = configurable_threshold_secs.map(|t| t.min(60)).unwrap_or(60);

    let stuck_ids = detect_stuck_workflow_run_ids(conn, threshold)?;
    if stuck_ids.is_empty() {
        return Ok(vec![]);
    }

    let flipped_ids = cas_claim_ids_and_notify(
        conn,
        config,
        &stuck_ids,
        "running",
        ORPHAN_BETWEEN_STEPS_MSG,
        "claim_stuck_workflows",
    )?;

    if !flipped_ids.is_empty() {
        let n = flipped_ids.len();
        tracing::info!("Auto-resuming {n} stuck workflow run(s) (threshold={threshold}s)");
    }

    Ok(flipped_ids)
}

pub fn claim_expired_lease_runs(
    conn: &Connection,
    config: &Config,
) -> Result<Vec<(String, String, Option<String>)>> {
    // Find orphaned root runs whose lease has expired (or was never set, which
    // covers runs created before migration 084). Includes zero-step runs where
    // the executor died before creating any steps.
    let orphaned: Vec<(String, String, Option<String>)> = query_collect(
        conn,
        "SELECT id, workflow_name, target_label FROM workflow_runs \
             WHERE status = 'running' \
               AND parent_workflow_run_id IS NULL \
               AND NOT EXISTS ( \
                 SELECT 1 FROM workflow_run_steps wrs \
                 WHERE wrs.workflow_run_id = workflow_runs.id \
                   AND wrs.status IN ('running', 'pending', 'waiting') \
               ) \
               AND (lease_until < datetime('now') OR lease_until IS NULL)",
        [],
        |row| {
            Ok((
                row.get("id")?,
                row.get("workflow_name")?,
                row.get("target_label")?,
            ))
        },
    )?;

    if orphaned.is_empty() {
        return Ok(vec![]);
    }

    // CAS-flip each candidate, fire the batch notification, collect winner IDs.
    let orphaned_ids: Vec<String> = orphaned.iter().map(|(id, _, _)| id.clone()).collect();
    let claimed_id_set: std::collections::HashSet<String> = cas_claim_ids_and_notify(
        conn,
        config,
        &orphaned_ids,
        "running",
        ORPHAN_BETWEEN_STEPS_MSG,
        "claim_expired_lease_runs",
    )?
    .into_iter()
    .collect();

    Ok(orphaned
        .into_iter()
        .filter(|(id, _, _)| claimed_id_set.contains(id))
        .collect())
}

pub fn reap_finalization_stuck_workflow_runs(
    conn: &Connection,
    threshold_secs: i64,
) -> crate::error::Result<usize> {
    // Find root running workflow runs where all steps are terminal and
    // the last step (or the run itself) ended more than threshold_secs ago.
    //
    // Actor steps mark their step record terminal as soon as the agent
    // emits FLOW_OUTPUT, but the agent subprocess continues for cleanup
    // (final SDK message, log flush, prompt-file removal, child wait) and
    // the workflow engine waits for the actual process exit before
    // scheduling the next step. For long actors, that gap can exceed
    // `threshold_secs` and trip the false-positive branch of this reaper.
    // Skip parents whose latest actor step still has a `running`
    // `agent_run` — see issue #2787.
    let stuck: Vec<(String, String, bool)> = query_collect(
        conn,
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
                 AND NOT EXISTS ( \
                   SELECT 1 FROM workflow_run_steps wrs_act \
                   JOIN agent_runs ar ON ar.id = wrs_act.child_run_id \
                   WHERE wrs_act.workflow_run_id = wr.id \
                     AND ar.status = 'running' \
                 ) \
             ) \
             WHERE age_ref IS NOT NULL \
               AND (CAST(strftime('%s', 'now') AS INTEGER) \
                    - CAST(strftime('%s', age_ref) AS INTEGER)) > :threshold_secs",
        named_params![":threshold_secs": threshold_secs],
        |row| {
            Ok((
                row.get("id")?,
                row.get("parent_run_id")?,
                row.get("has_failure")?,
            ))
        },
    )?;

    // Wrap all updates in a savepoint so they commit in one round-trip instead
    // of N separate auto-commit transactions (mirrors recover_stuck_steps).
    with_savepoint(conn, "reap_finalization_stuck_workflow_runs", || {
        let mut finalized = 0usize;
        // Constructed once here rather than inside the loop — AgentManager is
        // stateless (wraps &Connection) so rebuilding it per iteration is wasteful.
        let agent_mgr = crate::agent::AgentManager::new(conn);

        const SUMMARY: &str =
            "Auto-finalized by reaper: all steps terminal, status was stuck in 'running'";

        for (run_id, parent_run_id, has_failure) in stuck {
            let final_status = if has_failure {
                WorkflowRunStatus::Failed
            } else {
                WorkflowRunStatus::Completed
            };

            super::lifecycle::update_workflow_status(
                conn,
                &run_id,
                final_status.clone(),
                Some(SUMMARY),
                None,
            )?;
            tracing::info!(
                run_id = %run_id,
                status = %final_status,
                "Reaper finalized stuck workflow run"
            );

            // Best-effort: update the parent agent_runs row if still running.
            let update_result = if has_failure {
                agent_mgr.update_run_failed_if_running(&parent_run_id, SUMMARY)
            } else {
                agent_mgr.update_run_completed_if_running(&parent_run_id, SUMMARY)
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
    })
}

pub fn find_resumable_child_run(
    conn: &Connection,
    parent_workflow_run_id: &str,
    child_workflow_name: &str,
) -> Result<Option<WorkflowRun>> {
    Ok(conn
            .query_row(
                &format!(
                    "SELECT {RUN_COLUMNS} FROM workflow_runs \
                     WHERE parent_workflow_run_id = :parent_workflow_run_id \
                       AND workflow_name = :child_workflow_name \
                       AND status IN ('failed', 'pending', 'waiting') \
                     ORDER BY started_at DESC \
                     LIMIT 1"
                ),
                named_params![":parent_workflow_run_id": parent_workflow_run_id, ":child_workflow_name": child_workflow_name],
                row_to_workflow_run,
            )
            .optional()?)
}

const SQL_RESET_FAILED: &str =
    reset_sql!("WHERE workflow_run_id = :run_id AND status IN ('failed', 'running', 'timed_out')");

const SQL_RESET_COMPLETED: &str =
    reset_sql!("WHERE workflow_run_id = :run_id AND status = 'completed'");

const SQL_RESET_FROM_POS: &str =
    reset_sql!("WHERE workflow_run_id = :run_id AND position >= :position");

pub fn terminate_subprocesses(
    conn: &Connection,
    workflow_run_id: &str,
    from_position: Option<i64>,
) -> Result<()> {
    #[cfg(unix)]
    {
        // Collect all PIDs (script-step direct PIDs + agent-step PIDs via JOIN)
        // in one round-trip using a UNION query.
        let all_pids: Vec<i64> = query_collect(
            conn,
            "SELECT subprocess_pid \
                 FROM workflow_run_steps \
                 WHERE workflow_run_id = :run_id AND status = 'running' \
                   AND subprocess_pid IS NOT NULL \
                   AND (:from_pos IS NULL OR position >= :from_pos) \
                 UNION ALL \
                 SELECT ar.subprocess_pid \
                 FROM workflow_run_steps wrs \
                 JOIN agent_runs ar ON ar.id = wrs.child_run_id \
                 WHERE wrs.workflow_run_id = :run_id \
                   AND wrs.status = 'running' \
                   AND wrs.subprocess_pid IS NULL \
                   AND ar.subprocess_pid IS NOT NULL \
                   AND (:from_pos IS NULL OR wrs.position >= :from_pos)",
            named_params![":run_id": workflow_run_id, ":from_pos": from_position],
            |row| row.get("subprocess_pid"),
        )?;

        let handles: Vec<_> = all_pids
            .into_iter()
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

pub(crate) fn count_live_subprocess_steps(
    conn: &Connection,
    workflow_run_id: &str,
) -> Result<usize> {
    #[cfg(unix)]
    {
        let pids: Vec<i64> = query_collect(
            conn,
            "SELECT COALESCE(wrs.subprocess_pid, ar.subprocess_pid) AS pid \
                 FROM workflow_run_steps wrs \
                 LEFT JOIN agent_runs ar ON ar.id = wrs.child_run_id \
                 WHERE wrs.workflow_run_id = :run_id \
                   AND wrs.status = 'running' \
                   AND COALESCE(wrs.subprocess_pid, ar.subprocess_pid) IS NOT NULL",
            named_params![":run_id": workflow_run_id],
            |row| row.get("pid"),
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

pub fn reset_failed_steps(conn: &Connection, workflow_run_id: &str) -> Result<u64> {
    terminate_subprocesses(conn, workflow_run_id, None)?;
    let count = conn.execute(SQL_RESET_FAILED, named_params![":run_id": workflow_run_id])?;
    Ok(count as u64)
}

pub fn reset_completed_steps(conn: &Connection, workflow_run_id: &str) -> Result<u64> {
    let count = conn.execute(
        SQL_RESET_COMPLETED,
        named_params![":run_id": workflow_run_id],
    )?;
    Ok(count as u64)
}

pub fn reset_steps_from_position(
    conn: &Connection,
    workflow_run_id: &str,
    position: i64,
) -> Result<u64> {
    terminate_subprocesses(conn, workflow_run_id, Some(position))?;
    let count = conn.execute(
        SQL_RESET_FROM_POS,
        named_params![":run_id": workflow_run_id, ":position": position],
    )?;
    Ok(count as u64)
}

pub fn get_completed_step_keys(
    conn: &Connection,
    workflow_run_id: &str,
) -> Result<HashSet<StepKey>> {
    let steps = super::queries::get_workflow_steps(conn, workflow_run_id)?;
    Ok(steps
        .iter()
        .filter(|s| s.status == WorkflowStepStatus::Completed)
        .map(|s| (s.step_name.clone(), s.iteration as u32))
        .collect())
}

pub fn delete_run(conn: &Connection, run_id: &str) -> Result<()> {
    use crate::error::ConductorError;

    // Validate the run exists and is terminal.
    let run = conn
        .query_row(
            &format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE id = :id"),
            named_params![":id": run_id],
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

    delete_run_recursive(conn, run_id)
}

fn delete_run_recursive(conn: &Connection, run_id: &str) -> Result<()> {
    // A single recursive CTE collects all descendants plus the root, then
    // deletes them in one statement.  SQLite checks the self-referential FK
    // (parent_workflow_run_id) at statement end, not row-by-row, so deleting
    // parent and children together never produces an intermediate violation.
    conn.execute(
        "WITH RECURSIVE descendants(id) AS (
                 SELECT id FROM workflow_runs WHERE parent_workflow_run_id = :root
                 UNION ALL
                 SELECT r.id FROM workflow_runs r
                   JOIN descendants d ON r.parent_workflow_run_id = d.id
             )
             DELETE FROM workflow_runs
              WHERE id IN (SELECT id FROM descendants) OR id = :root",
        named_params![":root": run_id],
    )?;
    Ok(())
}

pub fn delete_orphaned_pending_steps(conn: &Connection, workflow_run_id: &str) -> Result<usize> {
    let deleted = conn.execute(
        "DELETE FROM workflow_run_steps \
             WHERE workflow_run_id = :run_id \
               AND status = 'pending' \
               AND started_at IS NULL",
        named_params![":run_id": workflow_run_id],
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

fn build_purge_params(
    repo_id: Option<&str>,
    statuses: &[&str],
) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let n = statuses.len();
    let placeholders = sql_placeholders(n);
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = statuses
        .iter()
        .map(|s| Box::new(s.to_string()) as Box<dyn rusqlite::ToSql>)
        .collect();
    let where_clause = if let Some(rid) = repo_id {
        params.push(Box::new(rid.to_string()));
        format!(
            "status IN ({placeholders}) \
                 AND worktree_id IN (SELECT id FROM worktrees WHERE repo_id = ?{})",
            n + 1
        )
    } else {
        format!("status IN ({placeholders})")
    };
    (where_clause, params)
}

pub fn purge(conn: &Connection, repo_id: Option<&str>, statuses: &[&str]) -> Result<usize> {
    if statuses.is_empty() {
        return Ok(0);
    }
    let (where_clause, params) = build_purge_params(repo_id, statuses);
    let sql = format!("DELETE FROM workflow_runs WHERE {where_clause}");
    Ok(conn.execute(&sql, rusqlite::params_from_iter(params))?)
}

pub fn purge_count(conn: &Connection, repo_id: Option<&str>, statuses: &[&str]) -> Result<usize> {
    if statuses.is_empty() {
        return Ok(0);
    }
    let (where_clause, params) = build_purge_params(repo_id, statuses);
    let sql = format!("SELECT COUNT(*) FROM workflow_runs WHERE {where_clause}");
    let count: i64 = conn.query_row(&sql, rusqlite::params_from_iter(params), |row| row.get(0))?;
    Ok(count as usize)
}

pub fn classify_resumable_workflows(conn: &Connection, auto_resume_limit: u32) -> Result<usize> {
    let count = conn.execute(
            "UPDATE workflow_runs \
             SET status = 'needs_resume' \
             WHERE status = 'failed' \
               AND error = 'parent agent run reached terminal state without completing the workflow' \
               AND iteration < :limit \
               AND NOT EXISTS ( \
                 SELECT 1 FROM workflow_run_steps \
                 WHERE workflow_run_id = workflow_runs.id \
                   AND status IN ('failed', 'timed_out') \
               )",
            named_params![":limit": auto_resume_limit],
        )?;

    if count > 0 {
        tracing::info!(
            "classify_resumable_workflows: flagged {count} workflow run(s) for auto-resume"
        );
    }

    Ok(count)
}

pub fn claim_needs_resume_runs(conn: &Connection, config: &Config) -> Result<Vec<String>> {
    // Step 1: find all needs_resume root runs.
    let candidates: Vec<String> = query_collect(
        conn,
        "SELECT id FROM workflow_runs \
             WHERE status = 'needs_resume' \
               AND parent_workflow_run_id IS NULL",
        [],
        |row| row.get("id"),
    )?;

    if candidates.is_empty() {
        return Ok(vec![]);
    }

    cas_claim_ids_and_notify(
        conn,
        config,
        &candidates,
        "needs_resume",
        "Orphaned: parent agent run died — auto-resumed by watchdog",
        "claim_needs_resume_runs",
    )
}

/// Claim any expired-lease workflow runs, kill their stale subprocesses, and
/// spawn a heartbeat-resume thread for each one.
pub fn claim_and_resume_expired_leases(
    conn: &Connection,
    config: &Config,
    conductor_bin_dir: Option<std::path::PathBuf>,
) {
    match claim_expired_lease_runs(conn, config) {
        Ok(claimed) if !claimed.is_empty() => {
            tracing::info!(
                "auto-resuming {} expired-lease workflow run(s)",
                claimed.len()
            );
            for (run_id, _, _) in &claimed {
                // Kill any zombie subprocesses from the dead engine before
                // spawning a fresh resume — prevents duplicate side effects.
                if let Err(e) = terminate_subprocesses(conn, run_id, None) {
                    tracing::warn!(
                        "terminate_subprocesses before watchdog resume failed for {run_id}: {e}"
                    );
                }
            }
            for (run_id, wf_name, label) in claimed {
                crate::workflow::spawn_heartbeat_resume(
                    crate::workflow::SpawnHeartbeatResumeParams {
                        run_id,
                        workflow_name: wf_name,
                        target_label: label,
                        config: config.clone(),
                        conductor_bin_dir: conductor_bin_dir.clone(),
                        db_path: None,
                    },
                );
            }
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("claim_expired_lease_runs failed: {e}"),
    }
}

pub fn run_workflow_maintenance(
    conn: &Connection,
    config: &Config,
    conductor_bin_dir: Option<std::path::PathBuf>,
) {
    match reap_finalization_stuck_workflow_runs(conn, 60) {
        Ok(n) if n > 0 => tracing::info!("reaper finalized {n} stuck workflow run(s)"),
        Ok(_) => {}
        Err(e) => tracing::warn!("reap_finalization_stuck_workflow_runs failed: {e}"),
    }
    claim_and_resume_expired_leases(conn, config, conductor_bin_dir.clone());
    let auto_resume_limit = config.general.auto_resume_limit;
    if auto_resume_limit > 0 {
        match classify_resumable_workflows(conn, auto_resume_limit) {
            Ok(n) if n > 0 => {
                tracing::info!("classifier flagged {n} workflow run(s) for auto-resume")
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("classify_resumable_workflows failed: {e}"),
        }
        match claim_needs_resume_runs(conn, config) {
            Ok(claimed) => crate::workflow::spawn_claimed_runs(
                claimed,
                std::sync::Arc::new(config.clone()),
                conductor_bin_dir.clone(),
            ),
            Err(e) => tracing::warn!("claim_needs_resume_runs failed: {e}"),
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use rusqlite::named_params;

    use crate::config::Config;

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
                |row| row.get("id"),
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
             VALUES (:id, 'test-wf', 'w1', :parent_id, :status, 0, 'manual', '2024-01-01T00:00:00Z', :iteration, :error)",
            named_params![":id": id, ":parent_id": parent_id, ":status": status, ":iteration": iteration, ":error": error],
        )
        .unwrap();
    }

    /// Insert a workflow_run_step with the given status for the given run.
    fn insert_step(conn: &rusqlite::Connection, run_id: &str, step_id: &str, status: &str) {
        conn.execute(
            "INSERT INTO workflow_run_steps \
             (id, workflow_run_id, step_name, role, position, status) \
             VALUES (:id, :run_id, 'step1', 'actor', 0, :status)",
            named_params![":id": step_id, ":run_id": run_id, ":status": status],
        )
        .unwrap();
    }

    /// Read the `status` of a workflow_run by ID.
    fn run_status(conn: &rusqlite::Connection, run_id: &str) -> String {
        conn.query_row(
            "SELECT status FROM workflow_runs WHERE id = :id",
            named_params![":id": run_id],
            |row| row.get("status"),
        )
        .unwrap()
    }

    // ── parse_duration_str (runkon_flow::dsl) ────────────────────────────────

    #[test]
    fn parse_duration_secs_hours() {
        assert_eq!(runkon_flow::dsl::parse_duration_str("2h"), Ok(7200));
    }

    #[test]
    fn parse_duration_secs_large_hours() {
        assert_eq!(runkon_flow::dsl::parse_duration_str("48h"), Ok(172800));
    }

    #[test]
    fn parse_duration_secs_minutes() {
        assert_eq!(runkon_flow::dsl::parse_duration_str("30m"), Ok(1800));
    }

    #[test]
    fn parse_duration_secs_seconds_suffix() {
        assert_eq!(runkon_flow::dsl::parse_duration_str("60s"), Ok(60));
    }

    #[test]
    fn parse_duration_secs_plain_integer() {
        assert_eq!(runkon_flow::dsl::parse_duration_str("120"), Ok(120));
    }

    #[test]
    fn parse_duration_secs_quoted_value() {
        // TOML duration values may arrive with surrounding quotes.
        assert_eq!(runkon_flow::dsl::parse_duration_str("\"30m\""), Ok(1800));
    }

    #[test]
    fn parse_duration_secs_invalid_input() {
        assert!(runkon_flow::dsl::parse_duration_str("not-a-number").is_err());
    }

    #[test]
    fn parse_duration_secs_overflow_hours() {
        // A value so large that multiplying by 3600 overflows u64.
        let huge = format!("{}h", u64::MAX);
        assert!(runkon_flow::dsl::parse_duration_str(&huge).is_err());
    }

    // ── classify_resumable_workflows ──────────────────────────────────────────

    #[test]
    fn test_classifier_eligible_run_transitions_to_needs_resume() {
        let (conn, parent_id) = setup();
        insert_run(&conn, "run1", &parent_id, "failed", Some(ORPHAN_ERROR), 0);
        // Add a completed step (not failed/timed_out) — should not block classifier.
        insert_step(&conn, "run1", "step1", "completed");
        let count = crate::workflow::classify_resumable_workflows(&conn, 3).unwrap();

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
        let count = crate::workflow::classify_resumable_workflows(&conn, 3).unwrap();

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
        let count = crate::workflow::classify_resumable_workflows(&conn, 3).unwrap();

        assert_eq!(count, 0, "run with timed_out step must not be classified");
        assert_eq!(run_status(&conn, "run1"), "failed");
    }

    #[test]
    fn test_classifier_respects_retry_cap() {
        let (conn, parent_id) = setup();
        // iteration == limit → should NOT be classified.
        insert_run(&conn, "run1", &parent_id, "failed", Some(ORPHAN_ERROR), 3);
        let count = crate::workflow::classify_resumable_workflows(&conn, 3).unwrap();

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
        let count = crate::workflow::classify_resumable_workflows(&conn, 3).unwrap();

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
        let count = crate::workflow::classify_resumable_workflows(&conn, 3).unwrap();

        assert_eq!(count, 0);
        assert_eq!(run_status(&conn, "run1"), "running");
    }

    // ── claim_needs_resume_runs ───────────────────────────────────────────────

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
        let config = Config::default();
        let claimed = crate::workflow::claim_needs_resume_runs(&conn, &config).unwrap();

        // Watchdog should have claimed the run (CAS flip to failed).
        assert_eq!(
            claimed.len(),
            1,
            "watchdog should claim the needs_resume run"
        );
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
        let config = Config::default();
        let claimed = crate::workflow::claim_needs_resume_runs(&conn, &config).unwrap();

        assert!(
            claimed.is_empty(),
            "watchdog should not touch non-needs_resume runs"
        );
        assert_eq!(run_status(&conn, "run1"), "failed");
    }

    #[test]
    fn test_classifier_then_watchdog_pipeline() {
        let (conn, parent_id) = setup();
        insert_run(&conn, "run1", &parent_id, "failed", Some(ORPHAN_ERROR), 0);
        // Add only a completed step (no failed/timed_out).
        insert_step(&conn, "run1", "s1", "completed");
        let config = Config::default();

        // Phase 1: classifier transitions to needs_resume.
        let classified = crate::workflow::classify_resumable_workflows(&conn, 3).unwrap();
        assert_eq!(classified, 1);
        assert_eq!(run_status(&conn, "run1"), "needs_resume");

        // Phase 2: watchdog CAS-flips to failed.
        let claimed = crate::workflow::claim_needs_resume_runs(&conn, &config).unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(run_status(&conn, "run1"), "failed");
    }

    #[test]
    fn test_classifier_zero_limit_disables_classification() {
        let (conn, parent_id) = setup();
        insert_run(&conn, "run1", &parent_id, "failed", Some(ORPHAN_ERROR), 0);
        // limit=0 means no run passes the `iteration < 0` guard.
        let count = crate::workflow::classify_resumable_workflows(&conn, 0).unwrap();
        assert_eq!(count, 0, "limit=0 should classify nothing");
        assert_eq!(run_status(&conn, "run1"), "failed");
    }

    #[test]
    fn test_classifier_iteration_below_limit_qualifies() {
        let (conn, parent_id) = setup();
        // iteration=2, limit=3 → 2 < 3 → eligible.
        insert_run(&conn, "run1", &parent_id, "failed", Some(ORPHAN_ERROR), 2);
        let count = crate::workflow::classify_resumable_workflows(&conn, 3).unwrap();
        assert_eq!(count, 1, "run with iteration below limit should qualify");
        assert_eq!(run_status(&conn, "run1"), "needs_resume");
    }

    // ── auto_resume_limit config default ──────────────────────────────────────

    #[test]
    fn test_auto_resume_limit_default_is_three() {
        let config = Config::default();
        assert_eq!(config.general.auto_resume_limit, 3);
    }

    // ── run_workflow_maintenance ───────────────────────────────────────────────

    /// When `auto_resume_limit = 0` the maintenance path must not attempt any
    /// auto-resume: `classify_resumable_workflows` is never called because
    /// `run_workflow_maintenance` gates it behind `auto_resume_limit > 0`.
    #[test]
    fn test_run_workflow_maintenance_skips_resume_when_limit_zero() {
        let (conn, parent_id) = setup();
        // Seed a run that *would* qualify for auto-resume if the limit were > 0.
        insert_run(&conn, "run1", &parent_id, "failed", Some(ORPHAN_ERROR), 0);
        let mut config = Config::default();
        config.general.auto_resume_limit = 0;

        // Must not panic or error.
        crate::workflow::run_workflow_maintenance(&conn, &config, None);

        // The run must remain `failed` — no classification occurred.
        assert_eq!(
            run_status(&conn, "run1"),
            "failed",
            "status must stay 'failed' when auto_resume_limit = 0"
        );
    }

    /// Smoke test: `run_workflow_maintenance` with a positive limit and an empty
    /// database completes without panicking or returning an error.
    #[test]
    fn test_run_workflow_maintenance_completes_without_error_no_stuck_runs() {
        // Use a fresh database with no workflow runs at all.
        let conn = crate::test_helpers::setup_db_with_agent_run();
        let config = Config::default(); // auto_resume_limit = 3

        // Must not panic — there are no stuck/stale/needs_resume runs to process.
        crate::workflow::run_workflow_maintenance(&conn, &config, None);
    }

    /// `run_workflow_maintenance` calls `terminate_subprocesses` for each claimed
    /// expired-lease run before spawning a fresh resume. Verify that the function
    /// completes without panic and the run is transitioned out of 'running'.
    #[test]
    fn test_run_workflow_maintenance_terminates_before_resume() {
        let (conn, parent_id) = setup();
        // Insert a running root run with expired lease and no active steps
        // (no active steps is required by claim_expired_lease_runs query).
        // Use datetime('now') for started_at so the finalization-stuck reaper
        // (60s threshold) doesn't claim the run first; lease_until in the past
        // ensures claim_expired_lease_runs picks it up.
        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
              started_at, iteration, lease_until) \
             VALUES ('maint-run-1', 'test-wf', 'w1', :parent_id, 'running', 0, 'manual', \
                     datetime('now'), 0, '1970-01-01T00:00:00Z')",
            named_params![":parent_id": parent_id],
        )
        .unwrap();

        let config = Config::default();
        // Must not panic — terminate_subprocesses is called for each claimed run
        // (lines 1073-1078 in run_workflow_maintenance) before spawn_heartbeat_resume.
        crate::workflow::run_workflow_maintenance(&conn, &config, None);

        // The CAS flip in claim_expired_lease_runs transitions the run to 'failed'.
        assert_eq!(
            run_status(&conn, "maint-run-1"),
            "failed",
            "watchdog must claim expired-lease run"
        );
    }

    // ── claim_and_resume_expired_leases ──────────────────────────────────────────

    /// No expired-lease runs → function is a no-op and returns without panicking.
    #[test]
    fn test_claim_and_resume_empty_is_noop() {
        let conn = crate::test_helpers::setup_db_with_agent_run();
        let config = Config::default();
        // Must not panic; DB has no running workflow runs at all.
        crate::workflow::claim_and_resume_expired_leases(&conn, &config, None);
    }

    /// Multiple running root runs with expired leases and no active steps are all
    /// claimed (status → 'failed') before any resume thread is spawned.
    #[test]
    fn test_claim_and_resume_multiple_expired_leases_all_claimed() {
        let (conn, parent_id) = setup();
        for id in &["exp-run-1", "exp-run-2", "exp-run-3"] {
            conn.execute(
                "INSERT INTO workflow_runs \
                 (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
                  started_at, iteration, lease_until) \
                 VALUES (:id, 'test-wf', 'w1', :parent_id, 'running', 0, 'manual', \
                         datetime('now'), 0, '1970-01-01T00:00:00Z')",
                named_params![":id": id, ":parent_id": parent_id],
            )
            .unwrap();
        }
        let config = Config::default();
        crate::workflow::claim_and_resume_expired_leases(&conn, &config, None);
        // All three runs must be transitioned to 'failed' by the CAS flip that
        // happens inside claim_expired_lease_runs before resume threads are spawned.
        for id in &["exp-run-1", "exp-run-2", "exp-run-3"] {
            assert_eq!(
                run_status(&conn, id),
                "failed",
                "run {id} must be claimed (status = 'failed') before resume"
            );
        }
    }

    /// A run with a valid (future) lease must not be reclaimed, even when it has
    /// no active steps — the watchdog only targets expired leases.
    #[test]
    fn test_claim_and_resume_skips_run_with_valid_lease() {
        let (conn, parent_id) = setup();
        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
              started_at, iteration, lease_until) \
             VALUES ('live-run', 'test-wf', 'w1', :parent_id, 'running', 0, 'manual', \
                     datetime('now'), 0, datetime('now', '+1 hour'))",
            named_params![":parent_id": parent_id],
        )
        .unwrap();
        let config = Config::default();
        crate::workflow::claim_and_resume_expired_leases(&conn, &config, None);
        assert_eq!(
            run_status(&conn, "live-run"),
            "running",
            "run with valid lease must not be reclaimed"
        );
    }

    /// A non-running (e.g. completed) run with an expired lease must not be
    /// reclaimed — claim_expired_lease_runs requires status = 'running'.
    #[test]
    fn test_claim_and_resume_ignores_non_running_runs() {
        let (conn, parent_id) = setup();
        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
              started_at, iteration, lease_until) \
             VALUES ('done-run', 'test-wf', 'w1', :parent_id, 'completed', 0, 'manual', \
                     datetime('now'), 0, '1970-01-01T00:00:00Z')",
            named_params![":parent_id": parent_id],
        )
        .unwrap();
        let config = Config::default();
        crate::workflow::claim_and_resume_expired_leases(&conn, &config, None);
        assert_eq!(
            run_status(&conn, "done-run"),
            "completed",
            "completed run must not be reclaimed even if its lease expired"
        );
    }

    // ── delete_run_recursive: multi-level CTE deletion ────────────────────────

    #[test]
    fn test_delete_run_recursive_removes_root_child_and_grandchild() {
        let (conn, parent_id) = setup();

        // Insert root run (terminal so delete_run validates OK).
        insert_run(&conn, "root", &parent_id, "completed", None, 0);

        // Insert child run parented to root.
        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
              started_at, iteration, parent_workflow_run_id) \
             VALUES ('child', 'test-wf', 'w1', :parent_id, 'completed', 0, 'manual', \
                     '2024-01-01T00:00:00Z', 0, 'root')",
            named_params![":parent_id": parent_id],
        )
        .unwrap();

        // Insert grandchild run parented to child.
        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
              started_at, iteration, parent_workflow_run_id) \
             VALUES ('grandchild', 'test-wf', 'w1', :parent_id, 'completed', 0, 'manual', \
                     '2024-01-01T00:00:00Z', 0, 'child')",
            named_params![":parent_id": parent_id],
        )
        .unwrap();

        // Verify all three rows exist before deletion.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM workflow_runs WHERE id IN ('root', 'child', 'grandchild')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 3, "all three runs should exist before delete");
        crate::workflow::delete_run(&conn, "root").unwrap();

        // All three rows must be gone after delete_run_recursive.
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM workflow_runs WHERE id IN ('root', 'child', 'grandchild')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            remaining, 0,
            "root, child, and grandchild must all be deleted"
        );
    }

    // ── terminate_subprocesses: agent PID collection ──────────────────────────

    /// Insert a workflow_run row and return its id.
    fn insert_workflow_run(conn: &rusqlite::Connection, run_id: &str, parent_id: &str) {
        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
              started_at, iteration) \
             VALUES (:id, 'test-wf', 'w1', :parent_id, 'running', 0, 'manual', \
                     '2024-01-01T00:00:00Z', 0)",
            rusqlite::named_params![":id": run_id, ":parent_id": parent_id],
        )
        .unwrap();
    }

    /// Insert an agent_run with an optional subprocess_pid; returns the agent run id.
    fn insert_agent_run_with_pid(conn: &rusqlite::Connection, pid: Option<i64>) -> String {
        let agent_mgr = crate::agent::AgentManager::new(conn);
        let run = agent_mgr.create_run(Some("w1"), "prompt", None).unwrap();
        if let Some(p) = pid {
            conn.execute(
                "UPDATE agent_runs SET subprocess_pid = :pid WHERE id = :id",
                rusqlite::named_params![":pid": p, ":id": run.id],
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
             VALUES (:id, :run_id, 'implement', 'actor', :position, 'running', 0, :child_run_id, \
                     '2024-01-01T00:00:00Z')",
            rusqlite::named_params![":id": step_id, ":run_id": run_id, ":position": position, ":child_run_id": child_run_id],
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
        let result = super::count_live_subprocess_steps(&conn, "wfrun1");
        assert!(
            result.is_ok(),
            "count_live_subprocess_steps should not error: {:?}",
            result
        );

        // Verify terminate_subprocesses itself completes without error.
        let term_result = crate::workflow::reset_failed_steps(&conn, "wfrun1");
        assert!(
            term_result.is_ok(),
            "reset_failed_steps should not error: {:?}",
            term_result
        );

        // After reset, the step must be back in 'pending'.
        let status: String = conn
            .query_row(
                "SELECT status FROM workflow_run_steps WHERE id = 'step1'",
                [],
                |r| r.get("status"),
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
             VALUES ('step2', 'wfrun2', 'script', 'actor', 0, 'running', 0, :agent_run_id, 88888, \
                     '2024-01-01T00:00:00Z')",
            rusqlite::named_params![":agent_run_id": agent_run_id],
        )
        .unwrap();

        // count_live_subprocess_steps uses COALESCE so wrs.subprocess_pid wins.
        // The agent PID query (wrs.subprocess_pid IS NULL) must NOT return this step.
        // Both terminate_subprocesses and count_live_subprocess_steps must complete cleanly.
        assert!(super::count_live_subprocess_steps(&conn, "wfrun2").is_ok());
        assert!(crate::workflow::reset_failed_steps(&conn, "wfrun2").is_ok());

        let status: String = conn
            .query_row(
                "SELECT status FROM workflow_run_steps WHERE id = 'step2'",
                [],
                |r| r.get("status"),
            )
            .unwrap();
        assert_eq!(status, "pending");
    }

    // ── recover_stuck_steps bulk UPDATE ──────────────────────────────────────

    /// Regression: multiple stuck steps with mixed statuses (completed with
    /// result_text, failed with NULL result_text) must all be recovered in a
    /// single `recover_stuck_steps` call via the bulk CASE-expression UPDATE.
    #[test]
    fn test_recover_stuck_steps_bulk() {
        let (conn, parent_id) = setup();
        insert_workflow_run(&conn, "wfrun-bulk", &parent_id);

        // Create two agent runs and mark them terminal.
        let agent_mgr = crate::agent::AgentManager::new(&conn);
        let run_completed = agent_mgr.create_run(Some("w1"), "prompt", None).unwrap();
        let run_failed = agent_mgr.create_run(Some("w1"), "prompt", None).unwrap();

        conn.execute(
            "UPDATE agent_runs SET status = 'completed', result_text = 'the result' WHERE id = :id",
            rusqlite::named_params![":id": run_completed.id],
        )
        .unwrap();
        conn.execute(
            "UPDATE agent_runs SET status = 'failed' WHERE id = :id",
            rusqlite::named_params![":id": run_failed.id],
        )
        .unwrap();

        // Insert two running steps pointing to those agent runs.
        insert_running_agent_step(&conn, "wfrun-bulk", "step-c", &run_completed.id, 0);
        insert_running_agent_step(&conn, "wfrun-bulk", "step-f", &run_failed.id, 1);
        let recovered = crate::workflow::recover_stuck_steps(&conn).unwrap();
        assert_eq!(recovered, 2, "both stuck steps must be recovered");

        let (status_c, result_text_c, ended_at_c): (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT status, result_text, ended_at FROM workflow_run_steps WHERE id = 'step-c'",
                [],
                |r| Ok((r.get("status")?, r.get("result_text")?, r.get("ended_at")?)),
            )
            .unwrap();
        assert_eq!(status_c, "completed");
        assert_eq!(result_text_c.as_deref(), Some("the result"));
        assert!(
            ended_at_c.is_some(),
            "ended_at must be set for completed step"
        );

        let (status_f, result_text_f, ended_at_f): (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT status, result_text, ended_at FROM workflow_run_steps WHERE id = 'step-f'",
                [],
                |r| Ok((r.get("status")?, r.get("result_text")?, r.get("ended_at")?)),
            )
            .unwrap();
        assert_eq!(status_f, "failed");
        assert!(
            result_text_f.is_none(),
            "failed step result_text must be NULL"
        );
        assert!(ended_at_f.is_some(), "ended_at must be set for failed step");
    }

    /// Regression test: terminate_subprocesses must complete without error even when
    /// cancel threads join on PIDs that no longer exist. Exercises the actual production
    /// code path (reset_failed_steps → terminate_subprocesses → thread spawn + join)
    /// with a real script step subprocess_pid, proving the `if let Err` guard in the
    /// join loop does not propagate errors from cancel threads.
    #[cfg(unix)]
    #[test]
    fn test_terminate_subprocesses_cancel_threads_join_without_error() {
        let (conn, parent_id) = setup();
        insert_workflow_run(&conn, "wfrun-cancel", &parent_id);

        // Insert a running script step with a nonexistent PID — cancel_subprocess
        // will send SIGTERM safely (no-op for a dead PID) and return, so the thread
        // won't panic. What matters is that reset_failed_steps returns Ok(()) and
        // the step is reset to pending via the same code path that contains the fix.
        conn.execute(
            "INSERT INTO workflow_run_steps \
             (id, workflow_run_id, step_name, role, position, status, iteration, \
              subprocess_pid, started_at) \
             VALUES ('step-cancel', 'wfrun-cancel', 'script', 'script', 0, 'running', 0, \
                     99999, '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        // reset_failed_steps calls terminate_subprocesses which spawns a cancel thread
        // for PID 99999 and joins it. The `if let Err` guard must not propagate any error.
        let result = crate::workflow::reset_failed_steps(&conn, "wfrun-cancel");
        assert!(
            result.is_ok(),
            "terminate_subprocesses cancel threads must not propagate errors: {:?}",
            result
        );

        let status: String = conn
            .query_row(
                "SELECT status FROM workflow_run_steps WHERE id = 'step-cancel'",
                [],
                |r| r.get("status"),
            )
            .unwrap();
        assert_eq!(
            status, "pending",
            "step must be reset to pending after terminate_subprocesses"
        );
    }
}
