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
    ActiveWorkflowCounts, PendingGateRow, WorkflowRun, WorkflowRunContext, WorkflowRunStep,
    WorkflowStepSummary,
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
            "SELECT {cols}, r.workflow_name, r.target_label, wt.branch, t.source_id AS ticket_ref \
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
