use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json;

use crate::agent::AgentManager;
use crate::config::Config;
use crate::db::query_collect;
use crate::error::{ConductorError, Result};
use crate::workflow_dsl;

use super::constants::{RUN_COLUMNS, STEP_COLUMNS, STEP_COLUMNS_WITH_PREFIX};
use super::status::{WorkflowRunStatus, WorkflowStepStatus};
use super::types::{
    ActiveWorkflowCounts, PendingGateRow, StepKey, WorkflowRun, WorkflowRunContext,
    WorkflowRunStep, WorkflowStepSummary,
};

/// Manages workflow definitions, execution, and persistence.
pub struct WorkflowManager<'a> {
    pub(super) conn: &'a Connection,
}

impl<'a> WorkflowManager<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Returns counts of active workflow runs (pending / running / waiting) per repo_id.
    /// Repos with no active runs are absent from the map. Rows where repo_id IS NULL are skipped.
    pub fn active_run_counts_by_repo(&self) -> Result<HashMap<String, ActiveWorkflowCounts>> {
        let placeholders = sql_placeholders(WorkflowRunStatus::ACTIVE.len());
        let sql = format!(
            "SELECT repo_id, status, COUNT(*) AS cnt \
             FROM workflow_runs \
             WHERE status IN ({placeholders}) \
               AND repo_id IS NOT NULL \
             GROUP BY repo_id, status"
        );
        let active_strings = WorkflowRunStatus::active_strings();
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(active_strings.iter()), |row| {
            let repo_id: String = row.get(0)?;
            let status: String = row.get(1)?;
            let cnt: u32 = row.get(2)?;
            Ok((repo_id, status, cnt))
        })?;
        let mut map: HashMap<String, ActiveWorkflowCounts> = HashMap::new();
        for row in rows {
            let (repo_id, status, cnt) = row?;
            let entry = map.entry(repo_id).or_default();
            match status.as_str() {
                "pending" => entry.pending += cnt,
                "running" => entry.running += cnt,
                "waiting" => entry.waiting += cnt,
                _ => {}
            }
        }
        Ok(map)
    }

    pub fn create_workflow_run(
        &self,
        workflow_name: &str,
        worktree_id: Option<&str>,
        parent_run_id: &str,
        dry_run: bool,
        trigger: &str,
        definition_snapshot: Option<&str>,
    ) -> Result<WorkflowRun> {
        self.create_workflow_run_with_targets(
            workflow_name,
            worktree_id,
            None,
            None,
            parent_run_id,
            dry_run,
            trigger,
            definition_snapshot,
            None,
            None,
        )
    }

    /// Create a workflow run record with ticket and repo target IDs in a single INSERT.
    #[allow(clippy::too_many_arguments)]
    pub fn create_workflow_run_with_targets(
        &self,
        workflow_name: &str,
        worktree_id: Option<&str>,
        ticket_id: Option<&str>,
        repo_id: Option<&str>,
        parent_run_id: &str,
        dry_run: bool,
        trigger: &str,
        definition_snapshot: Option<&str>,
        parent_workflow_run_id: Option<&str>,
        target_label: Option<&str>,
    ) -> Result<WorkflowRun> {
        let id = crate::new_id();
        let now = Utc::now().to_rfc3339();

        self.conn.execute(
            "INSERT INTO workflow_runs (id, workflow_name, worktree_id, ticket_id, repo_id, \
             parent_run_id, status, dry_run, trigger, started_at, definition_snapshot, \
             parent_workflow_run_id, target_label) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                id,
                workflow_name,
                worktree_id,
                ticket_id,
                repo_id,
                parent_run_id,
                "pending",
                dry_run as i64,
                trigger,
                now,
                definition_snapshot,
                parent_workflow_run_id,
                target_label,
            ],
        )?;

        Ok(WorkflowRun {
            id,
            workflow_name: workflow_name.to_string(),
            worktree_id: worktree_id.map(String::from),
            parent_run_id: parent_run_id.to_string(),
            status: WorkflowRunStatus::Pending,
            dry_run,
            trigger: trigger.to_string(),
            started_at: now,
            ended_at: None,
            result_summary: None,
            definition_snapshot: definition_snapshot.map(String::from),
            inputs: HashMap::new(),
            ticket_id: ticket_id.map(String::from),
            repo_id: repo_id.map(String::from),
            parent_workflow_run_id: parent_workflow_run_id.map(String::from),
            target_label: target_label.map(String::from),
            default_bot_name: None,
        })
    }

    /// Persist the default bot name for a workflow run.
    pub fn set_workflow_run_default_bot_name(&self, run_id: &str, bot_name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_runs SET default_bot_name = ?1 WHERE id = ?2",
            params![bot_name, run_id],
        )?;
        Ok(())
    }

    /// Persist the input variables for a workflow run.
    pub fn set_workflow_run_inputs(
        &self,
        run_id: &str,
        inputs: &HashMap<String, String>,
    ) -> Result<()> {
        let inputs_json = serde_json::to_string(inputs).map_err(|e| {
            ConductorError::Workflow(format!("Failed to serialize workflow inputs: {e}"))
        })?;
        self.conn.execute(
            "UPDATE workflow_runs SET inputs = ?1 WHERE id = ?2",
            params![inputs_json, run_id],
        )?;
        Ok(())
    }

    pub fn update_workflow_status(
        &self,
        workflow_run_id: &str,
        status: WorkflowRunStatus,
        result_summary: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let is_terminal = matches!(
            status,
            WorkflowRunStatus::Completed | WorkflowRunStatus::Failed | WorkflowRunStatus::Cancelled
        );
        let ended_at = if is_terminal {
            Some(now.as_str())
        } else {
            None
        };

        self.conn.execute(
            "UPDATE workflow_runs SET status = ?1, result_summary = ?2, ended_at = ?3 WHERE id = ?4",
            params![status, result_summary, ended_at, workflow_run_id],
        )?;
        Ok(())
    }

    /// Cancel a workflow run, best-effort cancelling any in-progress steps and
    /// their child agent runs before marking the run itself as cancelled.
    ///
    /// Returns an error only if the run is not found or is already in a
    /// terminal state (`completed`, `failed`, or `cancelled`).  Step and
    /// child-run cancellation failures are silently ignored (best-effort).
    pub fn cancel_run(&self, run_id: &str, reason: &str) -> Result<()> {
        let run = self
            .get_workflow_run(run_id)?
            .ok_or_else(|| ConductorError::Workflow(format!("Workflow run not found: {run_id}")))?;

        if matches!(
            run.status,
            WorkflowRunStatus::Completed | WorkflowRunStatus::Failed | WorkflowRunStatus::Cancelled
        ) {
            return Err(ConductorError::Workflow(format!(
                "Run {run_id} is already in terminal state: {}",
                run.status
            )));
        }

        let agent_mgr = AgentManager::new(self.conn);
        if let Ok(steps) = self.get_workflow_steps(run_id) {
            for step in steps {
                if matches!(
                    step.status,
                    WorkflowStepStatus::Completed
                        | WorkflowStepStatus::Failed
                        | WorkflowStepStatus::Skipped
                        | WorkflowStepStatus::TimedOut
                ) {
                    continue;
                }
                if let Some(ref child_id) = step.child_run_id {
                    if let Err(e) = agent_mgr.update_run_cancelled(child_id) {
                        tracing::warn!(
                            step_id = %step.id,
                            child_run_id = %child_id,
                            "Failed to mark child agent run as cancelled during workflow cancellation: {e}"
                        );
                    }
                }
                if let Err(e) = self.update_step_status(
                    &step.id,
                    WorkflowStepStatus::Failed,
                    step.child_run_id.as_deref(),
                    Some(reason),
                    None,
                    None,
                    None,
                ) {
                    tracing::warn!(
                        step_id = %step.id,
                        "Failed to update step status to Failed during workflow cancellation: {e}"
                    );
                }
            }
        }

        self.update_workflow_status(run_id, WorkflowRunStatus::Cancelled, Some(reason))
    }

    /// Insert a workflow step record.
    pub fn insert_step(
        &self,
        workflow_run_id: &str,
        step_name: &str,
        role: &str,
        can_commit: bool,
        position: i64,
        iteration: i64,
    ) -> Result<String> {
        let id = crate::new_id();
        self.conn.execute(
            "INSERT INTO workflow_run_steps \
             (id, workflow_run_id, step_name, role, can_commit, status, position, iteration) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                workflow_run_id,
                step_name,
                role,
                can_commit as i64,
                "pending",
                position,
                iteration,
            ],
        )?;
        Ok(id)
    }

    /// Update a step's status and associated fields.
    #[allow(clippy::too_many_arguments)]
    pub fn update_step_status(
        &self,
        step_id: &str,
        status: WorkflowStepStatus,
        child_run_id: Option<&str>,
        result_text: Option<&str>,
        context_out: Option<&str>,
        markers_out: Option<&str>,
        retry_count: Option<i64>,
    ) -> Result<()> {
        self.update_step_status_full(
            step_id,
            status,
            child_run_id,
            result_text,
            context_out,
            markers_out,
            retry_count,
            None,
        )
    }

    /// Update a step's status with all fields including structured_output.
    #[allow(clippy::too_many_arguments)]
    pub fn update_step_status_full(
        &self,
        step_id: &str,
        status: WorkflowStepStatus,
        child_run_id: Option<&str>,
        result_text: Option<&str>,
        context_out: Option<&str>,
        markers_out: Option<&str>,
        retry_count: Option<i64>,
        structured_output: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let is_starting = status == WorkflowStepStatus::Running;
        let is_terminal = matches!(
            status,
            WorkflowStepStatus::Completed
                | WorkflowStepStatus::Failed
                | WorkflowStepStatus::Skipped
                | WorkflowStepStatus::TimedOut
        );

        if is_starting {
            self.conn.execute(
                "UPDATE workflow_run_steps SET status = ?1, child_run_id = ?2, started_at = ?3 \
                 WHERE id = ?4",
                params![status, child_run_id, now, step_id],
            )?;
        } else if is_terminal {
            self.conn.execute(
                "UPDATE workflow_run_steps SET status = ?1, child_run_id = ?2, ended_at = ?3, \
                 result_text = ?4, context_out = ?5, markers_out = ?6, \
                 retry_count = COALESCE(?7, retry_count), structured_output = ?8 \
                 WHERE id = ?9",
                params![
                    status,
                    child_run_id,
                    now,
                    result_text,
                    context_out,
                    markers_out,
                    retry_count,
                    structured_output,
                    step_id,
                ],
            )?;
        } else {
            self.conn.execute(
                "UPDATE workflow_run_steps SET status = ?1 WHERE id = ?2",
                params![status, step_id],
            )?;
        }
        Ok(())
    }

    /// Persist the stdout capture file path for a script step.
    pub fn set_step_output_file(&self, step_id: &str, output_file: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_run_steps SET output_file = ?1 WHERE id = ?2",
            params![output_file, step_id],
        )?;
        Ok(())
    }

    /// Update gate-specific columns on a step.
    pub fn set_step_gate_info(
        &self,
        step_id: &str,
        gate_type: &str,
        gate_prompt: Option<&str>,
        gate_timeout: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_run_steps SET gate_type = ?1, gate_prompt = ?2, gate_timeout = ?3 \
             WHERE id = ?4",
            params![gate_type, gate_prompt, gate_timeout, step_id],
        )?;
        Ok(())
    }

    /// Set parallel_group_id on a step.
    pub fn set_step_parallel_group(&self, step_id: &str, group_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_run_steps SET parallel_group_id = ?1 WHERE id = ?2",
            params![group_id, step_id],
        )?;
        Ok(())
    }

    /// Approve a gate: set gate_approved_at, gate_approved_by, and optional feedback.
    pub fn approve_gate(
        &self,
        step_id: &str,
        approved_by: &str,
        feedback: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE workflow_run_steps SET gate_approved_at = ?1, gate_approved_by = ?2, \
             gate_feedback = ?3, status = 'completed', ended_at = ?1 WHERE id = ?4",
            params![now, approved_by, feedback, step_id],
        )?;
        Ok(())
    }

    /// Reject a gate: set step to failed.
    pub fn reject_gate(
        &self,
        step_id: &str,
        rejected_by: &str,
        feedback: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE workflow_run_steps SET gate_approved_by = ?1, gate_feedback = ?2, status = 'failed', ended_at = ?3 \
             WHERE id = ?4",
            params![rejected_by, feedback, now, step_id],
        )?;
        Ok(())
    }

    pub fn get_workflow_run(&self, id: &str) -> Result<Option<WorkflowRun>> {
        let result = self.conn.query_row(
            &format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE id = ?1"),
            params![id],
            row_to_workflow_run,
        );
        match result {
            Ok(run) => Ok(Some(run)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Resolve the execution context (working directory, repo path, and IDs) for
    /// a workflow that targets a prior workflow run.
    ///
    /// The prior run must have either a `worktree_id` or a `repo_id` set.
    /// Returns an error if the run is not found or its paths no longer exist on disk.
    pub fn resolve_run_context(&self, run_id: &str, config: &Config) -> Result<WorkflowRunContext> {
        let prior_run = self.get_workflow_run(run_id)?.ok_or_else(|| {
            ConductorError::Workflow(format!("workflow run '{run_id}' not found"))
        })?;

        if let Some(ref wt_id) = prior_run.worktree_id {
            let wt_mgr = crate::worktree::WorktreeManager::new(self.conn, config);
            let wt = wt_mgr.get_by_id(wt_id)?;
            if !std::path::Path::new(&wt.path).exists() {
                return Err(ConductorError::Workflow(format!(
                    "worktree path '{}' no longer exists on disk",
                    wt.path
                )));
            }
            let repo = crate::repo::RepoManager::new(self.conn, config).get_by_id(&wt.repo_id)?;
            Ok(WorkflowRunContext {
                working_dir: wt.path,
                repo_path: repo.local_path,
                worktree_id: Some(wt_id.clone()),
                repo_id: Some(wt.repo_id),
            })
        } else if let Some(ref repo_id) = prior_run.repo_id {
            let repo = crate::repo::RepoManager::new(self.conn, config).get_by_id(repo_id)?;
            Ok(WorkflowRunContext {
                working_dir: repo.local_path.clone(),
                repo_path: repo.local_path,
                worktree_id: None,
                repo_id: Some(repo_id.clone()),
            })
        } else {
            Err(ConductorError::Workflow(format!(
                "workflow run '{run_id}' has no associated worktree or repo"
            )))
        }
    }

    pub fn get_workflow_steps(&self, workflow_run_id: &str) -> Result<Vec<WorkflowRunStep>> {
        query_collect(
            self.conn,
            &format!("SELECT {STEP_COLUMNS} FROM workflow_run_steps WHERE workflow_run_id = ?1 ORDER BY position"),
            params![workflow_run_id],
            row_to_workflow_step,
        )
    }

    /// Batch-fetch steps for multiple runs in a single query.
    /// Returns a map of run_id → steps (sorted by position).
    pub fn get_steps_for_runs(
        &self,
        run_ids: &[&str],
    ) -> Result<HashMap<String, Vec<WorkflowRunStep>>> {
        self.fetch_steps_for_runs_filtered(run_ids, None)
    }

    /// Batch-fetch only running/waiting steps for multiple runs in a single query.
    /// Returns a map of run_id → active steps (sorted by position).
    pub fn get_active_steps_for_runs(
        &self,
        run_ids: &[&str],
    ) -> Result<HashMap<String, Vec<WorkflowRunStep>>> {
        self.fetch_steps_for_runs_filtered(run_ids, Some(&["running", "waiting"]))
    }

    fn fetch_steps_for_runs_filtered(
        &self,
        run_ids: &[&str],
        status_filter: Option<&[&str]>,
    ) -> Result<HashMap<String, Vec<WorkflowRunStep>>> {
        if run_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = sql_placeholders(run_ids.len());
        // Status placeholders must be offset so their ?N indices don't collide
        // with the run_id placeholders (which use ?1..?run_ids.len()).
        let status_clause = if let Some(statuses) = status_filter {
            let offset = run_ids.len();
            let status_placeholders = (1..=statuses.len())
                .map(|i| format!("?{}", offset + i))
                .collect::<Vec<_>>()
                .join(", ");
            format!(" AND status IN ({status_placeholders})")
        } else {
            String::new()
        };
        let sql = format!(
            "SELECT {STEP_COLUMNS} FROM workflow_run_steps \
             WHERE workflow_run_id IN ({placeholders}){status_clause} \
             ORDER BY workflow_run_id, position"
        );
        let combined = run_ids
            .iter()
            .copied()
            .chain(status_filter.unwrap_or_default().iter().copied());
        let mut stmt = self.conn.prepare(&sql)?;
        let steps = stmt
            .query_map(rusqlite::params_from_iter(combined), row_to_workflow_step)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut map: HashMap<String, Vec<WorkflowRunStep>> = HashMap::new();
        for step in steps {
            map.entry(step.workflow_run_id.clone())
                .or_default()
                .push(step);
        }
        Ok(map)
    }

    pub fn get_step_by_id(&self, step_id: &str) -> Result<Option<WorkflowRunStep>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {STEP_COLUMNS} FROM workflow_run_steps WHERE id = ?1"
        ))?;
        let mut rows = stmt.query_map(params![step_id], row_to_workflow_step)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Return the first active (pending/running/waiting) top-level workflow run for a worktree,
    /// or `None` if none exist.
    pub fn get_active_run_for_worktree(&self, worktree_id: &str) -> Result<Option<WorkflowRun>> {
        let placeholders = sql_placeholders_from(WorkflowRunStatus::ACTIVE.len(), 2);
        let active_strings = WorkflowRunStatus::active_strings();
        let sql = format!(
            "SELECT {RUN_COLUMNS} FROM workflow_runs \
             WHERE worktree_id = ?1 AND status IN ({placeholders}) \
             LIMIT 1"
        );
        let mut all_params: Vec<rusqlite::types::Value> =
            vec![rusqlite::types::Value::Text(worktree_id.to_owned())];
        all_params.extend(active_strings.into_iter().map(rusqlite::types::Value::Text));
        let result = self.conn.query_row(
            &sql,
            rusqlite::params_from_iter(all_params.iter()),
            row_to_workflow_run,
        );
        match result {
            Ok(run) => Ok(Some(run)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn list_workflow_runs(&self, worktree_id: &str) -> Result<Vec<WorkflowRun>> {
        query_collect(
            self.conn,
            &format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE worktree_id = ?1 ORDER BY started_at DESC"),
            params![worktree_id],
            row_to_workflow_run,
        )
    }

    pub fn list_workflow_runs_filtered(
        &self,
        worktree_id: &str,
        status: Option<WorkflowRunStatus>,
    ) -> Result<Vec<WorkflowRun>> {
        if let Some(s) = status {
            let status_str = s.to_string();
            query_collect(
                self.conn,
                &format!(
                    "SELECT {RUN_COLUMNS} FROM workflow_runs \
                     WHERE worktree_id = ?1 AND status = ?2 \
                     ORDER BY started_at DESC"
                ),
                params![worktree_id, status_str],
                row_to_workflow_run,
            )
        } else {
            self.list_workflow_runs(worktree_id)
        }
    }

    pub fn list_workflow_runs_by_repo_id_filtered(
        &self,
        repo_id: &str,
        limit: usize,
        offset: usize,
        status: Option<WorkflowRunStatus>,
    ) -> Result<Vec<WorkflowRun>> {
        if let Some(s) = status {
            let status_str = s.to_string();
            query_collect(
                self.conn,
                &format!(
                    "SELECT workflow_runs.* \
                     FROM workflow_runs \
                     LEFT JOIN worktrees ON worktrees.id = workflow_runs.worktree_id \
                     WHERE workflow_runs.repo_id = ?1 \
                       AND (workflow_runs.worktree_id IS NULL OR worktrees.status = 'active') \
                       AND workflow_runs.status = ?2 \
                     ORDER BY workflow_runs.started_at DESC LIMIT {limit} OFFSET {offset}"
                ),
                params![repo_id, status_str],
                row_to_workflow_run,
            )
        } else {
            self.list_workflow_runs_by_repo_id(repo_id, limit, offset)
        }
    }

    /// Like `list_workflow_runs_filtered` but with explicit limit and offset for pagination.
    pub fn list_workflow_runs_filtered_paginated(
        &self,
        worktree_id: &str,
        status: Option<WorkflowRunStatus>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<WorkflowRun>> {
        if let Some(s) = status {
            let status_str = s.to_string();
            query_collect(
                self.conn,
                &format!(
                    "SELECT {RUN_COLUMNS} FROM workflow_runs \
                     WHERE worktree_id = ?1 AND status = ?2 \
                     ORDER BY started_at DESC LIMIT {limit} OFFSET {offset}"
                ),
                params![worktree_id, status_str],
                row_to_workflow_run,
            )
        } else {
            self.list_workflow_runs_paginated(worktree_id, limit, offset)
        }
    }

    /// List recent workflow runs across all worktrees, ordered by started_at DESC.
    /// Only includes runs whose associated worktree is `active` (or runs with no
    /// worktree, i.e. ephemeral/repo-targeted runs).
    pub fn list_all_workflow_runs(&self, limit: usize) -> Result<Vec<WorkflowRun>> {
        query_collect(
            self.conn,
            &format!(
                "SELECT workflow_runs.* \
                 FROM workflow_runs \
                 LEFT JOIN worktrees ON worktrees.id = workflow_runs.worktree_id \
                 WHERE workflow_runs.worktree_id IS NULL OR worktrees.status = 'active' \
                 ORDER BY workflow_runs.started_at DESC LIMIT {limit}"
            ),
            params![],
            row_to_workflow_run,
        )
    }

    /// List workflow runs across all worktrees filtered by a set of statuses.
    /// When `statuses` is empty, defaults to `[running, waiting, pending]`.
    /// Only includes runs whose associated worktree is `active` (or runs with no worktree).
    /// Ordered by `started_at DESC`.
    pub fn list_active_workflow_runs(
        &self,
        statuses: &[WorkflowRunStatus],
    ) -> Result<Vec<WorkflowRun>> {
        let effective: &[WorkflowRunStatus] = if statuses.is_empty() {
            &WorkflowRunStatus::ACTIVE
        } else {
            statuses
        };

        let placeholders = sql_placeholders(effective.len());

        let sql = format!(
            "SELECT workflow_runs.* \
             FROM workflow_runs \
             LEFT JOIN worktrees ON worktrees.id = workflow_runs.worktree_id \
             WHERE (workflow_runs.worktree_id IS NULL OR worktrees.status = 'active') \
               AND workflow_runs.status IN ({placeholders}) \
             ORDER BY workflow_runs.started_at DESC"
        );

        let status_strings: Vec<String> = effective.iter().map(|s| s.to_string()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(status_strings.iter()),
            row_to_workflow_run,
        )?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Like `list_all_workflow_runs` but with an optional status filter and pagination offset.
    /// Covers all repos; the active-worktree guard is identical to `list_all_workflow_runs`.
    pub fn list_all_workflow_runs_filtered_paginated(
        &self,
        status: Option<WorkflowRunStatus>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<WorkflowRun>> {
        if let Some(s) = status {
            let status_str = s.to_string();
            query_collect(
                self.conn,
                &format!(
                    "SELECT workflow_runs.* \
                     FROM workflow_runs \
                     LEFT JOIN worktrees ON worktrees.id = workflow_runs.worktree_id \
                     WHERE (workflow_runs.worktree_id IS NULL OR worktrees.status = 'active') \
                       AND workflow_runs.status = ?1 \
                     ORDER BY workflow_runs.started_at DESC LIMIT {limit} OFFSET {offset}"
                ),
                params![status_str],
                row_to_workflow_run,
            )
        } else {
            query_collect(
                self.conn,
                &format!(
                    "SELECT workflow_runs.* \
                     FROM workflow_runs \
                     LEFT JOIN worktrees ON worktrees.id = workflow_runs.worktree_id \
                     WHERE workflow_runs.worktree_id IS NULL OR worktrees.status = 'active' \
                     ORDER BY workflow_runs.started_at DESC LIMIT {limit} OFFSET {offset}"
                ),
                params![],
                row_to_workflow_run,
            )
        }
    }

    /// List recent workflow runs for a specific repo, ordered by started_at DESC.
    /// Unlike `list_all_workflow_runs` + filter, this queries directly by `repo_id`
    /// so older per-repo runs beyond a global cap are never silently omitted.
    /// Only includes runs whose associated worktree is `active` (or runs with no worktree).
    pub fn list_workflow_runs_by_repo_id(
        &self,
        repo_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<WorkflowRun>> {
        query_collect(
            self.conn,
            &format!(
                "SELECT workflow_runs.* \
                 FROM workflow_runs \
                 LEFT JOIN worktrees ON worktrees.id = workflow_runs.worktree_id \
                 WHERE workflow_runs.repo_id = ?1 \
                   AND (workflow_runs.worktree_id IS NULL OR worktrees.status = 'active') \
                 ORDER BY workflow_runs.started_at DESC LIMIT {limit} OFFSET {offset}"
            ),
            params![repo_id],
            row_to_workflow_run,
        )
    }

    /// Like `list_workflow_runs` but with explicit limit and offset for pagination.
    /// `list_workflow_runs` is kept for TUI callers that return all runs.
    pub fn list_workflow_runs_paginated(
        &self,
        worktree_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<WorkflowRun>> {
        query_collect(
            self.conn,
            &format!(
                "SELECT {RUN_COLUMNS} FROM workflow_runs \
                 WHERE worktree_id = ?1 \
                 ORDER BY started_at DESC LIMIT {limit} OFFSET {offset}"
            ),
            params![worktree_id],
            row_to_workflow_run,
        )
    }

    /// List recent root workflow runs (those with no parent workflow run) across all
    /// worktrees, ordered by started_at DESC.  Used in the TUI per-worktree slot so that
    /// the root run wins over any concurrently-active child run.
    pub fn list_root_workflow_runs(&self, limit: usize) -> Result<Vec<WorkflowRun>> {
        query_collect(
            self.conn,
            &format!(
                "SELECT {RUN_COLUMNS} FROM workflow_runs \
                 WHERE parent_workflow_run_id IS NULL \
                 ORDER BY started_at DESC LIMIT {limit}"
            ),
            params![],
            row_to_workflow_run,
        )
    }

    /// List active (running or waiting) root workflow runs that have no associated
    /// worktree (`worktree_id IS NULL`).  These are repo- or ticket-targeted
    /// workflows (e.g. `label-all-tickets`) that the TUI status bar would otherwise
    /// never show.
    pub fn list_active_non_worktree_workflow_runs(&self, limit: i64) -> Result<Vec<WorkflowRun>> {
        query_collect(
            self.conn,
            &format!(
                "SELECT {RUN_COLUMNS} FROM workflow_runs \
                 WHERE parent_workflow_run_id IS NULL \
                   AND worktree_id IS NULL \
                   AND status IN ('running', 'waiting') \
                 ORDER BY started_at DESC LIMIT ?1"
            ),
            params![limit],
            row_to_workflow_run,
        )
    }

    /// Walk the active child chain starting from `root_run_id` and return the
    /// ordered list of `(id, workflow_name)` pairs below the root (the root is
    /// excluded — the caller already has it).
    ///
    /// Iterates at most `MAX_WORKFLOW_DEPTH` times to match the execution depth cap.
    pub fn get_active_chain_for_run(&self, root_run_id: &str) -> Result<Vec<(String, String)>> {
        const MAX_DEPTH: usize = 5;
        let mut chain: Vec<(String, String)> = Vec::new();
        let mut current_id = root_run_id.to_string();
        for _ in 0..MAX_DEPTH {
            let mut stmt = self.conn.prepare_cached(
                "SELECT id, workflow_name FROM workflow_runs \
                 WHERE parent_workflow_run_id = ?1 \
                   AND status IN ('running', 'waiting') \
                 LIMIT 1",
            )?;
            let result: Option<(String, String)> = stmt
                .query_row(params![current_id], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()?;
            match result {
                Some((child_id, child_name)) => {
                    chain.push((child_id.clone(), child_name));
                    current_id = child_id;
                }
                None => break,
            }
        }
        Ok(chain)
    }

    /// Load runs for a single worktree, or the most recent `global_limit` runs across all
    /// worktrees when `worktree_id` is `None`. Consolidates the scoped-vs-global branching
    /// that would otherwise be duplicated at every call site.
    pub fn list_workflow_runs_for_scope(
        &self,
        worktree_id: Option<&str>,
        global_limit: usize,
    ) -> Result<Vec<WorkflowRun>> {
        match worktree_id {
            Some(wt_id) => self.list_workflow_runs(wt_id),
            None => self.list_all_workflow_runs(global_limit),
        }
    }

    /// List recent workflow runs for a specific repo, ordered by started_at DESC.
    /// Includes runs from all worktrees belonging to the repo, plus any repo-targeted runs.
    /// Matches via `workflow_runs.repo_id` OR via the worktree join, since worktree-targeted
    /// runs may have `repo_id = NULL` and are only linked to the repo through `worktrees.repo_id`.
    pub fn list_workflow_runs_for_repo(
        &self,
        repo_id: &str,
        limit: usize,
    ) -> Result<Vec<WorkflowRun>> {
        query_collect(
            self.conn,
            &format!(
                "SELECT DISTINCT workflow_runs.* \
                 FROM workflow_runs \
                 LEFT JOIN worktrees ON worktrees.id = workflow_runs.worktree_id \
                 WHERE workflow_runs.repo_id = ?1 OR worktrees.repo_id = ?1 \
                 ORDER BY workflow_runs.started_at DESC LIMIT {limit}"
            ),
            params![repo_id],
            row_to_workflow_run,
        )
    }

    /// Batch-lookup the parent `workflow_run_id` for a set of agent run IDs.
    ///
    /// Uses `workflow_run_steps.child_run_id` to find the link.  Returns a map
    /// of `agent_run_id → workflow_run_id`.  Agent runs that are not linked to
    /// any workflow step are simply absent from the map.
    ///
    /// Avoids N+1 queries — one SQL round-trip regardless of slice size.
    pub fn get_workflow_run_ids_for_agent_runs(
        &self,
        agent_run_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, String>> {
        if agent_run_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let placeholders = sql_placeholders(agent_run_ids.len());
        let sql = format!(
            "SELECT child_run_id, workflow_run_id \
             FROM workflow_run_steps \
             WHERE child_run_id IN ({placeholders}) \
             GROUP BY child_run_id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut map = std::collections::HashMap::new();
        let params_iter = rusqlite::params_from_iter(agent_run_ids.iter());
        let rows = stmt.query_map(params_iter, |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (child_run_id, workflow_run_id) = row?;
            map.insert(child_run_id, workflow_run_id);
        }
        Ok(map)
    }

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

    /// Find the waiting gate step for a workflow run.
    pub fn find_waiting_gate(&self, workflow_run_id: &str) -> Result<Option<WorkflowRunStep>> {
        let result = self.conn.query_row(
            &format!(
                "SELECT {STEP_COLUMNS} FROM workflow_run_steps \
                 WHERE workflow_run_id = ?1 AND gate_type IS NOT NULL AND gate_approved_at IS NULL \
                   AND status IN ('running', 'waiting') \
                 ORDER BY position DESC LIMIT 1"
            ),
            params![workflow_run_id],
            row_to_workflow_step,
        );
        match result {
            Ok(step) => Ok(Some(step)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// List all gate steps currently in `waiting` status across all workflow runs.
    ///
    /// Returns `(step, workflow_name, target_label)` tuples. Used by the TUI background poller to
    /// fire cross-process gate-waiting notifications.
    pub fn list_all_waiting_gate_steps(
        &self,
    ) -> Result<Vec<(WorkflowRunStep, String, Option<String>)>> {
        self.list_waiting_gate_steps_scoped(None)
    }

    /// List gate steps currently in `waiting` status for a specific repo.
    ///
    /// Returns enriched [`PendingGateRow`] values that include the worktree branch and linked
    /// ticket source_id so the TUI can display context without additional queries.
    pub fn list_waiting_gate_steps_for_repo(&self, repo_id: &str) -> Result<Vec<PendingGateRow>> {
        let sql = format!(
            "SELECT {cols}, r.workflow_name, r.target_label, wt.branch, t.source_id AS ticket_ref \
             FROM workflow_run_steps s \
             JOIN workflow_runs r ON r.id = s.workflow_run_id \
             LEFT JOIN worktrees wt ON wt.id = r.worktree_id \
             LEFT JOIN tickets t ON t.id = r.ticket_id \
             WHERE s.gate_type IS NOT NULL AND s.status = 'waiting' \
             AND r.status IN ('pending', 'running', 'waiting') \
             AND (r.repo_id = ?1 OR wt.repo_id = ?1) \
             ORDER BY s.started_at",
            cols = &*STEP_COLUMNS_WITH_PREFIX,
        );
        crate::db::query_collect(self.conn, &sql, [repo_id], pending_gate_row_mapper)
    }

    /// Shared implementation for listing waiting gate steps, optionally scoped to a repo.
    ///
    /// When `repo_id` is `Some`, the query adds a `LEFT JOIN worktrees` and filters to runs whose
    /// `repo_id` matches directly or via their linked worktree.
    fn list_waiting_gate_steps_scoped(
        &self,
        repo_id: Option<&str>,
    ) -> Result<Vec<(WorkflowRunStep, String, Option<String>)>> {
        let (extra_join, extra_where) = match repo_id {
            Some(_) => (
                " LEFT JOIN worktrees wt ON wt.id = r.worktree_id",
                " AND (r.repo_id = ?1 OR wt.repo_id = ?1)",
            ),
            None => ("", ""),
        };
        let active_strings = WorkflowRunStatus::active_strings();
        let active_placeholders = match repo_id {
            Some(_) => sql_placeholders_from(WorkflowRunStatus::ACTIVE.len(), 2),
            None => sql_placeholders(WorkflowRunStatus::ACTIVE.len()),
        };
        let sql = format!(
            "SELECT {cols}, r.workflow_name, r.target_label \
             FROM workflow_run_steps s \
             JOIN workflow_runs r ON r.id = s.workflow_run_id{ej} \
             WHERE s.gate_type IS NOT NULL AND s.status = 'waiting' \
             AND r.status IN ({ai}){ew} \
             ORDER BY s.started_at",
            cols = &*STEP_COLUMNS_WITH_PREFIX,
            ej = extra_join,
            ai = active_placeholders,
            ew = extra_where,
        );
        match repo_id {
            Some(id) => {
                let mut all_params: Vec<rusqlite::types::Value> =
                    vec![rusqlite::types::Value::Text(id.to_owned())];
                all_params.extend(active_strings.into_iter().map(rusqlite::types::Value::Text));
                crate::db::query_collect(
                    self.conn,
                    &sql,
                    rusqlite::params_from_iter(all_params.iter()),
                    waiting_gate_step_row_mapper,
                )
            }
            None => crate::db::query_collect(
                self.conn,
                &sql,
                rusqlite::params_from_iter(active_strings.iter()),
                waiting_gate_step_row_mapper,
            ),
        }
    }

    /// Load workflow definitions from the filesystem for a worktree.
    ///
    /// Wraps `workflow_dsl::load_workflow_defs` so consumers don't need to
    /// reach into the low-level DSL module directly.
    ///
    /// Returns `(defs, warnings)` — warnings contain one [`WorkflowWarning`]
    /// per `.wf` file that failed to parse. Successfully-parsed definitions are
    /// always returned even when some files are broken.
    pub fn list_defs(
        worktree_path: &str,
        repo_path: &str,
    ) -> Result<(
        Vec<crate::workflow_dsl::WorkflowDef>,
        Vec<crate::workflow_dsl::WorkflowWarning>,
    )> {
        workflow_dsl::load_workflow_defs(worktree_path, repo_path)
    }

    /// Load a single workflow definition by name.
    pub fn load_def_by_name(
        worktree_path: &str,
        repo_path: &str,
        name: &str,
    ) -> Result<crate::workflow_dsl::WorkflowDef> {
        workflow_dsl::load_workflow_by_name(worktree_path, repo_path, name)
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
        Ok(super::engine::completed_keys_from_steps(&steps))
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

    /// Fetch the currently-running step for each of the given (root) workflow run IDs.
    /// Returns a map from root `workflow_run_id` to a `WorkflowStepSummary`.
    ///
    /// For each root run the method walks down the active child chain to find the
    /// deepest active sub-workflow run (the *leaf*), queries its running step, and
    /// populates `workflow_chain` with the ordered workflow names from the root down
    /// to (but not including) the leaf's own name — which is already available via
    /// the `workflow_name` field of the root's `WorkflowRun`.
    ///
    /// An empty `run_ids` slice returns an empty map without hitting the DB.
    pub fn get_step_summaries_for_runs(
        &self,
        run_ids: &[&str],
    ) -> Result<HashMap<String, WorkflowStepSummary>> {
        if run_ids.is_empty() {
            return Ok(HashMap::new());
        }

        // Fetch workflow names for the root runs.
        let placeholders = sql_placeholders(run_ids.len());
        let name_sql =
            format!("SELECT id, workflow_name FROM workflow_runs WHERE id IN ({placeholders})");
        let name_params: Vec<&dyn rusqlite::ToSql> =
            run_ids.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let mut name_stmt = self.conn.prepare(&name_sql)?;
        let mut name_rows = name_stmt.query(name_params.as_slice())?;
        let mut root_names: HashMap<String, String> = HashMap::new();
        while let Some(row) = name_rows.next()? {
            let id: String = row.get(0)?;
            let name: String = row.get(1)?;
            root_names.insert(id, name);
        }

        let mut map: HashMap<String, WorkflowStepSummary> = HashMap::new();

        for root_id in run_ids {
            let Some(root_name) = root_names.get(*root_id) else {
                continue;
            };

            // Walk the active sub-workflow chain from this root.
            // Returns (id, name) pairs so we already have the leaf run ID.
            let child_chain = self.get_active_chain_for_run(root_id)?;

            // The leaf run is the deepest active child, or the root itself.
            let leaf_id = child_chain
                .last()
                .map(|(id, _)| id.clone())
                .unwrap_or_else(|| root_id.to_string());

            // Find the running step on the leaf run.
            let mut step_stmt = self.conn.prepare_cached(
                "SELECT step_name, iteration FROM workflow_run_steps \
                 WHERE workflow_run_id = ?1 AND status = 'running' \
                 ORDER BY position ASC LIMIT 1",
            )?;
            let step: Option<(String, i64)> = step_stmt
                .query_row(params![leaf_id], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()?;

            if let Some((step_name, iteration)) = step {
                // For single-level (no children), expose an empty vec to keep
                // existing rendering unchanged. Otherwise build:
                // root_name + child names excluding the leaf (which owns the step).
                let workflow_chain = if child_chain.is_empty() {
                    Vec::new()
                } else {
                    let mut wc = vec![root_name.clone()];
                    // child_chain is (id, name); exclude the last entry (the leaf).
                    wc.extend(
                        child_chain[..child_chain.len() - 1]
                            .iter()
                            .map(|(_, name)| name.clone()),
                    );
                    wc
                };

                map.insert(
                    root_id.to_string(),
                    WorkflowStepSummary {
                        step_name,
                        iteration,
                        workflow_chain,
                    },
                );
            }
        }
        Ok(map)
    }
}

/// Build the WHERE clause and owned parameter list for purge / purge_count queries.
///
/// Returns a comma-separated list of numbered SQLite positional placeholders:
/// `?1, ?2, …, ?n`.  Returns an empty string when `n == 0`.
fn sql_placeholders(n: usize) -> String {
    sql_placeholders_from(n, 1)
}

/// `?start, ?{start+1}, …, ?{start+n-1}`.  Returns an empty string when `n == 0`.
fn sql_placeholders_from(n: usize, start: usize) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(n.saturating_mul(4));
    for i in start..start + n {
        if i > start {
            s.push_str(", ");
        }
        write!(s, "?{i}").unwrap();
    }
    s
}

/// Returns `(where_clause, params)` where `params` is a `Vec<String>` whose
/// elements bind to the positional placeholders in the clause.
fn purge_where_clause(statuses: &[&str], repo_id: Option<&str>) -> (String, Vec<String>) {
    let n = statuses.len();
    let placeholders = sql_placeholders(n);
    let where_clause = if repo_id.is_some() {
        format!(
            "status IN ({placeholders}) AND worktree_id IN \
             (SELECT id FROM worktrees WHERE repo_id = ?{})",
            n + 1
        )
    } else {
        format!("status IN ({placeholders})")
    };
    let mut params: Vec<String> = statuses.iter().map(|s| s.to_string()).collect();
    if let Some(rid) = repo_id {
        params.push(rid.to_string());
    }
    (where_clause, params)
}

pub(super) fn row_to_workflow_run(row: &rusqlite::Row) -> rusqlite::Result<WorkflowRun> {
    let dry_run_int: i64 = row.get(5)?;
    let inputs_json: Option<String> = row.get(11)?;
    let inputs: std::collections::HashMap<String, String> = inputs_json
        .as_deref()
        .map(|s| {
            serde_json::from_str(s).unwrap_or_else(|e| {
                tracing::warn!("Malformed inputs JSON in workflow run: {e}");
                std::collections::HashMap::new()
            })
        })
        .unwrap_or_default();
    let ticket_id: Option<String> = row.get(12)?;
    let repo_id: Option<String> = row.get(13)?;
    let parent_workflow_run_id: Option<String> = row.get(14)?;
    let target_label: Option<String> = row.get(15)?;
    let default_bot_name: Option<String> = row.get(16)?;
    Ok(WorkflowRun {
        id: row.get(0)?,
        workflow_name: row.get(1)?,
        worktree_id: row.get::<_, Option<String>>(2)?,
        parent_run_id: row.get(3)?,
        status: row.get(4)?,
        dry_run: dry_run_int != 0,
        trigger: row.get(6)?,
        started_at: row.get(7)?,
        ended_at: row.get(8)?,
        result_summary: row.get(9)?,
        definition_snapshot: row.get(10)?,
        inputs,
        ticket_id,
        repo_id,
        parent_workflow_run_id,
        target_label,
        default_bot_name,
    })
}

fn waiting_gate_step_row_mapper(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(WorkflowRunStep, String, Option<String>)> {
    let step = row_to_workflow_step(row)?;
    let workflow_name: String = row.get("workflow_name")?;
    let target_label: Option<String> = row.get("target_label")?;
    Ok((step, workflow_name, target_label))
}

fn pending_gate_row_mapper(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingGateRow> {
    let step = row_to_workflow_step(row)?;
    let workflow_name: String = row.get("workflow_name")?;
    let target_label: Option<String> = row.get("target_label")?;
    let branch: Option<String> = row.get("branch")?;
    let ticket_ref: Option<String> = row.get("ticket_ref")?;
    Ok(PendingGateRow {
        step,
        workflow_name,
        target_label,
        branch,
        ticket_ref,
    })
}

pub(super) fn row_to_workflow_step(row: &rusqlite::Row) -> rusqlite::Result<WorkflowRunStep> {
    let can_commit_int: i64 = row.get("can_commit")?;
    let condition_met_int: Option<i64> = row.get("condition_met")?;
    Ok(WorkflowRunStep {
        id: row.get("id")?,
        workflow_run_id: row.get("workflow_run_id")?,
        step_name: row.get("step_name")?,
        role: row.get("role")?,
        can_commit: can_commit_int != 0,
        condition_expr: row.get("condition_expr")?,
        status: row.get("status")?,
        child_run_id: row.get("child_run_id")?,
        position: row.get("position")?,
        started_at: row.get("started_at")?,
        ended_at: row.get("ended_at")?,
        result_text: row.get("result_text")?,
        condition_met: condition_met_int.map(|v| v != 0),
        iteration: row.get("iteration")?,
        parallel_group_id: row.get("parallel_group_id")?,
        context_out: row.get("context_out")?,
        markers_out: row.get("markers_out")?,
        retry_count: row.get("retry_count")?,
        gate_type: row.get("gate_type")?,
        gate_prompt: row.get("gate_prompt")?,
        gate_timeout: row.get("gate_timeout")?,
        gate_approved_by: row.get("gate_approved_by")?,
        gate_approved_at: row.get("gate_approved_at")?,
        gate_feedback: row.get("gate_feedback")?,
        structured_output: row.get("structured_output")?,
        output_file: row.get("output_file")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentManager;

    fn setup_db() -> rusqlite::Connection {
        let conn = crate::test_helpers::setup_db();
        // Add a second repo and worktrees for cross-repo filtering tests
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, default_branch, workspace_dir, created_at) \
             VALUES ('r2', 'other-repo', '/tmp/repo2', 'https://github.com/test/repo2.git', 'main', '/tmp/ws2', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w2', 'r1', 'fix-bug', 'fix/bug', '/tmp/ws/fix-bug', 'active', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w3', 'r2', 'other-feat', 'feat/other', '/tmp/ws2/other-feat', 'active', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn
    }

    fn make_parent_id(conn: &rusqlite::Connection, wt_id: &str) -> String {
        AgentManager::new(conn)
            .create_run(Some(wt_id), "workflow", None, None)
            .unwrap()
            .id
    }

    // Helper to create a run linked to a worktree (worktree_id set, repo_id null — simulates
    // the common case where runs are launched from a worktree context).
    fn create_worktree_run(conn: &rusqlite::Connection, wt_id: &str) -> WorkflowRun {
        let parent_id = make_parent_id(conn, wt_id);
        WorkflowManager::new(conn)
            .create_workflow_run("wf", Some(wt_id), &parent_id, false, "manual", None)
            .unwrap()
    }

    // Helper to set a step's status without touching optional fields.
    fn set_step_status(mgr: &WorkflowManager, step_id: &str, status: WorkflowStepStatus) {
        mgr.update_step_status(step_id, status, None, None, None, None, None)
            .unwrap();
    }

    // Helper to create a run linked directly to a repo (repo_id set, worktree_id null).
    fn create_repo_run(conn: &rusqlite::Connection, repo_id: &str) -> WorkflowRun {
        // Need a valid parent agent run; use w1 as the worktree for the agent run.
        let parent_id = make_parent_id(conn, "w1");
        WorkflowManager::new(conn)
            .create_workflow_run_with_targets(
                "wf",
                None,
                None,
                Some(repo_id),
                &parent_id,
                false,
                "manual",
                None,
                None,
                None,
            )
            .unwrap()
    }

    #[test]
    fn test_list_workflow_runs_for_repo_includes_worktree_runs() {
        // Runs linked to a worktree (repo_id NULL) should appear when querying by repo.
        let conn = setup_db();
        let run = create_worktree_run(&conn, "w1");
        let runs = WorkflowManager::new(&conn)
            .list_workflow_runs_for_repo("r1", 50)
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, run.id);
    }

    #[test]
    fn test_list_workflow_runs_for_repo_includes_repo_targeted_runs() {
        // Runs with repo_id set directly should also appear.
        let conn = setup_db();
        let run = create_repo_run(&conn, "r1");
        let runs = WorkflowManager::new(&conn)
            .list_workflow_runs_for_repo("r1", 50)
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, run.id);
    }

    #[test]
    fn test_list_workflow_runs_for_repo_distinct_no_duplicates() {
        // A run that matches both paths (repo_id = r1 AND worktree belongs to r1) should
        // appear exactly once thanks to SELECT DISTINCT.
        let conn = setup_db();
        let parent_id = make_parent_id(&conn, "w1");
        WorkflowManager::new(&conn)
            .create_workflow_run_with_targets(
                "wf",
                Some("w1"),
                None,
                Some("r1"),
                &parent_id,
                false,
                "manual",
                None,
                None,
                None,
            )
            .unwrap();
        let runs = WorkflowManager::new(&conn)
            .list_workflow_runs_for_repo("r1", 50)
            .unwrap();
        assert_eq!(
            runs.len(),
            1,
            "run matching both join paths must appear only once"
        );
    }

    #[test]
    fn test_list_workflow_runs_for_repo_cross_repo_filtering() {
        // Runs belonging to r2 must not appear when querying r1, and vice versa.
        let conn = setup_db();
        create_worktree_run(&conn, "w1"); // r1
        create_worktree_run(&conn, "w3"); // r2 via worktree
        create_repo_run(&conn, "r2"); // r2 directly

        let r1_runs = WorkflowManager::new(&conn)
            .list_workflow_runs_for_repo("r1", 50)
            .unwrap();
        assert_eq!(r1_runs.len(), 1);
        let r2_runs = WorkflowManager::new(&conn)
            .list_workflow_runs_for_repo("r2", 50)
            .unwrap();
        assert_eq!(r2_runs.len(), 2);
    }

    #[test]
    fn test_list_workflow_runs_for_repo_limit() {
        // Only `limit` most recent runs should be returned.
        let conn = setup_db();
        for _ in 0..5 {
            create_worktree_run(&conn, "w1");
        }
        let runs = WorkflowManager::new(&conn)
            .list_workflow_runs_for_repo("r1", 3)
            .unwrap();
        assert_eq!(runs.len(), 3);
    }

    #[test]
    fn test_list_workflow_runs_for_repo_multiple_worktrees() {
        // Runs from different worktrees of the same repo should all appear.
        let conn = setup_db();
        create_worktree_run(&conn, "w1");
        create_worktree_run(&conn, "w2");
        let runs = WorkflowManager::new(&conn)
            .list_workflow_runs_for_repo("r1", 50)
            .unwrap();
        assert_eq!(runs.len(), 2);
    }

    #[test]
    fn test_list_workflow_runs_for_repo_empty() {
        let conn = setup_db();
        let runs = WorkflowManager::new(&conn)
            .list_workflow_runs_for_repo("r1", 50)
            .unwrap();
        assert!(runs.is_empty());
    }

    // ── list_active_workflow_runs ────────────────────────────────────────────

    #[test]
    fn test_list_active_workflow_runs_empty_slice_defaults_to_pending_running_waiting() {
        // Empty status slice should default to [pending, running, waiting].
        // A completed run must NOT appear; pending/running runs must appear.
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        let pending_run = create_worktree_run(&conn, "w1"); // created as pending
        let running_run = create_worktree_run(&conn, "w1");
        mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None)
            .unwrap();
        let completed_run = create_worktree_run(&conn, "w1");
        mgr.update_workflow_status(&completed_run.id, WorkflowRunStatus::Completed, None)
            .unwrap();

        let runs = mgr.list_active_workflow_runs(&[]).unwrap();
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

        assert!(
            ids.contains(&pending_run.id.as_str()),
            "pending run must appear"
        );
        assert!(
            ids.contains(&running_run.id.as_str()),
            "running run must appear"
        );
        assert!(
            !ids.contains(&completed_run.id.as_str()),
            "completed run must not appear"
        );
    }

    #[test]
    fn test_list_active_workflow_runs_explicit_status_filter() {
        // When an explicit status slice is given, only runs with those statuses appear.
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        let pending_run = create_worktree_run(&conn, "w1");
        let running_run = create_worktree_run(&conn, "w1");
        mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None)
            .unwrap();

        // Ask only for running — pending must not appear.
        let runs = mgr
            .list_active_workflow_runs(&[WorkflowRunStatus::Running])
            .unwrap();
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

        assert!(
            ids.contains(&running_run.id.as_str()),
            "running run must appear"
        );
        assert!(
            !ids.contains(&pending_run.id.as_str()),
            "pending run must not appear when filter is running-only"
        );
    }

    #[test]
    fn test_list_active_workflow_runs_null_worktree_included() {
        // Runs with no worktree_id (ephemeral/repo-targeted runs) must always appear.
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        let repo_run = create_repo_run(&conn, "r1"); // worktree_id IS NULL

        let runs = mgr
            .list_active_workflow_runs(&[WorkflowRunStatus::Pending])
            .unwrap();
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

        assert!(
            ids.contains(&repo_run.id.as_str()),
            "repo-targeted run with NULL worktree_id must be included"
        );
    }

    #[test]
    fn test_list_active_workflow_runs_inactive_worktree_excluded() {
        // Runs linked to a non-active (e.g. merged) worktree must not appear.
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        let run = create_worktree_run(&conn, "w1");

        // Mark w1 as merged so it no longer counts as active.
        conn.execute("UPDATE worktrees SET status = 'merged' WHERE id = 'w1'", [])
            .unwrap();

        let runs = mgr
            .list_active_workflow_runs(&[WorkflowRunStatus::Pending])
            .unwrap();
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

        assert!(
            !ids.contains(&run.id.as_str()),
            "run linked to a merged worktree must not appear"
        );
    }

    // --- list_all_waiting_gate_steps ---

    #[test]
    fn test_list_all_waiting_gate_steps_empty() {
        let conn = setup_db();
        let steps = WorkflowManager::new(&conn)
            .list_all_waiting_gate_steps()
            .unwrap();
        assert!(steps.is_empty(), "no gate steps should exist yet");
    }

    #[test]
    fn test_list_all_waiting_gate_steps_returns_waiting_gate_steps() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let run = create_worktree_run(&conn, "w1");

        let step_id = mgr
            .insert_step(&run.id, "approval-gate", "gate", false, 0, 0)
            .unwrap();
        mgr.set_step_gate_info(&step_id, "human", Some("Please approve"), "1h")
            .unwrap();
        // Mark step as waiting so it appears in the query.
        set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

        let steps = mgr.list_all_waiting_gate_steps().unwrap();
        assert_eq!(steps.len(), 1, "one waiting gate step should be returned");
        let (step, workflow_name, target_label) = &steps[0];
        assert_eq!(step.id, step_id);
        assert_eq!(step.step_name, "approval-gate");
        assert_eq!(workflow_name, "wf");
        assert!(target_label.is_none(), "no target_label set on this run");
    }

    #[test]
    fn test_list_all_waiting_gate_steps_excludes_non_gate_steps() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let run = create_worktree_run(&conn, "w1");

        // Regular step with no gate_type — must not appear.
        let step_id = mgr
            .insert_step(&run.id, "regular-step", "actor", false, 0, 0)
            .unwrap();
        set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

        let steps = mgr.list_all_waiting_gate_steps().unwrap();
        assert!(
            steps.is_empty(),
            "steps without gate_type must not be returned"
        );
    }

    #[test]
    fn test_list_active_workflow_runs_multiple_statuses_dynamic_placeholders() {
        // Passing two explicit statuses exercises the dynamic placeholder builder.
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        let pending_run = create_worktree_run(&conn, "w1");
        let running_run = create_worktree_run(&conn, "w1");
        mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None)
            .unwrap();
        let failed_run = create_worktree_run(&conn, "w1");
        mgr.update_workflow_status(&failed_run.id, WorkflowRunStatus::Failed, None)
            .unwrap();

        let runs = mgr
            .list_active_workflow_runs(&[WorkflowRunStatus::Pending, WorkflowRunStatus::Running])
            .unwrap();
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

        assert!(ids.contains(&pending_run.id.as_str()));
        assert!(ids.contains(&running_run.id.as_str()));
        assert!(!ids.contains(&failed_run.id.as_str()));
    }

    #[test]
    fn test_sql_placeholders() {
        assert_eq!(sql_placeholders(0), "");
        assert_eq!(sql_placeholders(1), "?1");
        assert_eq!(sql_placeholders(3), "?1, ?2, ?3");
    }

    #[test]
    fn test_sql_placeholders_from_non_one_start() {
        assert_eq!(sql_placeholders_from(0, 5), "");
        assert_eq!(sql_placeholders_from(1, 2), "?2");
        assert_eq!(sql_placeholders_from(3, 4), "?4, ?5, ?6");
    }

    #[test]
    fn test_get_active_steps_for_runs_groups_by_run_id() {
        // Seed two runs, each with one running step (and one completed step that
        // should be excluded).  Verify that get_active_steps_for_runs returns
        // only the running steps and groups them under the correct run_id.
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        let run1 = create_worktree_run(&conn, "w1");
        let run2 = create_worktree_run(&conn, "w2");

        // run1: one running step, one completed step
        let step1_active = mgr
            .insert_step(&run1.id, "step-a", "actor", false, 0, 0)
            .unwrap();
        set_step_status(&mgr, &step1_active, WorkflowStepStatus::Running);
        let step1_done = mgr
            .insert_step(&run1.id, "step-b", "actor", false, 1, 0)
            .unwrap();
        set_step_status(&mgr, &step1_done, WorkflowStepStatus::Completed);

        // run2: one running step only
        let step2_active = mgr
            .insert_step(&run2.id, "step-c", "actor", false, 0, 0)
            .unwrap();
        set_step_status(&mgr, &step2_active, WorkflowStepStatus::Running);

        let result = mgr
            .get_active_steps_for_runs(&[run1.id.as_str(), run2.id.as_str()])
            .unwrap();

        // Each run should be present with exactly its active step.
        assert_eq!(result.len(), 2, "expected entries for both runs");

        let run1_steps = result.get(&run1.id).expect("run1 missing from result");
        assert_eq!(run1_steps.len(), 1, "run1 should have 1 active step");
        assert_eq!(run1_steps[0].id, step1_active);

        let run2_steps = result.get(&run2.id).expect("run2 missing from result");
        assert_eq!(run2_steps.len(), 1, "run2 should have 1 active step");
        assert_eq!(run2_steps[0].id, step2_active);
    }

    #[test]
    fn test_get_active_steps_for_runs_includes_waiting_steps() {
        // Verify that get_active_steps_for_runs returns Waiting steps (not just
        // Running ones), and excludes Pending steps.
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        let run = create_worktree_run(&conn, "w1");

        // Insert a step and transition it to Waiting.
        let waiting_step = mgr
            .insert_step(&run.id, "step-a", "actor", false, 0, 0)
            .unwrap();
        set_step_status(&mgr, &waiting_step, WorkflowStepStatus::Waiting);

        // Insert a second step and leave it Pending — should be excluded.
        let _pending_step = mgr
            .insert_step(&run.id, "step-b", "actor", false, 1, 0)
            .unwrap();

        let result = mgr.get_active_steps_for_runs(&[run.id.as_str()]).unwrap();

        // Only the Waiting step should appear.
        assert_eq!(result.len(), 1, "expected one run entry");
        let steps = result.get(&run.id).expect("run missing from result");
        assert_eq!(steps.len(), 1, "expected exactly one active step");
        assert_eq!(
            steps[0].id, waiting_step,
            "active step should be the Waiting one"
        );
    }

    #[test]
    fn test_get_active_steps_for_runs_empty_slice_returns_empty_map() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let result = mgr.get_active_steps_for_runs(&[]).unwrap();
        assert!(result.is_empty(), "empty run_ids must yield an empty map");
    }

    #[test]
    fn test_get_steps_for_runs_empty_slice_returns_empty_map() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let result = mgr.get_steps_for_runs(&[]).unwrap();
        assert!(result.is_empty(), "empty run_ids must yield an empty map");
    }

    #[test]
    fn test_get_workflow_run_ids_for_agent_runs_empty_slice_returns_empty_map() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let result = mgr.get_workflow_run_ids_for_agent_runs(&[]).unwrap();
        assert!(
            result.is_empty(),
            "empty agent_run_ids must yield an empty map"
        );
    }

    #[test]
    fn test_get_step_summaries_for_runs_empty_slice_returns_empty_map() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let result = mgr.get_step_summaries_for_runs(&[]).unwrap();
        assert!(result.is_empty(), "empty run_ids must yield an empty map");
    }

    #[test]
    fn test_get_steps_for_runs_returns_all_steps_regardless_of_status() {
        // Verify that get_steps_for_runs returns ALL steps (pending, running,
        // completed) for multiple runs, grouped by run_id.
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        let run1 = create_worktree_run(&conn, "w1");
        let run2 = create_worktree_run(&conn, "w2");

        // run1: one running step and one completed step — both should appear
        let step1a = mgr
            .insert_step(&run1.id, "step-a", "actor", false, 0, 0)
            .unwrap();
        set_step_status(&mgr, &step1a, WorkflowStepStatus::Running);
        let step1b = mgr
            .insert_step(&run1.id, "step-b", "actor", false, 1, 0)
            .unwrap();
        set_step_status(&mgr, &step1b, WorkflowStepStatus::Completed);

        // run2: one pending step
        let step2a = mgr
            .insert_step(&run2.id, "step-c", "actor", false, 0, 0)
            .unwrap();

        let result = mgr
            .get_steps_for_runs(&[run1.id.as_str(), run2.id.as_str()])
            .unwrap();

        assert_eq!(result.len(), 2, "expected entries for both runs");

        let run1_steps = result.get(&run1.id).expect("run1 missing from result");
        assert_eq!(run1_steps.len(), 2, "run1 should have both steps");
        let run1_ids: Vec<&str> = run1_steps.iter().map(|s| s.id.as_str()).collect();
        assert!(run1_ids.contains(&step1a.as_str()));
        assert!(run1_ids.contains(&step1b.as_str()));

        let run2_steps = result.get(&run2.id).expect("run2 missing from result");
        assert_eq!(run2_steps.len(), 1, "run2 should have one step");
        assert_eq!(run2_steps[0].id, step2a);
    }

    // ── list_workflow_runs_filtered ──────────────────────────────────────────

    #[test]
    fn test_list_workflow_runs_filtered_with_status() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        let pending_run = create_worktree_run(&conn, "w1");
        let running_run = create_worktree_run(&conn, "w1");
        mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None)
            .unwrap();

        let runs = mgr
            .list_workflow_runs_filtered("w1", Some(WorkflowRunStatus::Running))
            .unwrap();
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

        assert!(
            ids.contains(&running_run.id.as_str()),
            "running run must appear"
        );
        assert!(
            !ids.contains(&pending_run.id.as_str()),
            "pending run must not appear"
        );
    }

    #[test]
    fn test_list_workflow_runs_filtered_none_returns_all() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        let pending_run = create_worktree_run(&conn, "w1");
        let running_run = create_worktree_run(&conn, "w1");
        mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None)
            .unwrap();

        let runs = mgr.list_workflow_runs_filtered("w1", None).unwrap();
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

        assert!(
            ids.contains(&pending_run.id.as_str()),
            "pending run must appear"
        );
        assert!(
            ids.contains(&running_run.id.as_str()),
            "running run must appear"
        );
    }

    // ── list_workflow_runs_by_repo_id_filtered ───────────────────────────────

    #[test]
    fn test_list_workflow_runs_by_repo_id_filtered_with_status() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        // Use repo-targeted runs so workflow_runs.repo_id is set.
        let pending_run = create_repo_run(&conn, "r1");
        let running_run = create_repo_run(&conn, "r1");
        mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None)
            .unwrap();

        let runs = mgr
            .list_workflow_runs_by_repo_id_filtered("r1", 50, 0, Some(WorkflowRunStatus::Running))
            .unwrap();
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

        assert!(
            ids.contains(&running_run.id.as_str()),
            "running run must appear"
        );
        assert!(
            !ids.contains(&pending_run.id.as_str()),
            "pending run must not appear"
        );
    }

    #[test]
    fn test_list_workflow_runs_by_repo_id_filtered_none_returns_all() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        let pending_run = create_repo_run(&conn, "r1");
        let running_run = create_repo_run(&conn, "r1");
        mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None)
            .unwrap();

        let runs = mgr
            .list_workflow_runs_by_repo_id_filtered("r1", 50, 0, None)
            .unwrap();
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

        assert!(
            ids.contains(&pending_run.id.as_str()),
            "pending run must appear"
        );
        assert!(
            ids.contains(&running_run.id.as_str()),
            "running run must appear"
        );
    }

    // ── list_workflow_runs_filtered_paginated ────────────────────────────────

    #[test]
    fn test_list_workflow_runs_filtered_paginated_with_status() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        // Two pending runs plus one running run for the same worktree.
        let _pending1 = create_worktree_run(&conn, "w1");
        let _pending2 = create_worktree_run(&conn, "w1");
        let running = create_worktree_run(&conn, "w1");
        mgr.update_workflow_status(&running.id, WorkflowRunStatus::Running, None)
            .unwrap();

        // First page: limit=1, offset=0 — exactly one pending run.
        let page1 = mgr
            .list_workflow_runs_filtered_paginated("w1", Some(WorkflowRunStatus::Pending), 1, 0)
            .unwrap();
        assert_eq!(
            page1.len(),
            1,
            "first page must have exactly one pending run"
        );

        // Second page: limit=1, offset=1 — the other pending run.
        let page2 = mgr
            .list_workflow_runs_filtered_paginated("w1", Some(WorkflowRunStatus::Pending), 1, 1)
            .unwrap();
        assert_eq!(
            page2.len(),
            1,
            "second page must have exactly one pending run"
        );

        assert_ne!(page1[0].id, page2[0].id, "pages must return different runs");
        assert!(
            page1[0].id != running.id && page2[0].id != running.id,
            "running run must not appear in pending-filtered results"
        );
    }

    #[test]
    fn test_list_workflow_runs_filtered_paginated_none_delegates() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        for _ in 0..3 {
            create_worktree_run(&conn, "w1");
        }

        // None — no status filter, pagination alone controls results.
        let page1 = mgr
            .list_workflow_runs_filtered_paginated("w1", None, 2, 0)
            .unwrap();
        assert_eq!(page1.len(), 2, "limit=2 must return exactly 2 runs");

        let page2 = mgr
            .list_workflow_runs_filtered_paginated("w1", None, 2, 2)
            .unwrap();
        assert_eq!(
            page2.len(),
            1,
            "offset=2 with limit=2 must return the remaining run"
        );
    }

    // ── list_all_workflow_runs_filtered_paginated ────────────────────────────

    #[test]
    fn test_list_all_workflow_runs_filtered_paginated_with_status() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        let pending_run = create_worktree_run(&conn, "w1");
        let running_run = create_worktree_run(&conn, "w1");
        mgr.update_workflow_status(&running_run.id, WorkflowRunStatus::Running, None)
            .unwrap();

        let runs = mgr
            .list_all_workflow_runs_filtered_paginated(Some(WorkflowRunStatus::Running), 50, 0)
            .unwrap();
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

        assert!(
            ids.contains(&running_run.id.as_str()),
            "running run must appear"
        );
        assert!(
            !ids.contains(&pending_run.id.as_str()),
            "pending run must not appear"
        );
    }

    #[test]
    fn test_list_all_workflow_runs_filtered_paginated_none() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        let run1 = create_worktree_run(&conn, "w1");
        let run2 = create_worktree_run(&conn, "w2");
        mgr.update_workflow_status(&run2.id, WorkflowRunStatus::Running, None)
            .unwrap();

        let runs = mgr
            .list_all_workflow_runs_filtered_paginated(None, 50, 0)
            .unwrap();
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

        assert!(ids.contains(&run1.id.as_str()), "run1 must appear");
        assert!(ids.contains(&run2.id.as_str()), "run2 must appear");
    }

    #[test]
    fn test_list_all_workflow_runs_filtered_paginated_excludes_inactive_worktrees() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        let active_run = create_worktree_run(&conn, "w1");
        let inactive_run = create_worktree_run(&conn, "w2");
        conn.execute("UPDATE worktrees SET status = 'merged' WHERE id = 'w2'", [])
            .unwrap();

        let runs = mgr
            .list_all_workflow_runs_filtered_paginated(None, 50, 0)
            .unwrap();
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

        assert!(
            ids.contains(&active_run.id.as_str()),
            "active worktree run must appear"
        );
        assert!(
            !ids.contains(&inactive_run.id.as_str()),
            "merged worktree run must not appear"
        );
    }

    // ── list_all_workflow_runs ───────────────────────────────────────────────

    #[test]
    fn test_list_all_workflow_runs_respects_limit() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        for _ in 0..5 {
            create_worktree_run(&conn, "w1");
        }

        let runs = mgr.list_all_workflow_runs(3).unwrap();
        assert_eq!(runs.len(), 3, "limit=3 must return exactly 3 runs");
    }

    #[test]
    fn test_list_all_workflow_runs_excludes_inactive_worktrees() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        let active_run = create_worktree_run(&conn, "w1");
        let inactive_run = create_worktree_run(&conn, "w2");
        conn.execute("UPDATE worktrees SET status = 'merged' WHERE id = 'w2'", [])
            .unwrap();

        let runs = mgr.list_all_workflow_runs(50).unwrap();
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();

        assert!(
            ids.contains(&active_run.id.as_str()),
            "active worktree run must appear"
        );
        assert!(
            !ids.contains(&inactive_run.id.as_str()),
            "merged worktree run must not appear"
        );
    }

    #[test]
    fn test_list_all_waiting_gate_steps_excludes_approved_gate_steps() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let run = create_worktree_run(&conn, "w1");

        let step_id = mgr
            .insert_step(&run.id, "gate", "gate", false, 0, 0)
            .unwrap();
        mgr.set_step_gate_info(&step_id, "human", None, "1h")
            .unwrap();
        // Mark as completed (approved) — must not appear in waiting list.
        conn.execute(
            "UPDATE workflow_run_steps SET status = 'completed', gate_approved_at = '2024-01-01T00:00:00Z' WHERE id = ?1",
            rusqlite::params![step_id],
        ).unwrap();

        let steps = mgr.list_all_waiting_gate_steps().unwrap();
        assert!(
            steps.is_empty(),
            "approved (completed) gate steps must not be returned"
        );
    }

    #[test]
    fn test_list_all_waiting_gate_steps_includes_target_label() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let parent_id = make_parent_id(&conn, "w1");
        let run = mgr
            .create_workflow_run_with_targets(
                "deploy",
                Some("w1"),
                None,
                None,
                &parent_id,
                false,
                "manual",
                None,
                None,
                Some("conductor-ai/feat-123"),
            )
            .unwrap();

        let step_id = mgr
            .insert_step(&run.id, "approve-deploy", "gate", false, 0, 0)
            .unwrap();
        mgr.set_step_gate_info(&step_id, "human", None, "1h")
            .unwrap();
        set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

        let steps = mgr.list_all_waiting_gate_steps().unwrap();
        assert_eq!(steps.len(), 1);
        let (step, workflow_name, target_label) = &steps[0];
        assert_eq!(step.id, step_id);
        assert_eq!(workflow_name, "deploy");
        assert_eq!(
            target_label.as_deref(),
            Some("conductor-ai/feat-123"),
            "target_label must be propagated from workflow_runs"
        );
    }

    // --- list_waiting_gate_steps_for_repo ---

    #[test]
    fn test_list_waiting_gate_steps_for_repo_empty() {
        let conn = setup_db();
        let steps = WorkflowManager::new(&conn)
            .list_waiting_gate_steps_for_repo("r1")
            .unwrap();
        assert!(steps.is_empty(), "no gate steps should exist yet");
    }

    #[test]
    fn test_list_waiting_gate_steps_for_repo_via_worktree() {
        // Runs linked through a worktree (worktree_id set, repo_id NULL) must appear.
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        // w1 belongs to r1 (seeded in setup_db via test_helpers)
        let run = create_worktree_run(&conn, "w1");

        let step_id = mgr
            .insert_step(&run.id, "approval-gate", "gate", false, 0, 0)
            .unwrap();
        mgr.set_step_gate_info(&step_id, "human", Some("Please approve"), "1h")
            .unwrap();
        set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

        let steps = mgr.list_waiting_gate_steps_for_repo("r1").unwrap();
        assert_eq!(
            steps.len(),
            1,
            "worktree-linked gate step must appear for its repo"
        );
        let row = &steps[0];
        assert_eq!(row.step.id, step_id);
        assert_eq!(row.step.step_name, "approval-gate");
        assert_eq!(row.workflow_name, "wf");
        assert!(row.target_label.is_none());
    }

    #[test]
    fn test_list_waiting_gate_steps_for_repo_via_direct_repo_id() {
        // Runs with repo_id set directly (no worktree) must also appear.
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let run = create_repo_run(&conn, "r1");

        let step_id = mgr
            .insert_step(&run.id, "direct-gate", "gate", false, 0, 0)
            .unwrap();
        mgr.set_step_gate_info(&step_id, "human", None, "1h")
            .unwrap();
        set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

        let steps = mgr.list_waiting_gate_steps_for_repo("r1").unwrap();
        assert_eq!(
            steps.len(),
            1,
            "directly-linked gate step must appear for its repo"
        );
        assert_eq!(steps[0].step.id, step_id);
    }

    #[test]
    fn test_list_waiting_gate_steps_for_repo_excludes_other_repo() {
        // Gate steps from r2 must not appear when querying r1, and vice versa.
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        // w3 belongs to r2 (inserted in setup_db)
        let run = create_worktree_run(&conn, "w3");

        let step_id = mgr
            .insert_step(&run.id, "gate-other", "gate", false, 0, 0)
            .unwrap();
        mgr.set_step_gate_info(&step_id, "human", None, "1h")
            .unwrap();
        set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

        let steps_r1 = mgr.list_waiting_gate_steps_for_repo("r1").unwrap();
        assert!(steps_r1.is_empty(), "r1 must not see r2's gate steps");

        let steps_r2 = mgr.list_waiting_gate_steps_for_repo("r2").unwrap();
        assert_eq!(steps_r2.len(), 1, "r2 should return its own gate step");
        assert_eq!(steps_r2[0].step.id, step_id);
    }

    #[test]
    fn test_list_waiting_gate_steps_for_repo_excludes_non_gate_steps() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let run = create_worktree_run(&conn, "w1");

        // A regular actor step with waiting status must not be returned.
        let step_id = mgr
            .insert_step(&run.id, "regular-step", "actor", false, 0, 0)
            .unwrap();
        set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

        let steps = mgr.list_waiting_gate_steps_for_repo("r1").unwrap();
        assert!(steps.is_empty(), "non-gate steps must not be returned");
    }

    #[test]
    fn test_list_waiting_gate_steps_for_repo_excludes_completed_gate_steps() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let run = create_worktree_run(&conn, "w1");

        let step_id = mgr
            .insert_step(&run.id, "gate", "gate", false, 0, 0)
            .unwrap();
        mgr.set_step_gate_info(&step_id, "human", None, "1h")
            .unwrap();
        conn.execute(
            "UPDATE workflow_run_steps SET status = 'completed', gate_approved_at = '2024-01-01T00:00:00Z' WHERE id = ?1",
            rusqlite::params![step_id],
        ).unwrap();

        let steps = mgr.list_waiting_gate_steps_for_repo("r1").unwrap();
        assert!(
            steps.is_empty(),
            "completed gate steps must not be returned"
        );
    }

    #[test]
    fn test_list_waiting_gate_steps_for_repo_excludes_cancelled_run() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let run = create_worktree_run(&conn, "w1");

        let step_id = mgr
            .insert_step(&run.id, "gate", "gate", false, 0, 0)
            .unwrap();
        mgr.set_step_gate_info(&step_id, "human", None, "1h")
            .unwrap();
        conn.execute(
            "UPDATE workflow_runs SET status = 'cancelled' WHERE id = ?1",
            [&run.id],
        )
        .unwrap();

        let steps = mgr.list_waiting_gate_steps_for_repo("r1").unwrap();
        assert!(
            steps.is_empty(),
            "waiting gate steps from cancelled runs must not be returned"
        );
    }

    #[test]
    fn test_list_waiting_gate_steps_for_repo_excludes_failed_run() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let run = create_worktree_run(&conn, "w1");

        let step_id = mgr
            .insert_step(&run.id, "gate", "gate", false, 0, 0)
            .unwrap();
        mgr.set_step_gate_info(&step_id, "human", None, "1h")
            .unwrap();
        conn.execute(
            "UPDATE workflow_runs SET status = 'failed' WHERE id = ?1",
            [&run.id],
        )
        .unwrap();

        let steps = mgr.list_waiting_gate_steps_for_repo("r1").unwrap();
        assert!(
            steps.is_empty(),
            "waiting gate steps from failed runs must not be returned"
        );
    }
}
