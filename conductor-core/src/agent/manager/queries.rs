use std::collections::HashMap;

use rusqlite::params;

use crate::db::query_collect;
use crate::error::Result;

use super::super::db::{row_to_agent_run, AGENT_RUN_SELECT};
use super::super::status::AgentRunStatus;
use super::super::types::AgentRun;
use super::AgentManager;

impl<'a> AgentManager<'a> {
    pub fn get_run(&self, run_id: &str) -> Result<Option<AgentRun>> {
        let result = self.conn.query_row(
            &format!("{AGENT_RUN_SELECT} WHERE id = ?1"),
            params![run_id],
            row_to_agent_run,
        );

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

    /// Batch-load multiple agent runs by ID in a single query.
    ///
    /// Returns a map from run ID → `AgentRun`. Missing IDs are silently skipped.
    /// Plan steps are **not** loaded (callers only need cost/turn/duration data).
    pub fn get_runs_by_ids(&self, ids: &[&str]) -> Result<HashMap<String, AgentRun>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        // Build "?1, ?2, …" in a single allocation sized up-front.
        let mut placeholders = String::with_capacity(ids.len() * 4);
        for i in 1..=ids.len() {
            if i > 1 {
                placeholders.push_str(", ");
            }
            placeholders.push('?');
            placeholders.push_str(&i.to_string());
        }
        let sql = format!("{AGENT_RUN_SELECT} WHERE id IN ({placeholders})");
        let params: Vec<&dyn rusqlite::types::ToSql> = ids
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(&*params, row_to_agent_run)?;
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
        // Cannot use AGENT_RUN_SELECT here: the JOIN requires the `a.` alias, and
        // col 15 is intentionally `NULL` for `plan` (populated separately via
        // `populate_plans` to avoid loading steps for every row in the JOIN).
        let mut runs = query_collect(
            self.conn,
            "SELECT a.id, a.worktree_id, a.claude_session_id, a.prompt, a.status, a.result_text, \
             a.cost_usd, a.num_turns, a.duration_ms, a.started_at, a.ended_at, a.tmux_window, \
             a.log_file, a.model, NULL, a.parent_run_id, \
             a.input_tokens, a.output_tokens, a.cache_read_input_tokens, a.cache_creation_input_tokens, \
             a.bot_name \
             FROM agent_runs a \
             JOIN worktrees w ON a.worktree_id = w.id \
             WHERE w.repo_id = ?1 \
             ORDER BY a.started_at DESC",
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

    /// Returns the latest agent run for each worktree, keyed by worktree_id.
    pub fn latest_runs_by_worktree(&self) -> Result<HashMap<String, AgentRun>> {
        let mut runs = query_collect(
            self.conn,
            "SELECT a.id, a.worktree_id, a.claude_session_id, a.prompt, a.status, \
             a.result_text, a.cost_usd, a.num_turns, a.duration_ms, a.started_at, \
             a.ended_at, a.tmux_window, a.log_file, a.model, a.plan, a.parent_run_id, \
             a.input_tokens, a.output_tokens, a.cache_read_input_tokens, a.cache_creation_input_tokens, bot_name \
             FROM agent_runs a \
             INNER JOIN ( \
                 SELECT worktree_id, MAX(started_at) AS max_started \
                 FROM agent_runs GROUP BY worktree_id \
             ) latest ON a.worktree_id = latest.worktree_id AND a.started_at = latest.max_started",
            [],
            row_to_agent_run,
        )?;
        self.populate_plans(&mut runs)?;
        let mut map = HashMap::new();
        for run in runs {
            if let Some(ref wt_id) = run.worktree_id {
                map.insert(wt_id.clone(), run);
            }
        }
        Ok(map)
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
            "WITH RECURSIVE tree(id) AS ( \
                 SELECT id FROM agent_runs WHERE id = ?1 \
                 UNION ALL \
                 SELECT a.id FROM agent_runs a JOIN tree t ON a.parent_run_id = t.id \
             ) \
             SELECT a.id, a.worktree_id, a.claude_session_id, a.prompt, a.status, \
                    a.result_text, a.cost_usd, a.num_turns, a.duration_ms, a.started_at, \
                    a.ended_at, a.tmux_window, a.log_file, a.model, a.plan, a.parent_run_id, \
                    a.input_tokens, a.output_tokens, a.cache_read_input_tokens, a.cache_creation_input_tokens, \
                    a.bot_name \
             FROM agent_runs a \
             JOIN tree t ON a.id = t.id \
             ORDER BY a.started_at ASC",
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
        // Column list with `ar.` alias for JOIN queries.
        const AR_COLS: &str = "ar.id, ar.worktree_id, ar.claude_session_id, ar.prompt, ar.status, \
             ar.result_text, ar.cost_usd, ar.num_turns, ar.duration_ms, ar.started_at, \
             ar.ended_at, ar.tmux_window, ar.log_file, ar.model, ar.plan, ar.parent_run_id, \
             ar.input_tokens, ar.output_tokens, ar.cache_read_input_tokens, \
             ar.cache_creation_input_tokens, ar.bot_name";

        let mut runs = match (worktree_id, repo_id, status) {
            // 1. worktree_id + status
            (Some(wt_id), _, Some(s)) => {
                let status_str = s.to_string();
                query_collect(
                    self.conn,
                    &format!(
                        "{AGENT_RUN_SELECT} WHERE worktree_id = ?1 AND status = ?2 \
                         ORDER BY started_at DESC LIMIT {limit} OFFSET {offset}"
                    ),
                    params![wt_id, status_str],
                    row_to_agent_run,
                )?
            }
            // 2. worktree_id only
            (Some(wt_id), _, None) => query_collect(
                self.conn,
                &format!(
                    "{AGENT_RUN_SELECT} WHERE worktree_id = ?1 \
                     ORDER BY started_at DESC LIMIT {limit} OFFSET {offset}"
                ),
                params![wt_id],
                row_to_agent_run,
            )?,
            // 3. repo_id + status
            (None, Some(r_id), Some(s)) => {
                let status_str = s.to_string();
                query_collect(
                    self.conn,
                    &format!(
                        "SELECT {AR_COLS} FROM agent_runs ar \
                         JOIN worktrees w ON w.id = ar.worktree_id \
                         WHERE w.repo_id = ?1 AND ar.status = ?2 \
                         ORDER BY ar.started_at DESC LIMIT {limit} OFFSET {offset}"
                    ),
                    params![r_id, status_str],
                    row_to_agent_run,
                )?
            }
            // 4. repo_id only
            (None, Some(r_id), None) => query_collect(
                self.conn,
                &format!(
                    "SELECT {AR_COLS} FROM agent_runs ar \
                     JOIN worktrees w ON w.id = ar.worktree_id \
                     WHERE w.repo_id = ?1 \
                     ORDER BY ar.started_at DESC LIMIT {limit} OFFSET {offset}"
                ),
                params![r_id],
                row_to_agent_run,
            )?,
            // 5. status only
            (None, None, Some(s)) => {
                let status_str = s.to_string();
                query_collect(
                    self.conn,
                    &format!(
                        "{AGENT_RUN_SELECT} WHERE status = ?1 \
                         ORDER BY started_at DESC LIMIT {limit} OFFSET {offset}"
                    ),
                    params![status_str],
                    row_to_agent_run,
                )?
            }
            // 6. no filter
            (None, None, None) => query_collect(
                self.conn,
                &format!(
                    "{AGENT_RUN_SELECT} \
                     ORDER BY started_at DESC LIMIT {limit} OFFSET {offset}"
                ),
                params![],
                row_to_agent_run,
            )?,
        };
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
            "INSERT INTO repos (id, slug, local_path, remote_url, default_branch, workspace_dir, created_at) \
             VALUES ('r2', 'other-repo', '/tmp/other', 'https://github.com/test/other.git', 'main', '/tmp/ws2', '2024-01-01T00:00:00Z')",
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
}
