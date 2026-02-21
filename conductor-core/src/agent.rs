use std::collections::HashMap;

use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRun {
    pub id: String,
    pub worktree_id: String,
    pub claude_session_id: Option<String>,
    pub prompt: String,
    pub status: String,
    pub result_text: Option<String>,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub duration_ms: Option<i64>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub tmux_window: Option<String>,
}

/// Parsed JSON result from `claude -p --output-format json`.
#[derive(Debug, Deserialize)]
pub struct ClaudeJsonResult {
    pub session_id: Option<String>,
    pub result: Option<String>,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub duration_ms: Option<i64>,
    pub is_error: Option<bool>,
}

pub struct AgentManager<'a> {
    conn: &'a Connection,
}

impl<'a> AgentManager<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn create_run(
        &self,
        worktree_id: &str,
        prompt: &str,
        tmux_window: Option<&str>,
    ) -> Result<AgentRun> {
        let id = ulid::Ulid::new().to_string();
        let now = Utc::now().to_rfc3339();

        let run = AgentRun {
            id: id.clone(),
            worktree_id: worktree_id.to_string(),
            claude_session_id: None,
            prompt: prompt.to_string(),
            status: "running".to_string(),
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            started_at: now.clone(),
            ended_at: None,
            tmux_window: tmux_window.map(String::from),
        };

        self.conn.execute(
            "INSERT INTO agent_runs (id, worktree_id, prompt, status, started_at, tmux_window) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                run.id,
                run.worktree_id,
                run.prompt,
                run.status,
                run.started_at,
                run.tmux_window
            ],
        )?;

        Ok(run)
    }

    pub fn get_run(&self, run_id: &str) -> Result<Option<AgentRun>> {
        let result = self.conn.query_row(
            "SELECT id, worktree_id, claude_session_id, prompt, status, result_text, \
             cost_usd, num_turns, duration_ms, started_at, ended_at, tmux_window \
             FROM agent_runs WHERE id = ?1",
            params![run_id],
            row_to_agent_run,
        );

        match result {
            Ok(run) => Ok(Some(run)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn update_run_completed(
        &self,
        run_id: &str,
        session_id: Option<&str>,
        result_text: Option<&str>,
        cost_usd: Option<f64>,
        num_turns: Option<i64>,
        duration_ms: Option<i64>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE agent_runs SET status = 'completed', claude_session_id = ?1, \
             result_text = ?2, cost_usd = ?3, num_turns = ?4, duration_ms = ?5, \
             ended_at = ?6 WHERE id = ?7",
            params![
                session_id,
                result_text,
                cost_usd,
                num_turns,
                duration_ms,
                now,
                run_id
            ],
        )?;
        Ok(())
    }

    pub fn update_run_failed(&self, run_id: &str, error: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE agent_runs SET status = 'failed', result_text = ?1, ended_at = ?2 \
             WHERE id = ?3",
            params![error, now, run_id],
        )?;
        Ok(())
    }

    pub fn update_run_cancelled(&self, run_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE agent_runs SET status = 'cancelled', ended_at = ?1 WHERE id = ?2",
            params![now, run_id],
        )?;
        Ok(())
    }

    pub fn list_for_worktree(&self, worktree_id: &str) -> Result<Vec<AgentRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, worktree_id, claude_session_id, prompt, status, result_text, \
             cost_usd, num_turns, duration_ms, started_at, ended_at, tmux_window \
             FROM agent_runs WHERE worktree_id = ?1 ORDER BY started_at DESC",
        )?;
        let rows = stmt.query_map(params![worktree_id], row_to_agent_run)?;
        let runs = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(runs)
    }

    pub fn latest_for_worktree(&self, worktree_id: &str) -> Result<Option<AgentRun>> {
        let result = self.conn.query_row(
            "SELECT id, worktree_id, claude_session_id, prompt, status, result_text, \
             cost_usd, num_turns, duration_ms, started_at, ended_at, tmux_window \
             FROM agent_runs WHERE worktree_id = ?1 ORDER BY started_at DESC LIMIT 1",
            params![worktree_id],
            row_to_agent_run,
        );

        match result {
            Ok(run) => Ok(Some(run)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Returns the latest agent run for each worktree, keyed by worktree_id.
    pub fn latest_runs_by_worktree(&self) -> Result<HashMap<String, AgentRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.worktree_id, a.claude_session_id, a.prompt, a.status, \
             a.result_text, a.cost_usd, a.num_turns, a.duration_ms, a.started_at, \
             a.ended_at, a.tmux_window \
             FROM agent_runs a \
             INNER JOIN ( \
                 SELECT worktree_id, MAX(started_at) AS max_started \
                 FROM agent_runs GROUP BY worktree_id \
             ) latest ON a.worktree_id = latest.worktree_id AND a.started_at = latest.max_started",
        )?;

        let rows = stmt.query_map([], row_to_agent_run)?;
        let mut map = HashMap::new();
        for run in rows {
            let run = run?;
            map.insert(run.worktree_id.clone(), run);
        }
        Ok(map)
    }
}

