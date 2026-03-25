use std::collections::HashMap;

use rusqlite::params;

use crate::db::query_collect;
use crate::error::Result;

use super::super::db::{
    row_to_agent_run, AGENT_RUN_COLS_A, AGENT_RUN_COLS_AR, AGENT_RUN_COLS_A_NULL_PLAN,
    AGENT_RUN_SELECT,
};
use super::super::status::AgentRunStatus;
use super::super::types::AgentRun;
use super::AgentManager;

impl<'a> AgentManager<'a> {
    /// Convert a single-row query result into `Ok(Some(run))`, loading plan steps,
    /// or `Ok(None)` on `QueryReturnedNoRows`.  Centralises the 3-arm match that
    /// `get_run` and `latest_for_worktree` previously inlined identically.
    fn load_optional_run(
        &self,
        result: std::result::Result<AgentRun, rusqlite::Error>,
    ) -> Result<Option<AgentRun>> {
        match result {
            Ok(mut run) => {
                let steps = self.get_run_steps(&run.id)?;
                run.plan = if steps.is_empty() { None } else { Some(steps) };
                Ok(Some(run))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_run(&self, run_id: &str) -> Result<Option<AgentRun>> {
        let result = self.conn.query_row(
            &format!("{AGENT_RUN_SELECT} WHERE id = ?1"),
            params![run_id],
            row_to_agent_run,
        );
        self.load_optional_run(result)
    }

    /// Batch-load multiple agent runs by ID in a single query.
    ///
    /// Returns a map from run ID → `AgentRun`. Missing IDs are silently skipped.
    /// Plan steps are **not** loaded (callers only need cost/turn/duration data).
    pub fn get_runs_by_ids(&self, ids: &[&str]) -> Result<HashMap<String, AgentRun>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = crate::db::sql_placeholders(ids.len());
        let sql = format!("{AGENT_RUN_SELECT} WHERE id IN ({placeholders})");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), row_to_agent_run)?;
        let mut map = HashMap::new();
        for row in rows {
            let run = row?;
            map.insert(run.id.clone(), run);
        }
        Ok(map)
    }

    pub fn list_for_worktree(&self, worktree_id: &str) -> Result<Vec<AgentRun>> {
        let mut runs = query_collect(
            self.conn,
            &format!("{AGENT_RUN_SELECT} WHERE worktree_id = ?1 ORDER BY started_at DESC"),
            params![worktree_id],
            row_to_agent_run,
        )?;
        self.populate_plans(&mut runs)?;
        Ok(runs)
    }

    /// List all agent runs for a repo (across all its worktrees), newest first.
    pub fn list_for_repo(&self, repo_id: &str) -> Result<Vec<AgentRun>> {
        // Cannot use AGENT_RUN_SELECT here: the JOIN requires the `a.` alias.
        // NULL for plan is intentional — populated separately via `populate_plans`
        // to avoid loading steps for every row in the JOIN.
        let mut runs = query_collect(
            self.conn,
            &format!(
                "SELECT {AGENT_RUN_COLS_A_NULL_PLAN} \
                 FROM agent_runs a \
                 JOIN worktrees w ON a.worktree_id = w.id \
                 WHERE w.repo_id = ?1 \
                 ORDER BY a.started_at DESC"
            ),
            params![repo_id],
            row_to_agent_run,
        )?;
        self.populate_plans(&mut runs)?;
        Ok(runs)
    }

