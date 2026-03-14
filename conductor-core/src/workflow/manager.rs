use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json;

use crate::agent::AgentManager;
use crate::config::Config;
use crate::db::query_collect;
use crate::error::{ConductorError, Result};
use crate::workflow_dsl;

use super::constants::{RUN_COLUMNS, STEP_COLUMNS};
use super::status::{WorkflowRunStatus, WorkflowStepStatus};
use super::types::{
    ActiveWorkflowCounts, StepKey, WorkflowRun, WorkflowRunContext, WorkflowRunStep,
    WorkflowStepSummary,
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
        let mut stmt = self.conn.prepare_cached(
            "SELECT repo_id, status, COUNT(*) AS cnt \
             FROM workflow_runs \
             WHERE status IN ('pending', 'running', 'waiting') \
               AND repo_id IS NOT NULL \
             GROUP BY repo_id, status",
        )?;
        let rows = stmt.query_map([], |row| {
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
        let id = ulid::Ulid::new().to_string();
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
        let id = ulid::Ulid::new().to_string();
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
        if run_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = (1..=run_ids.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {STEP_COLUMNS} FROM workflow_run_steps WHERE workflow_run_id IN ({placeholders}) ORDER BY workflow_run_id, position"
        );
        let run_id_strings: Vec<String> = run_ids.iter().map(|s| s.to_string()).collect();
        let params: Vec<&dyn rusqlite::ToSql> = run_id_strings
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let steps = stmt
            .query_map(params.as_slice(), row_to_workflow_step)?
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
        let result = self.conn.query_row(
            &format!(
                "SELECT {RUN_COLUMNS} FROM workflow_runs \
                 WHERE worktree_id = ?1 AND status IN ('pending', 'running', 'waiting') \
                 LIMIT 1"
            ),
            params![worktree_id],
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
        let placeholders = agent_run_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT child_run_id, workflow_run_id \
             FROM workflow_run_steps \
             WHERE child_run_id IN ({placeholders}) \
             GROUP BY child_run_id"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
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
        let placeholders = (1..=run_ids.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let name_sql =
            format!("SELECT id, workflow_name FROM workflow_runs WHERE id IN ({placeholders})");
        let run_id_strings: Vec<String> = run_ids.iter().map(|s| s.to_string()).collect();
        let name_params: Vec<&dyn rusqlite::ToSql> = run_id_strings
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();
        let mut name_stmt = self.conn.prepare_cached(&name_sql)?;
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
/// Returns `(where_clause, params)` where `params` is a `Vec<String>` whose
/// elements bind to the positional placeholders in the clause.
fn purge_where_clause(statuses: &[&str], repo_id: Option<&str>) -> (String, Vec<String>) {
    let n = statuses.len();
    let placeholders = (1..=n)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
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

pub(super) fn row_to_workflow_step(row: &rusqlite::Row) -> rusqlite::Result<WorkflowRunStep> {
    let can_commit_int: i64 = row.get(4)?;
    let condition_met_int: Option<i64> = row.get(12)?;
    Ok(WorkflowRunStep {
        id: row.get(0)?,
        workflow_run_id: row.get(1)?,
        step_name: row.get(2)?,
        role: row.get(3)?,
        can_commit: can_commit_int != 0,
        condition_expr: row.get(5)?,
        status: row.get(6)?,
        child_run_id: row.get(7)?,
        position: row.get(8)?,
        started_at: row.get(9)?,
        ended_at: row.get(10)?,
        result_text: row.get(11)?,
        condition_met: condition_met_int.map(|v| v != 0),
        iteration: row.get(13)?,
        parallel_group_id: row.get(14)?,
        context_out: row.get(15)?,
        markers_out: row.get(16)?,
        retry_count: row.get(17)?,
        gate_type: row.get(18)?,
        gate_prompt: row.get(19)?,
        gate_timeout: row.get(20)?,
        gate_approved_by: row.get(21)?,
        gate_approved_at: row.get(22)?,
        gate_feedback: row.get(23)?,
        structured_output: row.get(24)?,
    })
}