fn row_to_agent_run(row: &rusqlite::Row) -> rusqlite::Result<AgentRun> {
    Ok(AgentRun {
        id: row.get(0)?,
        worktree_id: row.get(1)?,
        claude_session_id: row.get(2)?,
        prompt: row.get(3)?,
        status: row.get(4)?,
        result_text: row.get(5)?,
        cost_usd: row.get(6)?,
        num_turns: row.get(7)?,
        duration_ms: row.get(8)?,
        started_at: row.get(9)?,
        ended_at: row.get(10)?,
        tmux_window: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use rusqlite::Connection;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        db::migrations::run(&conn).unwrap();
        // Insert a repo and worktree for FK constraints
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, default_branch, workspace_dir, created_at) \
             VALUES ('r1', 'test-repo', '/tmp/repo', 'https://github.com/test/repo.git', 'main', '/tmp/ws', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w1', 'r1', 'feat-test', 'feat/test', '/tmp/ws/feat-test', 'active', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w2', 'r1', 'fix-bug', 'fix/bug', '/tmp/ws/fix-bug', 'active', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn
    }

    #[test]
    fn test_create_and_list() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None).unwrap();
        assert_eq!(run.status, "running");
        assert_eq!(run.prompt, "Fix the bug");
        assert!(run.tmux_window.is_none());

        let runs = mgr.list_for_worktree("w1").unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, run.id);
    }

    #[test]
    fn test_create_with_tmux_window() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run("w1", "Fix the bug", Some("feat-test"))
            .unwrap();
        assert_eq!(run.tmux_window.as_deref(), Some("feat-test"));

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.tmux_window.as_deref(), Some("feat-test"));
    }

    #[test]
    fn test_get_run() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None).unwrap();
        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.id, run.id);
        assert_eq!(fetched.prompt, "Fix the bug");

        let missing = mgr.get_run("nonexistent").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_update_completed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None).unwrap();
        mgr.update_run_completed(
            &run.id,
            Some("sess-123"),
            Some("Done!"),
            Some(0.05),
            Some(3),
            Some(15000),
        )
        .unwrap();

        let latest = mgr.latest_for_worktree("w1").unwrap().unwrap();
        assert_eq!(latest.status, "completed");
        assert_eq!(latest.claude_session_id.as_deref(), Some("sess-123"));
        assert_eq!(latest.cost_usd, Some(0.05));
    }

    #[test]
    fn test_update_failed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None).unwrap();
        mgr.update_run_failed(&run.id, "Something went wrong")
            .unwrap();

        let latest = mgr.latest_for_worktree("w1").unwrap().unwrap();
        assert_eq!(latest.status, "failed");
        assert_eq!(latest.result_text.as_deref(), Some("Something went wrong"));
    }

    #[test]
    fn test_update_cancelled() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None).unwrap();
        mgr.update_run_cancelled(&run.id).unwrap();

        let latest = mgr.latest_for_worktree("w1").unwrap().unwrap();
        assert_eq!(latest.status, "cancelled");
        assert!(latest.ended_at.is_some());
    }

    #[test]
    fn test_latest_for_worktree_empty() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let result = mgr.latest_for_worktree("w1").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_latest_runs_by_worktree() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Create runs for two different worktrees
        let _run1 = mgr.create_run("w1", "First prompt", None).unwrap();
        let run2 = mgr.create_run("w1", "Second prompt", None).unwrap();
        let run3 = mgr.create_run("w2", "Other prompt", None).unwrap();

        let map = mgr.latest_runs_by_worktree().unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("w1").unwrap().id, run2.id);
        assert_eq!(map.get("w2").unwrap().id, run3.id);
    }

    #[test]
    fn test_claude_json_result_deserialization() {
        let json = r#"{"session_id":"sess-abc","result":"Final output","cost_usd":0.05,"num_turns":3,"duration_ms":15000,"is_error":false}"#;
        let result: ClaudeJsonResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.session_id.as_deref(), Some("sess-abc"));
        assert_eq!(result.cost_usd, Some(0.05));
        assert_eq!(result.num_turns, Some(3));
        assert_eq!(result.duration_ms, Some(15000));
        assert_eq!(result.is_error, Some(false));
    }
}