    /// Returns true if the worktree has any prior agent runs.
    pub fn has_runs_for_worktree(&self, worktree_id: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM agent_runs WHERE worktree_id = ?1",
            params![worktree_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn latest_for_worktree(&self, worktree_id: &str) -> Result<Option<AgentRun>> {
        let result = self.conn.query_row(
            &format!("{AGENT_RUN_SELECT} WHERE worktree_id = ?1 ORDER BY started_at DESC LIMIT 1"),
            params![worktree_id],
            row_to_agent_run,
        );
        self.load_optional_run(result)
    }

    /// Convert a list of runs into a map keyed by worktree_id, populating plans.
    fn runs_to_worktree_map(&self, mut runs: Vec<AgentRun>) -> Result<HashMap<String, AgentRun>> {
        self.populate_plans(&mut runs)?;
        let mut map = HashMap::new();
        for run in runs {
            if let Some(ref wt_id) = run.worktree_id {
                map.insert(wt_id.clone(), run);
            }
        }
        Ok(map)
    }

    /// Returns the latest agent run for each worktree, keyed by worktree_id.
    pub fn latest_runs_by_worktree(&self) -> Result<HashMap<String, AgentRun>> {
        let runs = query_collect(
            self.conn,
            &format!(
                "SELECT {AGENT_RUN_COLS_A} \
                 FROM agent_runs a \
                 INNER JOIN ( \
                     SELECT worktree_id, MAX(started_at) AS max_started \
                     FROM agent_runs GROUP BY worktree_id \
                 ) latest ON a.worktree_id = latest.worktree_id AND a.started_at = latest.max_started"
            ),
            [],
            row_to_agent_run,
        )?;
        self.runs_to_worktree_map(runs)
    }

    /// Returns the latest agent run for each worktree belonging to a specific repo,
    /// keyed by worktree_id.
    pub fn latest_runs_by_worktree_for_repo(
        &self,
        repo_id: &str,
    ) -> Result<HashMap<String, AgentRun>> {
        let runs = query_collect(
            self.conn,
            &format!(
                "SELECT {AGENT_RUN_COLS_A} \
                 FROM agent_runs a \
                 INNER JOIN ( \
                     SELECT ar.worktree_id, MAX(ar.started_at) AS max_started \
                     FROM agent_runs ar \
                     JOIN worktrees w ON ar.worktree_id = w.id \
                     WHERE w.repo_id = ?1 \
                     GROUP BY ar.worktree_id \
                 ) latest ON a.worktree_id = latest.worktree_id AND a.started_at = latest.max_started"
            ),
            params![repo_id],
            row_to_agent_run,
        )?;
        self.runs_to_worktree_map(runs)
    }

    /// Returns the latest top-level agent run for a single worktree, or `None` if none exist.
    ///
    /// `parent_run_id IS NULL` filters to top-level runs — sub-agent child runs are excluded.
    pub fn latest_run_for_worktree(&self, worktree_id: &str) -> Result<Option<AgentRun>> {
        let mut runs = query_collect(
            self.conn,
            &format!(
                "{AGENT_RUN_SELECT} \
                 WHERE worktree_id = ?1 AND parent_run_id IS NULL \
                 ORDER BY started_at DESC \
                 LIMIT 1"
            ),
            params![worktree_id],
            row_to_agent_run,
        )?;
        self.populate_plans(&mut runs)?;
        Ok(runs.into_iter().next())
    }

    // ── Parent/child run tree queries ─────────────────────────────────

    /// List direct child runs of a parent run (newest first).
    pub fn list_child_runs(&self, parent_run_id: &str) -> Result<Vec<AgentRun>> {
        let mut runs = query_collect(
            self.conn,
            &format!("{AGENT_RUN_SELECT} WHERE parent_run_id = ?1 ORDER BY started_at DESC"),
            params![parent_run_id],
            row_to_agent_run,
        )?;
        self.populate_plans(&mut runs)?;
        Ok(runs)
    }

    /// Get a full run tree: the given run plus all descendants (children, grandchildren, etc.).
    /// Returns a flat list ordered by started_at ASC. The caller can reconstruct
    /// the tree using `parent_run_id` references.
    pub fn get_run_tree(&self, root_run_id: &str) -> Result<Vec<AgentRun>> {
        // SQLite supports recursive CTEs, which are perfect for tree traversal.
        // Cannot use AGENT_RUN_SELECT here: the CTE requires the `a.` alias throughout.
        let mut runs = query_collect(
            self.conn,
            &format!(
                "WITH RECURSIVE tree(id) AS ( \
                     SELECT id FROM agent_runs WHERE id = ?1 \
                     UNION ALL \
                     SELECT a.id FROM agent_runs a JOIN tree t ON a.parent_run_id = t.id \
                 ) \
                 SELECT {AGENT_RUN_COLS_A} \
                 FROM agent_runs a \
                 JOIN tree t ON a.id = t.id \
                 ORDER BY a.started_at ASC"
            ),
            params![root_run_id],
            row_to_agent_run,
        )?;
        self.populate_plans(&mut runs)?;
        Ok(runs)
    }

    /// List only top-level (root) runs for a worktree — runs with no parent.
    pub fn list_root_runs_for_worktree(&self, worktree_id: &str) -> Result<Vec<AgentRun>> {
        let mut runs = query_collect(
            self.conn,
            &format!(
                "{AGENT_RUN_SELECT} WHERE worktree_id = ?1 AND parent_run_id IS NULL \
                 ORDER BY started_at DESC"
            ),
            params![worktree_id],
            row_to_agent_run,
        )?;
        self.populate_plans(&mut runs)?;
        Ok(runs)
    }

    /// List agent runs with optional filters, ordered by `started_at DESC`.
    ///
    /// Filtering dimensions (caller ensures `worktree_id` and `repo_id` are
    /// mutually exclusive):
    /// - `worktree_id` — direct filter on `agent_runs.worktree_id`
    /// - `repo_id` — requires JOIN with `worktrees` on `w.repo_id`
    /// - `status` — filter on `agent_runs.status`
    /// - `limit` / `offset` — pagination
    pub fn list_agent_runs(
        &self,
        worktree_id: Option<&str>,
        repo_id: Option<&str>,
        status: Option<&AgentRunStatus>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<AgentRun>> {
        // repo_id filter requires a JOIN; worktree_id/status use the plain SELECT.
        let use_join = repo_id.is_some();
        let sql_base = if use_join {
            format!(
                "SELECT {AGENT_RUN_COLS_AR} FROM agent_runs ar \
                 JOIN worktrees w ON w.id = ar.worktree_id"
            )
        } else {
            AGENT_RUN_SELECT.to_string()
        };

        // Accumulate WHERE conditions and parameter values in lock-step.
        let mut where_parts: Vec<String> = Vec::new();
        let mut param_values: Vec<String> = Vec::new();

        if let Some(wt_id) = worktree_id {
            param_values.push(wt_id.to_owned());
            where_parts.push("worktree_id = ?".to_string());
        } else if let Some(r_id) = repo_id {
            param_values.push(r_id.to_owned());
            where_parts.push("w.repo_id = ?".to_string());
        }

        if let Some(s) = status {
            param_values.push(s.to_string());
            let col = if use_join { "ar.status" } else { "status" };
            where_parts.push(format!("{col} = ?"));
        }

        let where_clause = if where_parts.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", where_parts.join(" AND "))
        };
        let order_col = if use_join {
            "ar.started_at"
        } else {
            "started_at"
        };

        let sql = format!(
            "{sql_base}{where_clause} ORDER BY {order_col} DESC LIMIT {limit} OFFSET {offset}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(param_values.iter()),
            row_to_agent_run,
        )?;
        let mut runs: Vec<AgentRun> = rows.collect::<rusqlite::Result<_>>()?;
        self.populate_plans(&mut runs)?;
        Ok(runs)
    }
}

#[cfg(test)]
mod tests {
    use super::super::setup_db;
    use super::super::AgentManager;
    use crate::agent::status::AgentRunStatus;

    #[test]
    fn test_get_run() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.id, run.id);
        assert_eq!(fetched.prompt, "Fix the bug");

        let missing = mgr.get_run("nonexistent").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_latest_for_worktree_empty() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let result = mgr.latest_for_worktree("w1").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_has_runs_for_worktree() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // No runs yet
        assert!(!mgr.has_runs_for_worktree("w1").unwrap());

        // Create a run
        mgr.create_run(Some("w1"), "First prompt", None, None)
            .unwrap();
        assert!(mgr.has_runs_for_worktree("w1").unwrap());

        // Different worktree still has no runs
        assert!(!mgr.has_runs_for_worktree("w2").unwrap());
    }

