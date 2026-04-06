use std::collections::HashMap;

use rusqlite::{params, OptionalExtension};

use crate::config::Config;
use crate::db::{query_collect, sql_placeholders, sql_placeholders_from};
use crate::error::{ConductorError, Result};

use super::helpers::{
    pending_gate_row_mapper, row_to_workflow_run, row_to_workflow_step,
    waiting_gate_step_row_mapper,
};
use super::WorkflowManager;
use crate::workflow::constants::{RUN_COLUMNS, STEP_COLUMNS, STEP_COLUMNS_WITH_PREFIX};
use crate::workflow::status::WorkflowRunStatus;
use crate::workflow::types::{
    extract_workflow_title, ActiveWorkflowCounts, PendingGateRow, StepFailureHeatmapRow,
    StepTokenHeatmapRow, WorkflowFailureRateTrendRow, WorkflowPercentiles, WorkflowRun,
    WorkflowRunContext, WorkflowRunMetricsRow, WorkflowRunStep, WorkflowStepSummary,
    WorkflowTokenAggregate, WorkflowTokenTrendRow,
};

impl<'a> WorkflowManager<'a> {
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

    pub fn get_workflow_run(&self, id: &str) -> Result<Option<WorkflowRun>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE id = ?1"),
                params![id],
                row_to_workflow_run,
            )
            .optional()?)
    }

    /// List child workflow runs for a given parent run, ordered by start time.
    pub fn list_child_workflow_runs(&self, parent_run_id: &str) -> Result<Vec<WorkflowRun>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {RUN_COLUMNS} FROM workflow_runs \
             WHERE parent_workflow_run_id = ?1 \
             ORDER BY started_at ASC"
        ))?;
        let rows = stmt.query_map(params![parent_run_id], row_to_workflow_run)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
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
            &format!(
                "SELECT {cols}, ar.input_tokens, ar.output_tokens, ar.cache_read_input_tokens, ar.cache_creation_input_tokens \
                 FROM workflow_run_steps s \
                 LEFT JOIN agent_runs ar ON s.child_run_id = ar.id \
                 WHERE s.workflow_run_id = ?1 \
                 ORDER BY s.position",
                cols = &*STEP_COLUMNS_WITH_PREFIX
            ),
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
            format!(" AND s.status IN ({status_placeholders})")
        } else {
            String::new()
        };
        let sql = format!(
            "SELECT {cols}, ar.input_tokens, ar.output_tokens, ar.cache_read_input_tokens, ar.cache_creation_input_tokens \
             FROM workflow_run_steps s \
             LEFT JOIN agent_runs ar ON s.child_run_id = ar.id \
             WHERE s.workflow_run_id IN ({placeholders}){status_clause} \
             ORDER BY s.workflow_run_id, s.position",
            cols = &*STEP_COLUMNS_WITH_PREFIX
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
            "SELECT {cols}, ar.input_tokens, ar.output_tokens, ar.cache_read_input_tokens, ar.cache_creation_input_tokens \
             FROM workflow_run_steps s \
             LEFT JOIN agent_runs ar ON s.child_run_id = ar.id \
             WHERE s.id = ?1",
            cols = &*STEP_COLUMNS_WITH_PREFIX
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
        Ok(self
            .conn
            .query_row(
                &sql,
                rusqlite::params_from_iter(all_params.iter()),
                row_to_workflow_run,
            )
            .optional()?)
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
                     ORDER BY started_at DESC LIMIT ?3 OFFSET ?4"
                ),
                params![worktree_id, status_str, limit as i64, offset as i64],
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
            "SELECT workflow_runs.* \
             FROM workflow_runs \
             LEFT JOIN worktrees ON worktrees.id = workflow_runs.worktree_id \
             WHERE workflow_runs.worktree_id IS NULL OR worktrees.status = 'active' \
             ORDER BY workflow_runs.started_at DESC LIMIT ?1",
            params![limit as i64],
            row_to_workflow_run,
        )
    }

    /// When `statuses` is empty, returns `WorkflowRunStatus::ACTIVE`; otherwise returns `statuses`.
    fn effective_statuses(statuses: &[WorkflowRunStatus]) -> &[WorkflowRunStatus] {
        if statuses.is_empty() {
            &WorkflowRunStatus::ACTIVE
        } else {
            statuses
        }
    }

    /// List workflow runs across all worktrees filtered by a set of statuses.
    /// When `statuses` is empty, defaults to `[running, waiting, pending]`.
    /// Only includes runs whose associated worktree is `active` (or runs with no worktree).
    /// Ordered by `started_at DESC`.
    pub fn list_active_workflow_runs(
        &self,
        statuses: &[WorkflowRunStatus],
    ) -> Result<Vec<WorkflowRun>> {
        let effective = Self::effective_statuses(statuses);

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
                "SELECT workflow_runs.* \
                 FROM workflow_runs \
                 LEFT JOIN worktrees ON worktrees.id = workflow_runs.worktree_id \
                 WHERE (workflow_runs.worktree_id IS NULL OR worktrees.status = 'active') \
                   AND workflow_runs.status = ?1 \
                 ORDER BY workflow_runs.started_at DESC LIMIT ?2 OFFSET ?3",
                params![status_str, limit as i64, offset as i64],
                row_to_workflow_run,
            )
        } else {
            query_collect(
                self.conn,
                "SELECT workflow_runs.* \
                 FROM workflow_runs \
                 LEFT JOIN worktrees ON worktrees.id = workflow_runs.worktree_id \
                 WHERE workflow_runs.worktree_id IS NULL OR worktrees.status = 'active' \
                 ORDER BY workflow_runs.started_at DESC LIMIT ?1 OFFSET ?2",
                params![limit as i64, offset as i64],
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
            "SELECT workflow_runs.* \
             FROM workflow_runs \
             LEFT JOIN worktrees ON worktrees.id = workflow_runs.worktree_id \
             WHERE workflow_runs.repo_id = ?1 \
               AND (workflow_runs.worktree_id IS NULL OR worktrees.status = 'active') \
             ORDER BY workflow_runs.started_at DESC LIMIT ?2 OFFSET ?3",
            params![repo_id, limit as i64, offset as i64],
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
                 ORDER BY started_at DESC LIMIT ?2 OFFSET ?3"
            ),
            params![worktree_id, limit as i64, offset as i64],
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
                 ORDER BY started_at DESC LIMIT ?1"
            ),
            params![limit as i64],
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

    /// List workflow runs filtered by a set of statuses and scoped to a single repo.
    /// Covers both direct association (`workflow_runs.repo_id = repo_id`) and indirect
    /// association via a worktree (`worktrees.repo_id = repo_id`).
    /// When `statuses` is empty, defaults to `[running, waiting, pending]`.
    /// Only includes runs whose associated worktree is `active` (or runs with no worktree).
    /// Ordered by `started_at DESC`.
    pub fn list_active_workflow_runs_for_repo(
        &self,
        repo_id: &str,
        statuses: &[WorkflowRunStatus],
    ) -> Result<Vec<WorkflowRun>> {
        let effective = Self::effective_statuses(statuses);

        let placeholders = sql_placeholders_from(effective.len(), 2);

        let sql = format!(
            "SELECT DISTINCT workflow_runs.* \
             FROM workflow_runs \
             LEFT JOIN worktrees ON worktrees.id = workflow_runs.worktree_id \
             WHERE (workflow_runs.repo_id = ?1 OR worktrees.repo_id = ?1) \
               AND (workflow_runs.worktree_id IS NULL OR worktrees.status = 'active') \
               AND workflow_runs.status IN ({placeholders}) \
             ORDER BY workflow_runs.started_at DESC"
        );

        let status_strings: Vec<String> = effective.iter().map(|s| s.to_string()).collect();
        let mut all_params: Vec<rusqlite::types::Value> =
            vec![rusqlite::types::Value::Text(repo_id.to_owned())];
        all_params.extend(status_strings.into_iter().map(rusqlite::types::Value::Text));
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(all_params.iter()),
            row_to_workflow_run,
        )?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
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

    /// Find the waiting gate step for a workflow run.
    pub fn find_waiting_gate(&self, workflow_run_id: &str) -> Result<Option<WorkflowRunStep>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {STEP_COLUMNS} FROM workflow_run_steps \
                     WHERE workflow_run_id = ?1 AND gate_type IS NOT NULL AND gate_approved_at IS NULL \
                       AND status IN ('running', 'waiting') \
                     ORDER BY position DESC LIMIT 1"
                ),
                params![workflow_run_id],
                row_to_workflow_step,
            )
            .optional()?)
    }

    /// List all gate steps currently in `waiting` status across all workflow runs.
    ///
    /// Returns `(step, workflow_name, target_label)` tuples. Used by the TUI background poller to
    /// fire cross-process gate-waiting notifications.
    pub fn list_all_waiting_gate_steps(
        &self,
    ) -> Result<Vec<(WorkflowRunStep, String, Option<String>)>> {
        let placeholders = sql_placeholders(WorkflowRunStatus::ACTIVE.len());
        let active_strings = WorkflowRunStatus::active_strings();
        let sql = format!(
            "SELECT {cols}, r.workflow_name, r.target_label \
             FROM workflow_run_steps s \
             JOIN workflow_runs r ON r.id = s.workflow_run_id \
             WHERE s.gate_type IS NOT NULL AND s.status = 'waiting' \
             AND r.status IN ({placeholders}) \
             ORDER BY s.started_at",
            cols = &*STEP_COLUMNS_WITH_PREFIX,
        );
        crate::db::query_collect(
            self.conn,
            &sql,
            rusqlite::params_from_iter(active_strings.iter()),
            waiting_gate_step_row_mapper,
        )
    }

    /// List gate steps currently in `waiting` status for a specific repo.
    ///
    /// Returns enriched [`PendingGateRow`] values that include the worktree branch and linked
    /// ticket source_id so the TUI can display context without additional queries.
    pub fn list_waiting_gate_steps_for_repo(&self, repo_id: &str) -> Result<Vec<PendingGateRow>> {
        let placeholders = sql_placeholders_from(WorkflowRunStatus::ACTIVE.len(), 2);
        let active_strings = WorkflowRunStatus::active_strings();
        let sql = format!(
            "SELECT {cols}, r.workflow_name, r.target_label, wt.branch, t.source_id AS ticket_ref, r.definition_snapshot \
             FROM workflow_run_steps s \
             JOIN workflow_runs r ON r.id = s.workflow_run_id \
             LEFT JOIN worktrees wt ON wt.id = r.worktree_id \
             LEFT JOIN tickets t ON t.id = r.ticket_id \
             WHERE s.gate_type IS NOT NULL AND s.status = 'waiting' \
             AND r.status IN ({placeholders}) \
             AND (r.repo_id = ?1 OR wt.repo_id = ?1) \
             ORDER BY s.started_at",
            cols = &*STEP_COLUMNS_WITH_PREFIX,
        );
        let mut all_params: Vec<rusqlite::types::Value> =
            vec![rusqlite::types::Value::Text(repo_id.to_owned())];
        all_params.extend(active_strings.into_iter().map(rusqlite::types::Value::Text));
        crate::db::query_collect(
            self.conn,
            &sql,
            rusqlite::params_from_iter(all_params.iter()),
            pending_gate_row_mapper,
        )
    }

    /// Batch-walk active child chains for all given root run IDs in a single recursive CTE query.
    ///
    /// Returns a map from `root_run_id` to the ordered list of `(child_id, child_workflow_name)`
    /// pairs below that root (depth >= 1, ascending). Roots with no active children are absent
    /// from the map. Depth is capped at 5 to match `get_active_chain_for_run`.
    fn get_active_chains_for_runs_batch(
        &self,
        root_ids: &[&str],
    ) -> Result<HashMap<String, Vec<(String, String)>>> {
        if root_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = sql_placeholders(root_ids.len());
        // Seed the CTE with the root runs themselves (depth = 0), then recursively
        // follow active children up to depth 5 (= MAX_DEPTH in get_active_chain_for_run).
        let sql = format!(
            "WITH RECURSIVE chain(root_id, id, workflow_name, depth) AS (\
               SELECT id, id, workflow_name, 0 \
               FROM workflow_runs WHERE id IN ({placeholders}) \
               UNION ALL \
               SELECT c.root_id, r.id, r.workflow_name, c.depth + 1 \
               FROM chain c \
               JOIN workflow_runs r ON r.parent_workflow_run_id = c.id \
               WHERE r.status IN ('running', 'waiting') \
                 AND c.depth < 5 \
             ) \
             SELECT root_id, id, workflow_name, depth \
             FROM chain \
             WHERE depth >= 1 \
             ORDER BY root_id, depth"
        );
        let params: Vec<rusqlite::types::Value> = root_ids
            .iter()
            .map(|s| rusqlite::types::Value::Text(s.to_string()))
            .collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
        let mut map: HashMap<String, Vec<(String, String)>> = HashMap::new();
        while let Some(row) = rows.next()? {
            let root_id: String = row.get(0)?;
            let child_id: String = row.get(1)?;
            let child_name: String = row.get(2)?;
            map.entry(root_id).or_default().push((child_id, child_name));
        }
        Ok(map)
    }

    /// Batch-fetch the first running step for each of the given leaf run IDs.
    ///
    /// Returns a map from `leaf_run_id` to `(step_name, iteration)`. The first running step
    /// by ascending position is returned per run, matching the per-leaf `LIMIT 1` semantics.
    fn get_running_steps_for_leaf_runs(
        &self,
        leaf_ids: &[String],
    ) -> Result<HashMap<String, (String, i64)>> {
        if leaf_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = sql_placeholders(leaf_ids.len());
        let sql = format!(
            "SELECT workflow_run_id, step_name, iteration \
             FROM workflow_run_steps \
             WHERE workflow_run_id IN ({placeholders}) AND status = 'running' \
             ORDER BY workflow_run_id, position ASC"
        );
        let params: Vec<rusqlite::types::Value> = leaf_ids
            .iter()
            .map(|s| rusqlite::types::Value::Text(s.clone()))
            .collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
        let mut map: HashMap<String, (String, i64)> = HashMap::new();
        while let Some(row) = rows.next()? {
            let run_id: String = row.get(0)?;
            // Take only the first (lowest-position) row per run_id.
            map.entry(run_id).or_insert_with(|| {
                let step_name: String = row.get(1).unwrap_or_default();
                let iteration: i64 = row.get(2).unwrap_or(0);
                (step_name, iteration)
            });
        }
        Ok(map)
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
    /// Uses batch queries (3 total) regardless of N to avoid N+1 round-trips.
    pub fn get_step_summaries_for_runs(
        &self,
        run_ids: &[&str],
    ) -> Result<HashMap<String, WorkflowStepSummary>> {
        if run_ids.is_empty() {
            return Ok(HashMap::new());
        }

        // 1. Fetch workflow names for the root runs (single query).
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

        // 2. Walk all active child chains for all roots in one recursive CTE query.
        let chains_map = self.get_active_chains_for_runs_batch(run_ids)?;

        // Derive the leaf run ID per root (deepest child, or root itself).
        let leaf_ids: Vec<String> = run_ids
            .iter()
            .map(|root_id| {
                chains_map
                    .get(*root_id)
                    .and_then(|chain| chain.last())
                    .map(|(id, _)| id.clone())
                    .unwrap_or_else(|| root_id.to_string())
            })
            .collect();

        // 3. Batch-fetch the running step for all leaf runs (single query).
        let steps_map = self.get_running_steps_for_leaf_runs(&leaf_ids)?;

        // Re-assemble WorkflowStepSummary entries.
        let mut map: HashMap<String, WorkflowStepSummary> = HashMap::new();
        for root_id in run_ids {
            let Some(root_name) = root_names.get(*root_id) else {
                continue;
            };

            let child_chain = chains_map.get(*root_id).map(Vec::as_slice).unwrap_or(&[]);

            let leaf_id = child_chain
                .last()
                .map(|(id, _)| id.as_str())
                .unwrap_or(root_id);

            if let Some((step_name, iteration)) = steps_map.get(leaf_id) {
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
                        step_name: step_name.clone(),
                        iteration: *iteration,
                        workflow_chain,
                    },
                );
            }
        }
        Ok(map)
    }

    /// Lightweight cancellation check — queries only the status column.
    ///
    /// Returns `true` when the run exists and its status is `cancelled`.
    /// Returns `false` for any other status or if the run is not found.
    /// Propagates DB errors so callers can decide how to handle them.
    pub fn is_workflow_cancelled(&self, run_id: &str) -> Result<bool> {
        let status: Option<String> = self
            .conn
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(status.as_deref() == Some("cancelled"))
    }

    /// Aggregate token usage per workflow name across all terminal runs (completed + failed).
    /// Token averages are computed only over completed runs to avoid skewing with failed runs.
    /// When `repo_id` is `Some`, restricts to runs for that repo.
    pub fn get_workflow_token_aggregates(
        &self,
        repo_id: Option<&str>,
    ) -> Result<Vec<WorkflowTokenAggregate>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT workflow_name, \
                    COALESCE(AVG(CASE WHEN status='completed' THEN total_input_tokens END), 0.0) AS avg_input, \
                    COALESCE(AVG(CASE WHEN status='completed' THEN total_output_tokens END), 0.0) AS avg_output, \
                    COALESCE(AVG(CASE WHEN status='completed' THEN total_cache_read_input_tokens END), 0.0) AS avg_cache_read, \
                    COALESCE(AVG(CASE WHEN status='completed' THEN total_cache_creation_input_tokens END), 0.0) AS avg_cache_creation, \
                    COUNT(*) AS run_count, \
                    COALESCE(CAST(SUM(CASE WHEN status='completed' THEN 1 ELSE 0 END) AS REAL) / NULLIF(COUNT(*), 0) * 100.0, 0.0) AS success_rate, \
                    MAX(definition_snapshot) AS definition_snapshot \
             FROM workflow_runs \
             WHERE status IN ('completed', 'failed') \
               AND (?1 IS NULL OR repo_id = ?1) \
             GROUP BY workflow_name \
             ORDER BY avg_input + avg_output DESC, run_count DESC",
        )?;
        let rows = stmt.query_map(params![repo_id], |row| {
            let definition_snapshot: Option<String> = row.get(7)?;
            let workflow_title = extract_workflow_title(definition_snapshot.as_deref());
            Ok(WorkflowTokenAggregate {
                workflow_name: row.get(0)?,
                avg_input: row.get(1)?,
                avg_output: row.get(2)?,
                avg_cache_read: row.get(3)?,
                avg_cache_creation: row.get(4)?,
                run_count: row.get(5)?,
                success_rate: row.get(6)?,
                workflow_title,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Token totals grouped by time period (daily or weekly) for a specific workflow.
    pub fn get_workflow_token_trend(
        &self,
        workflow_name: &str,
        granularity: &str,
    ) -> Result<Vec<WorkflowTokenTrendRow>> {
        let fmt = if granularity == "weekly" {
            "%Y-%W"
        } else {
            "%Y-%m-%d"
        };
        let sql = format!(
            "SELECT strftime('{fmt}', started_at) as period, \
                    COALESCE(SUM(total_input_tokens), 0) as total_input, \
                    COALESCE(SUM(total_output_tokens), 0) as total_output, \
                    COALESCE(SUM(total_cache_read_input_tokens), 0) as total_cache_read, \
                    COALESCE(SUM(total_cache_creation_input_tokens), 0) as total_cache_creation \
             FROM workflow_runs \
             WHERE workflow_name = ?1 AND status = 'completed' AND total_input_tokens IS NOT NULL \
             GROUP BY period \
             ORDER BY period DESC \
             LIMIT 30"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![workflow_name], |row| {
            Ok(WorkflowTokenTrendRow {
                period: row.get(0)?,
                total_input: row.get(1)?,
                total_output: row.get(2)?,
                total_cache_read: row.get(3)?,
                total_cache_creation: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Per-step average token usage across the N most recent completed runs of a workflow.
    pub fn get_step_token_heatmap(
        &self,
        workflow_name: &str,
        limit_runs: usize,
    ) -> Result<Vec<StepTokenHeatmapRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT wrs.step_name, \
                    COALESCE(AVG(ar.input_tokens), 0.0) as avg_input, \
                    COALESCE(AVG(ar.output_tokens), 0.0) as avg_output, \
                    COALESCE(AVG(ar.cache_read_input_tokens), 0.0) as avg_cache_read, \
                    COUNT(DISTINCT wr.id) as run_count \
             FROM workflow_runs wr \
             JOIN workflow_run_steps wrs ON wrs.workflow_run_id = wr.id \
             JOIN agent_runs ar ON ar.id = wrs.child_run_id \
             WHERE wr.workflow_name = ?1 AND wr.status = 'completed' \
               AND wr.id IN ( \
                 SELECT id FROM workflow_runs \
                 WHERE workflow_name = ?1 AND status = 'completed' \
                 ORDER BY started_at DESC LIMIT ?2 \
               ) \
             GROUP BY wrs.step_name \
             ORDER BY (AVG(ar.input_tokens) + AVG(ar.output_tokens)) DESC",
        )?;
        let rows = stmt.query_map(params![workflow_name, limit_runs as i64], |row| {
            Ok(StepTokenHeatmapRow {
                step_name: row.get(0)?,
                avg_input: row.get(1)?,
                avg_output: row.get(2)?,
                avg_cache_read: row.get(3)?,
                run_count: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Failure rate grouped by time period (daily or weekly) for a specific workflow.
    /// Counts all terminal runs (completed + failed) per period and computes success rate.
    pub fn get_workflow_failure_rate_trend(
        &self,
        workflow_name: &str,
        granularity: &str,
    ) -> Result<Vec<WorkflowFailureRateTrendRow>> {
        let fmt = if granularity == "weekly" {
            "%Y-%W"
        } else {
            "%Y-%m-%d"
        };
        let sql = format!(
            "SELECT strftime('{fmt}', started_at) AS period, \
                    COUNT(*) AS total_runs, \
                    SUM(CASE WHEN status='failed' THEN 1 ELSE 0 END) AS failed_runs, \
                    COALESCE(CAST(SUM(CASE WHEN status='completed' THEN 1 ELSE 0 END) AS REAL) / NULLIF(COUNT(*), 0) * 100.0, 0.0) AS success_rate \
             FROM workflow_runs \
             WHERE workflow_name = ?1 AND status IN ('completed', 'failed') \
             GROUP BY period \
             ORDER BY period DESC \
             LIMIT 30"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![workflow_name], |row| {
            Ok(WorkflowFailureRateTrendRow {
                period: row.get(0)?,
                total_runs: row.get(1)?,
                failed_runs: row.get(2)?,
                success_rate: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Per-step failure statistics across the N most recent terminal runs of a workflow.
    /// Only counts steps with status `completed` or `failed` (skipped steps are excluded).
    pub fn get_step_failure_heatmap(
        &self,
        workflow_name: &str,
        limit_runs: usize,
    ) -> Result<Vec<StepFailureHeatmapRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT wrs.step_name, \
                    COUNT(*) AS total_executions, \
                    SUM(CASE WHEN wrs.status='failed' THEN 1 ELSE 0 END) AS failed_executions, \
                    COALESCE(CAST(SUM(CASE WHEN wrs.status='failed' THEN 1 ELSE 0 END) AS REAL) / NULLIF(COUNT(*), 0) * 100.0, 0.0) AS failure_rate, \
                    COALESCE(AVG(wrs.retry_count), 0.0) AS avg_retry_count \
             FROM workflow_runs wr \
             JOIN workflow_run_steps wrs ON wrs.workflow_run_id = wr.id \
             WHERE wr.workflow_name = ?1 \
               AND wr.status IN ('completed', 'failed') \
               AND wrs.status IN ('completed', 'failed') \
               AND wr.id IN ( \
                 SELECT id FROM workflow_runs \
                 WHERE workflow_name = ?1 AND status IN ('completed', 'failed') \
                 ORDER BY started_at DESC LIMIT ?2 \
               ) \
             GROUP BY wrs.step_name \
             ORDER BY failure_rate DESC, total_executions DESC",
        )?;
        let rows = stmt.query_map(params![workflow_name, limit_runs as i64], |row| {
            Ok(StepFailureHeatmapRow {
                step_name: row.get(0)?,
                total_executions: row.get(1)?,
                failed_executions: row.get(2)?,
                failure_rate: row.get(3)?,
                avg_retry_count: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Raw per-run metrics for completed runs of a workflow within the given day window.
    /// Returns one row per run with duration_ms, input_tokens, output_tokens.
    /// Binning happens client-side to avoid extra round-trips when switching metric toggles.
    pub fn get_run_metrics(
        &self,
        workflow_name: &str,
        days: u32,
    ) -> Result<Vec<WorkflowRunMetricsRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, started_at, total_duration_ms, total_input_tokens, total_output_tokens, worktree_id, repo_id \
             FROM workflow_runs \
             WHERE workflow_name = ?1 \
               AND status = 'completed' \
               AND started_at >= datetime('now', '-' || ?2 || ' days') \
               AND (COALESCE(total_input_tokens, 0) > 0 OR COALESCE(total_output_tokens, 0) > 0 OR COALESCE(total_duration_ms, 0) > 0) \
             ORDER BY started_at DESC",
        )?;
        let rows = stmt.query_map(params![workflow_name, days], |row| {
            Ok(WorkflowRunMetricsRow {
                run_id: row.get(0)?,
                started_at: row.get(1)?,
                duration_ms: row.get(2)?,
                input_tokens: row.get(3)?,
                output_tokens: row.get(4)?,
                worktree_id: row.get(5)?,
                repo_id: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Compute P50/P75/P95/P99 percentiles for duration, cost, and total tokens.
    ///
    /// Returns `None` when there are no qualifying completed runs with duration data.
    pub fn get_workflow_percentiles(
        &self,
        workflow_name: &str,
        days: u32,
    ) -> crate::error::Result<Option<WorkflowPercentiles>> {
        let mut stmt = self.conn.prepare_cached(
            "WITH ranked AS ( \
               SELECT \
                 total_duration_ms, \
                 total_cost_usd, \
                 (COALESCE(total_input_tokens, 0) + COALESCE(total_output_tokens, 0)) AS total_tokens, \
                 ROW_NUMBER() OVER (ORDER BY total_duration_ms)   AS rn_dur, \
                 ROW_NUMBER() OVER (ORDER BY total_cost_usd)      AS rn_cost, \
                 ROW_NUMBER() OVER (ORDER BY (COALESCE(total_input_tokens,0) + COALESCE(total_output_tokens,0))) AS rn_tok, \
                 COUNT(*) OVER () AS cnt \
               FROM workflow_runs \
               WHERE workflow_name = ?1 \
                 AND status = 'completed' \
                 AND started_at >= datetime('now', '-' || ?2 || ' days') \
                 AND total_duration_ms IS NOT NULL \
             ) \
             SELECT \
               AVG(CASE WHEN rn_dur  = (cnt * 50 + 99) / 100 THEN total_duration_ms END) AS p50_duration_ms, \
               AVG(CASE WHEN rn_dur  = (cnt * 75 + 99) / 100 THEN total_duration_ms END) AS p75_duration_ms, \
               AVG(CASE WHEN rn_dur  = (cnt * 95 + 99) / 100 THEN total_duration_ms END) AS p95_duration_ms, \
               AVG(CASE WHEN rn_dur  = (cnt * 99 + 99) / 100 THEN total_duration_ms END) AS p99_duration_ms, \
               AVG(CASE WHEN rn_cost = (cnt * 50 + 99) / 100 THEN total_cost_usd    END) AS p50_cost_usd, \
               AVG(CASE WHEN rn_cost = (cnt * 75 + 99) / 100 THEN total_cost_usd    END) AS p75_cost_usd, \
               AVG(CASE WHEN rn_cost = (cnt * 95 + 99) / 100 THEN total_cost_usd    END) AS p95_cost_usd, \
               AVG(CASE WHEN rn_cost = (cnt * 99 + 99) / 100 THEN total_cost_usd    END) AS p99_cost_usd, \
               AVG(CASE WHEN rn_tok  = (cnt * 50 + 99) / 100 THEN total_tokens      END) AS p50_total_tokens, \
               AVG(CASE WHEN rn_tok  = (cnt * 75 + 99) / 100 THEN total_tokens      END) AS p75_total_tokens, \
               AVG(CASE WHEN rn_tok  = (cnt * 95 + 99) / 100 THEN total_tokens      END) AS p95_total_tokens, \
               AVG(CASE WHEN rn_tok  = (cnt * 99 + 99) / 100 THEN total_tokens      END) AS p99_total_tokens, \
               MAX(cnt) AS run_count \
             FROM ranked",
        )?;
        let row = stmt.query_row(params![workflow_name, days], |row| {
            let run_count: Option<i64> = row.get(12)?;
            Ok((
                row.get::<_, Option<f64>>(0)?,
                row.get::<_, Option<f64>>(1)?,
                row.get::<_, Option<f64>>(2)?,
                row.get::<_, Option<f64>>(3)?,
                row.get::<_, Option<f64>>(4)?,
                row.get::<_, Option<f64>>(5)?,
                row.get::<_, Option<f64>>(6)?,
                row.get::<_, Option<f64>>(7)?,
                row.get::<_, Option<f64>>(8)?,
                row.get::<_, Option<f64>>(9)?,
                row.get::<_, Option<f64>>(10)?,
                row.get::<_, Option<f64>>(11)?,
                run_count,
            ))
        })?;
        let run_count = row.12.unwrap_or(0);
        if run_count == 0 {
            return Ok(None);
        }
        Ok(Some(WorkflowPercentiles {
            p50_duration_ms: row.0,
            p75_duration_ms: row.1,
            p95_duration_ms: row.2,
            p99_duration_ms: row.3,
            p50_cost_usd: row.4,
            p75_cost_usd: row.5,
            p95_cost_usd: row.6,
            p99_cost_usd: row.7,
            p50_total_tokens: row.8,
            p75_total_tokens: row.9,
            p95_total_tokens: row.10,
            p99_total_tokens: row.11,
            run_count,
        }))
    }
}