    #[test]
    fn test_latest_runs_by_worktree() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Create runs for two different worktrees
        let _run1 = mgr
            .create_run(Some("w1"), "First prompt", None, None)
            .unwrap();
        let run2 = mgr
            .create_run(Some("w1"), "Second prompt", None, None)
            .unwrap();
        let run3 = mgr
            .create_run(Some("w2"), "Other prompt", None, None)
            .unwrap();

        let map = mgr.latest_runs_by_worktree().unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("w1").unwrap().id, run2.id);
        assert_eq!(map.get("w2").unwrap().id, run3.id);
    }

    #[test]
    fn test_latest_runs_by_worktree_excludes_none_worktree() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Create an ephemeral run with no worktree_id
        mgr.create_run(None, "ephemeral prompt", None, None)
            .unwrap();
        // Create a run with a real worktree_id
        let run_w1 = mgr
            .create_run(Some("w1"), "real prompt", None, None)
            .unwrap();

        let map = mgr.latest_runs_by_worktree().unwrap();
        // Only the run with a worktree_id should appear
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("w1").unwrap().id, run_w1.id);
        // Ephemeral run must not appear under any key
        assert!(!map.contains_key(""));
    }

    #[test]
    fn test_create_child_run() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let parent = mgr
            .create_run(Some("w1"), "Supervisor task", None, None)
            .unwrap();
        assert!(parent.parent_run_id.is_none());

        let child = mgr
            .create_child_run(Some("w1"), "Sub-task A", None, None, &parent.id, None)
            .unwrap();
        assert_eq!(child.parent_run_id.as_deref(), Some(parent.id.as_str()));

        let fetched = mgr.get_run(&child.id).unwrap().unwrap();
        assert_eq!(fetched.parent_run_id.as_deref(), Some(parent.id.as_str()));
    }

    #[test]
    fn test_list_child_runs() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let parent = mgr
            .create_run(Some("w1"), "Supervisor", None, None)
            .unwrap();
        let _child1 = mgr
            .create_child_run(Some("w1"), "Child 1", None, None, &parent.id, None)
            .unwrap();
        let _child2 = mgr
            .create_child_run(Some("w1"), "Child 2", None, None, &parent.id, None)
            .unwrap();

        // Unrelated run should not appear
        let _other = mgr
            .create_run(Some("w1"), "Independent", None, None)
            .unwrap();

        let children = mgr.list_child_runs(&parent.id).unwrap();
        assert_eq!(children.len(), 2);
        assert!(children
            .iter()
            .all(|c| c.parent_run_id.as_deref() == Some(parent.id.as_str())));
    }

    #[test]
    fn test_get_run_tree() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Build a tree: parent -> child1, child2 -> grandchild
        let parent = mgr.create_run(Some("w1"), "Root task", None, None).unwrap();
        let child1 = mgr
            .create_child_run(Some("w1"), "Child 1", None, None, &parent.id, None)
            .unwrap();
        let _child2 = mgr
            .create_child_run(Some("w2"), "Child 2", None, None, &parent.id, None)
            .unwrap();
        let _grandchild = mgr
            .create_child_run(Some("w1"), "Grandchild", None, None, &child1.id, None)
            .unwrap();

        // Unrelated run
        let _other = mgr.create_run(Some("w1"), "Other", None, None).unwrap();

        let tree = mgr.get_run_tree(&parent.id).unwrap();
        assert_eq!(tree.len(), 4); // parent + 2 children + 1 grandchild
        assert_eq!(tree[0].id, parent.id); // root is first (earliest started_at)
    }

    #[test]
    fn test_list_root_runs_for_worktree() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let parent = mgr
            .create_run(Some("w1"), "Supervisor", None, None)
            .unwrap();
        let _child = mgr
            .create_child_run(Some("w1"), "Child", None, None, &parent.id, None)
            .unwrap();
        let standalone = mgr
            .create_run(Some("w1"), "Standalone", None, None)
            .unwrap();

        let root_runs = mgr.list_root_runs_for_worktree("w1").unwrap();
        assert_eq!(root_runs.len(), 2);
        // Newest first
        assert_eq!(root_runs[0].id, standalone.id);
        assert_eq!(root_runs[1].id, parent.id);
        assert!(root_runs.iter().all(|r| r.parent_run_id.is_none()));
    }

    #[test]
    fn test_parent_run_id_set_null_on_delete() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let parent = mgr.create_run(Some("w1"), "Parent", None, None).unwrap();
        let child = mgr
            .create_child_run(Some("w1"), "Child", None, None, &parent.id, None)
            .unwrap();

        // Delete the parent — ON DELETE SET NULL should clear child's parent_run_id
        conn.execute(
            "DELETE FROM agent_runs WHERE id = ?1",
            rusqlite::params![parent.id],
        )
        .unwrap();

        let fetched = mgr.get_run(&child.id).unwrap().unwrap();
        assert!(fetched.parent_run_id.is_none());
    }

    #[test]
    fn test_get_runs_by_ids_returns_matching() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let r1 = mgr.create_run(Some("w1"), "Task 1", None, None).unwrap();
        let r2 = mgr.create_run(Some("w1"), "Task 2", None, None).unwrap();
        let r3 = mgr.create_run(Some("w1"), "Task 3", None, None).unwrap();

        let ids = vec![r1.id.as_str(), r2.id.as_str()];
        let result = mgr.get_runs_by_ids(&ids).unwrap();

        assert_eq!(result.len(), 2);
        assert!(result.contains_key(&r1.id));
        assert!(result.contains_key(&r2.id));
        assert!(!result.contains_key(&r3.id));
        assert_eq!(result[&r1.id].prompt, "Task 1");
        assert_eq!(result[&r2.id].prompt, "Task 2");
    }

    #[test]
    fn test_get_runs_by_ids_empty_input() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let _r1 = mgr.create_run(Some("w1"), "Task 1", None, None).unwrap();

        let result = mgr.get_runs_by_ids(&[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_get_runs_by_ids_missing_ids_skipped() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let r1 = mgr.create_run(Some("w1"), "Task 1", None, None).unwrap();

        let ids = vec![r1.id.as_str(), "nonexistent-id-xyz"];
        let result = mgr.get_runs_by_ids(&ids).unwrap();

        assert_eq!(result.len(), 1);
        assert!(result.contains_key(&r1.id));
        assert!(!result.contains_key("nonexistent-id-xyz"));
    }

    #[test]
    fn test_list_agent_runs_no_filter() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let r1 = mgr.create_run(Some("w1"), "Task 1", None, None).unwrap();
        let r2 = mgr.create_run(Some("w1"), "Task 2", None, None).unwrap();

        let runs = mgr.list_agent_runs(None, None, None, 50, 0).unwrap();
        assert_eq!(runs.len(), 2);
        // newest first
        assert_eq!(runs[0].id, r2.id);
        assert_eq!(runs[1].id, r1.id);
    }

    #[test]
    fn test_list_agent_runs_worktree_filter() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // w1 and w2 are both seeded; create runs in each
        let r1 = mgr
            .create_run(Some("w1"), "Task in w1", None, None)
            .unwrap();
        let _r2 = mgr
            .create_run(Some("w2"), "Task in w2", None, None)
            .unwrap();

        let runs = mgr.list_agent_runs(Some("w1"), None, None, 50, 0).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, r1.id);
    }

    #[test]
    fn test_list_agent_runs_worktree_and_status_filter() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let r1 = mgr.create_run(Some("w1"), "Task 1", None, None).unwrap();
        let r2 = mgr.create_run(Some("w1"), "Task 2", None, None).unwrap();
        mgr.update_run_completed(
            &r2.id,
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

        let running = mgr
            .list_agent_runs(Some("w1"), None, Some(&AgentRunStatus::Running), 50, 0)
            .unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].id, r1.id);

        let completed = mgr
            .list_agent_runs(Some("w1"), None, Some(&AgentRunStatus::Completed), 50, 0)
            .unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].id, r2.id);
    }

    #[test]
    fn test_list_agent_runs_repo_filter() {
        let conn = setup_db();
        // setup_db inserts w1 (repo_id='r1') and w2 (repo_id='r1').
        // Insert a second repo with its own worktree.
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
             VALUES ('r2', 'other-repo', '/tmp/other', 'https://github.com/test/other.git', '/tmp/ws2', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w3', 'r2', 'feat-other', 'feat/other', '/tmp/ws2/other', 'active', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();

        let mgr = AgentManager::new(&conn);
        let r1 = mgr.create_run(Some("w1"), "r1 task", None, None).unwrap();
        let _r3 = mgr.create_run(Some("w3"), "r2 task", None, None).unwrap();

        let runs = mgr.list_agent_runs(None, Some("r1"), None, 50, 0).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, r1.id);
    }

    #[test]
    fn test_list_agent_runs_repo_and_status_filter() {
        let conn = setup_db();
        // setup_db inserts w1 (repo_id='r1') and w2 (repo_id='r1').
        // Insert a second repo with its own worktree.
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
             VALUES ('r2', 'other-repo', '/tmp/other', 'https://github.com/test/other.git', '/tmp/ws2', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w3', 'r2', 'feat-other', 'feat/other', '/tmp/ws2/other', 'active', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();

        let mgr = AgentManager::new(&conn);
        let r1_running = mgr
            .create_run(Some("w1"), "r1 running task", None, None)
            .unwrap();
        let r1_completed = mgr
            .create_run(Some("w1"), "r1 completed task", None, None)
            .unwrap();
        let _r2_running = mgr
            .create_run(Some("w3"), "r2 running task", None, None)
            .unwrap();

        mgr.update_run_completed(
            &r1_completed.id,
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

        // repo_id=r1 + status=Running → only r1_running
        let running = mgr
            .list_agent_runs(None, Some("r1"), Some(&AgentRunStatus::Running), 50, 0)
            .unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].id, r1_running.id);

        // repo_id=r1 + status=Completed → only r1_completed
        let completed = mgr
            .list_agent_runs(None, Some("r1"), Some(&AgentRunStatus::Completed), 50, 0)
            .unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].id, r1_completed.id);

        // repo_id=r2 + status=Running → only r2's run (excludes r1 runs)
        let r2_running = mgr
            .list_agent_runs(None, Some("r2"), Some(&AgentRunStatus::Running), 50, 0)
            .unwrap();
        assert_eq!(r2_running.len(), 1);

        // repo_id=r2 + status=Completed → nothing (r2 has no completed runs)
        let r2_completed = mgr
            .list_agent_runs(None, Some("r2"), Some(&AgentRunStatus::Completed), 50, 0)
            .unwrap();
        assert_eq!(r2_completed.len(), 0);
    }

    #[test]
    fn test_list_agent_runs_status_only_filter() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let r1 = mgr.create_run(Some("w1"), "Task 1", None, None).unwrap();
        let r2 = mgr.create_run(Some("w1"), "Task 2", None, None).unwrap();
        mgr.update_run_completed(
            &r1.id,
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

        let running = mgr
            .list_agent_runs(None, None, Some(&AgentRunStatus::Running), 50, 0)
            .unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].id, r2.id);
    }

    #[test]
    fn test_list_agent_runs_pagination() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        for i in 0..5 {
            mgr.create_run(Some("w1"), &format!("Task {i}"), None, None)
                .unwrap();
        }

        let page1 = mgr.list_agent_runs(None, None, None, 3, 0).unwrap();
        assert_eq!(page1.len(), 3);

        let page2 = mgr.list_agent_runs(None, None, None, 3, 3).unwrap();
        assert_eq!(page2.len(), 2);
    }

    #[test]
    fn test_list_for_repo() {
        let conn = setup_db();
        // setup_db seeds r1 with w1 and w2; insert a second repo with its own worktree.
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
             VALUES ('r2', 'other-repo', '/tmp/other', 'https://github.com/test/other.git', '/tmp/ws2', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w3', 'r2', 'feat-other', 'feat/other', '/tmp/ws2/other', 'active', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();

        let mgr = AgentManager::new(&conn);
        let r1a = mgr
            .create_run(Some("w1"), "Task for r1 w1", None, None)
            .unwrap();
        let r1b = mgr
            .create_run(Some("w2"), "Task for r1 w2", None, None)
            .unwrap();
        let _r2 = mgr
            .create_run(Some("w3"), "Task for r2", None, None)
            .unwrap();

        let runs = mgr.list_for_repo("r1").unwrap();
        assert_eq!(runs.len(), 2);
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&r1a.id.as_str()));
        assert!(ids.contains(&r1b.id.as_str()));

        let r2_runs = mgr.list_for_repo("r2").unwrap();
        assert_eq!(r2_runs.len(), 1);
    }

    #[test]
    fn test_agent_run_bot_name_non_null_round_trip() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let parent = mgr.create_run(Some("w1"), "Parent", None, None).unwrap();
        let run = mgr
            .create_child_run(Some("w1"), "Task", None, None, &parent.id, Some("my-bot"))
            .unwrap();
        assert_eq!(run.bot_name.as_deref(), Some("my-bot"));

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(
            fetched.bot_name.as_deref(),
            Some("my-bot"),
            "bot_name should round-trip through the DB unchanged"
        );
    }

    #[test]
    fn test_latest_run_for_worktree_excludes_child_runs() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Create a top-level parent run
        let parent = mgr
            .create_run(Some("w1"), "Parent task", None, None)
            .unwrap();

        // Create a child run on the same worktree (simulates a sub-agent)
        let child = mgr
            .create_child_run(Some("w1"), "Child task", None, None, &parent.id, None)
            .unwrap();

        // latest_run_for_worktree should return only the parent (top-level) run,
        // not the child, because it filters with `parent_run_id IS NULL`.
        let latest = mgr.latest_run_for_worktree("w1").unwrap().unwrap();
        assert_eq!(
            latest.id, parent.id,
            "should return the top-level run, not the child run"
        );
        assert_ne!(
            latest.id, child.id,
            "child run must be excluded by the parent_run_id IS NULL filter"
        );
    }

    #[test]
    fn test_latest_run_for_worktree_returns_newest_top_level() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Create two top-level runs for the same worktree
        let _older = mgr
            .create_run(Some("w1"), "Older task", None, None)
            .unwrap();
        let newer = mgr
            .create_run(Some("w1"), "Newer task", None, None)
            .unwrap();

        // Also create a child of the newer run
        let _child = mgr
            .create_child_run(Some("w1"), "Child of newer", None, None, &newer.id, None)
            .unwrap();

        let latest = mgr.latest_run_for_worktree("w1").unwrap().unwrap();
        assert_eq!(
            latest.id, newer.id,
            "should return the newest top-level run"
        );
    }

    #[test]
    fn test_latest_run_for_worktree_empty() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let result = mgr.latest_run_for_worktree("w1").unwrap();
        assert!(result.is_none(), "empty worktree should return None");
    }

    #[test]
    fn test_latest_run_for_worktree_only_child_runs_returns_none() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Create a parent run on w1
        let parent = mgr
            .create_run(Some("w1"), "Parent task", None, None)
            .unwrap();

        // Create a child run on w2, parented to the w1 run
        let _child = mgr
            .create_child_run(Some("w2"), "Child task", None, None, &parent.id, None)
            .unwrap();

        // w2 only has a child run — parent_run_id IS NULL filter should exclude it
        let result = mgr.latest_run_for_worktree("w2").unwrap();
        assert!(
            result.is_none(),
            "worktree with only child runs should return None"
        );
    }

    #[test]
    fn test_latest_for_worktree_vs_latest_run_for_worktree_child_only() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Create a parent run on w1
        let parent = mgr
            .create_run(Some("w1"), "Parent task", None, None)
            .unwrap();

        // Create a child run on w2, parented to the w1 run
        let child = mgr
            .create_child_run(Some("w2"), "Child task", None, None, &parent.id, None)
            .unwrap();

        // latest_for_worktree sees all runs — returns the child
        let any_latest = mgr.latest_for_worktree("w2").unwrap();
        assert_eq!(
            any_latest.as_ref().map(|r| &r.id),
            Some(&child.id),
            "latest_for_worktree should return the child run"
        );

        // latest_run_for_worktree filters to parent_run_id IS NULL — returns None
        let top_level_latest = mgr.latest_run_for_worktree("w2").unwrap();
        assert!(
            top_level_latest.is_none(),
            "latest_run_for_worktree should return None when only child runs exist"
        );
    }

    #[test]
    fn test_latest_for_worktree_vs_latest_run_for_worktree_newest_is_child() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Create a top-level parent run on w1
        let parent = mgr
            .create_run(Some("w1"), "Parent task", None, None)
            .unwrap();

        // Create a child run on w1 (newer) parented to the same parent
        let child = mgr
            .create_child_run(Some("w1"), "Child task", None, None, &parent.id, None)
            .unwrap();

        // latest_for_worktree sees all runs — returns the child (newest)
        let any_latest = mgr.latest_for_worktree("w1").unwrap().unwrap();
        assert_eq!(
            any_latest.id, child.id,
            "latest_for_worktree should return the newest run (child)"
        );

        // latest_run_for_worktree filters to top-level only — returns the parent
        let top_level_latest = mgr.latest_run_for_worktree("w1").unwrap().unwrap();
        assert_eq!(
            top_level_latest.id, parent.id,
            "latest_run_for_worktree should return the parent (newest top-level)"
        );

        // The two functions return different runs on the same worktree
        assert_ne!(
            any_latest.id, top_level_latest.id,
            "the two functions must diverge when newest run is a child"
        );
    }

    #[test]
    fn test_latest_runs_by_worktree_for_repo() {
        let conn = setup_db();
        // setup_db seeds r1 with w1 and w2; insert a second repo with its own worktree.
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
             VALUES ('r2', 'other-repo', '/tmp/other', 'https://github.com/test/other.git', '/tmp/ws2', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w3', 'r2', 'feat-other', 'feat/other', '/tmp/ws2/other', 'active', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();

        let mgr = AgentManager::new(&conn);

        // Create runs across repos
        let _r1_old = mgr
            .create_run(Some("w1"), "Old r1 task", None, None)
            .unwrap();
        let r1_new = mgr
            .create_run(Some("w1"), "New r1 task", None, None)
            .unwrap();
        let r1_w2 = mgr
            .create_run(Some("w2"), "r1 w2 task", None, None)
            .unwrap();
        let _r2 = mgr.create_run(Some("w3"), "r2 task", None, None).unwrap();

        // Repo r1 should only include w1 and w2
        let map = mgr.latest_runs_by_worktree_for_repo("r1").unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("w1").unwrap().id, r1_new.id);
        assert_eq!(map.get("w2").unwrap().id, r1_w2.id);
        assert!(!map.contains_key("w3"));

        // Repo r2 should only include w3
        let map_r2 = mgr.latest_runs_by_worktree_for_repo("r2").unwrap();
        assert_eq!(map_r2.len(), 1);
        assert!(map_r2.contains_key("w3"));
    }
}
