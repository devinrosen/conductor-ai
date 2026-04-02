use chrono::Utc;
use rusqlite::params;

use crate::error::Result;

use super::super::status::AgentRunStatus;
use super::super::types::AgentRun;
use super::AgentManager;

impl<'a> AgentManager<'a> {
    pub fn create_run(
        &self,
        worktree_id: Option<&str>,
        prompt: &str,
        tmux_window: Option<&str>,
        model: Option<&str>,
    ) -> Result<AgentRun> {
        self.create_run_with_parent(
            worktree_id,
            None,
            prompt,
            tmux_window,
            model,
            None,
            None,
            None,
        )
    }

    /// Create a run scoped to a repo (no worktree). Used for read-only repo agents.
    pub fn create_repo_run(
        &self,
        repo_id: &str,
        prompt: &str,
        tmux_window: Option<&str>,
        model: Option<&str>,
    ) -> Result<AgentRun> {
        self.create_run_with_parent(
            None,
            Some(repo_id),
            prompt,
            tmux_window,
            model,
            None,
            None,
            None,
        )
    }

    pub fn create_child_run(
        &self,
        worktree_id: Option<&str>,
        prompt: &str,
        tmux_window: Option<&str>,
        model: Option<&str>,
        parent_run_id: &str,
        bot_name: Option<&str>,
    ) -> Result<AgentRun> {
        self.create_run_with_parent(
            worktree_id,
            None,
            prompt,
            tmux_window,
            model,
            Some(parent_run_id),
            bot_name,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn create_run_with_parent(
        &self,
        worktree_id: Option<&str>,
        repo_id: Option<&str>,
        prompt: &str,
        tmux_window: Option<&str>,
        model: Option<&str>,
        parent_run_id: Option<&str>,
        bot_name: Option<&str>,
        log_file: Option<&str>,
    ) -> Result<AgentRun> {
        let id = crate::new_id();
        let now = Utc::now().to_rfc3339();

        let run = AgentRun {
            id: id.clone(),
            worktree_id: worktree_id.map(String::from),
            repo_id: repo_id.map(String::from),
            claude_session_id: None,
            prompt: prompt.to_string(),
            status: AgentRunStatus::Running,
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            started_at: now.clone(),
            ended_at: None,
            tmux_window: tmux_window.map(String::from),
            log_file: log_file.map(String::from),
            model: model.map(String::from),
            plan: None,
            parent_run_id: parent_run_id.map(String::from),
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            bot_name: bot_name.map(String::from),
        };

        self.conn.execute(
            "INSERT INTO agent_runs (id, worktree_id, repo_id, prompt, status, started_at, tmux_window, model, parent_run_id, bot_name, log_file) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                run.id,
                run.worktree_id,
                run.repo_id,
                run.prompt,
                run.status,
                run.started_at,
                run.tmux_window,
                run.model,
                run.parent_run_id,
                run.bot_name,
                run.log_file
            ],
        )?;

        Ok(run)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_run_completed(
        &self,
        run_id: &str,
        session_id: Option<&str>,
        result_text: Option<&str>,
        cost_usd: Option<f64>,
        num_turns: Option<i64>,
        duration_ms: Option<i64>,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
        cache_read_input_tokens: Option<i64>,
        cache_creation_input_tokens: Option<i64>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE agent_runs SET status = 'completed', claude_session_id = ?1, \
             result_text = ?2, cost_usd = ?3, num_turns = ?4, duration_ms = ?5, \
             ended_at = ?6, input_tokens = ?8, output_tokens = ?9, \
             cache_read_input_tokens = ?10, cache_creation_input_tokens = ?11 \
             WHERE id = ?7",
            params![
                session_id,
                result_text,
                cost_usd,
                num_turns,
                duration_ms,
                now,
                run_id,
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
            ],
        )?;
        Ok(())
    }

    pub fn update_run_failed(&self, run_id: &str, error: &str) -> Result<()> {
        self.update_run_failed_with_session(run_id, error, None)
    }

    /// Mark a run as failed, optionally preserving the session_id for resume.
    pub fn update_run_failed_with_session(
        &self,
        run_id: &str,
        error: &str,
        session_id: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE agent_runs SET status = 'failed', result_text = ?1, ended_at = ?2, \
             claude_session_id = COALESCE(?3, claude_session_id) \
             WHERE id = ?4",
            params![error, now, session_id, run_id],
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

    /// Save the claude session_id as soon as it's known (before run completes).
    /// This enables resume even if the run fails or is cancelled.
    pub fn update_run_session_id(&self, run_id: &str, session_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE agent_runs SET claude_session_id = ?1 WHERE id = ?2",
            params![session_id, run_id],
        )?;
        Ok(())
    }

    pub fn update_run_log_file(&self, run_id: &str, path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE agent_runs SET log_file = ?1 WHERE id = ?2",
            params![path, run_id],
        )?;
        Ok(())
    }

    /// Create a new run by cloning the prompt/config from a failed run.
    ///
    /// The original run must be in a terminal state (failed or cancelled).
    /// The new run gets `parent_run_id` set to the original run's ID to
    /// preserve the restart lineage.
    ///
    /// Returns the newly created `AgentRun` record (status = Running).
    pub fn restart_run(&self, run_id: &str) -> Result<AgentRun> {
        let original = self.get_run(run_id)?.ok_or_else(|| {
            crate::error::ConductorError::Agent(format!("Run {run_id} not found"))
        })?;

        if original.is_active() {
            return Err(crate::error::ConductorError::Agent(
                "Cannot restart an active run".to_string(),
            ));
        }

        self.create_run_with_parent(
            original.worktree_id.as_deref(),
            original.repo_id.as_deref(),
            &original.prompt,
            original.tmux_window.as_deref(),
            original.model.as_deref(),
            Some(run_id),
            original.bot_name.as_deref(),
            None,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::setup_db;
    use super::super::AgentManager;
    use crate::agent::status::AgentRunStatus;

    #[test]
    fn test_create_and_list() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        assert_eq!(run.status, AgentRunStatus::Running);
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
            .create_run(Some("w1"), "Fix the bug", Some("feat-test"), None)
            .unwrap();
        assert_eq!(run.tmux_window.as_deref(), Some("feat-test"));

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.tmux_window.as_deref(), Some("feat-test"));
    }

    #[test]
    fn test_update_completed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        mgr.update_run_completed(
            &run.id,
            Some("sess-123"),
            Some("Done!"),
            Some(0.05),
            Some(3),
            Some(15000),
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let latest = mgr.latest_for_worktree("w1").unwrap().unwrap();
        assert_eq!(latest.status, AgentRunStatus::Completed);
        assert_eq!(latest.claude_session_id.as_deref(), Some("sess-123"));
        assert_eq!(latest.cost_usd, Some(0.05));
    }

    #[test]
    fn test_update_failed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        mgr.update_run_failed(&run.id, "Something went wrong")
            .unwrap();

        let latest = mgr.latest_for_worktree("w1").unwrap().unwrap();
        assert_eq!(latest.status, AgentRunStatus::Failed);
        assert_eq!(latest.result_text.as_deref(), Some("Something went wrong"));
    }

    #[test]
    fn test_update_cancelled() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        mgr.update_run_cancelled(&run.id).unwrap();

        let latest = mgr.latest_for_worktree("w1").unwrap().unwrap();
        assert_eq!(latest.status, AgentRunStatus::Cancelled);
        assert!(latest.ended_at.is_some());
    }

    #[test]
    fn test_update_log_file() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", Some("feat-test"), None)
            .unwrap();
        assert!(run.log_file.is_none());

        mgr.update_run_log_file(&run.id, "/tmp/agent-logs/test.log")
            .unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(
            fetched.log_file.as_deref(),
            Some("/tmp/agent-logs/test.log")
        );
    }

    #[test]
    fn test_update_run_failed_with_session() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        mgr.update_run_failed_with_session(&run.id, "Context exhausted", Some("sess-456"))
            .unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.status, AgentRunStatus::Failed);
        assert_eq!(fetched.result_text.as_deref(), Some("Context exhausted"));
        assert_eq!(fetched.claude_session_id.as_deref(), Some("sess-456"));
    }

    #[test]
    fn test_update_run_session_id() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        assert!(run.claude_session_id.is_none());

        mgr.update_run_session_id(&run.id, "sess-early").unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.claude_session_id.as_deref(), Some("sess-early"));
    }

    #[test]
    fn test_failed_with_session_preserves_eager_session_id() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        // Session ID was saved eagerly during stream
        mgr.update_run_session_id(&run.id, "sess-eager").unwrap();
        // Fail without passing session_id (uses COALESCE to keep existing)
        mgr.update_run_failed(&run.id, "Crashed").unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.claude_session_id.as_deref(), Some("sess-eager"));
    }

    #[test]
    fn test_create_run_with_model() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, Some("claude-sonnet-4-6"))
            .unwrap();
        assert_eq!(run.model.as_deref(), Some("claude-sonnet-4-6"));

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.model.as_deref(), Some("claude-sonnet-4-6"));
    }

    #[test]
    fn test_create_run_without_model() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        assert!(run.model.is_none());

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert!(fetched.model.is_none());
    }

    #[test]
    fn test_restart_run_creates_new_run_with_same_config() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(
                Some("w1"),
                "Fix the bug",
                Some("feat-test"),
                Some("claude-sonnet-4-6"),
            )
            .unwrap();
        mgr.update_run_failed(&run.id, "Crashed").unwrap();

        let restarted = mgr.restart_run(&run.id).unwrap();
        assert_eq!(restarted.status, AgentRunStatus::Running);
        assert_eq!(restarted.prompt, "Fix the bug");
        assert_eq!(restarted.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(restarted.tmux_window.as_deref(), Some("feat-test"));
        assert_eq!(restarted.parent_run_id.as_deref(), Some(run.id.as_str()));
        assert_ne!(restarted.id, run.id);

        // Original run stays failed
        let original = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(original.status, AgentRunStatus::Failed);
    }

    #[test]
    fn test_restart_run_from_cancelled() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(
                Some("w1"),
                "Fix the bug",
                Some("feat-test"),
                Some("claude-sonnet-4-6"),
            )
            .unwrap();
        mgr.update_run_cancelled(&run.id).unwrap();

        let restarted = mgr.restart_run(&run.id).unwrap();
        assert_eq!(restarted.status, AgentRunStatus::Running);
        assert_eq!(restarted.prompt, "Fix the bug");
        assert_eq!(restarted.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(restarted.parent_run_id.as_deref(), Some(run.id.as_str()));

        // Original run stays cancelled
        let original = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(original.status, AgentRunStatus::Cancelled);
    }

    #[test]
    fn test_restart_run_rejects_active_run() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        // Run is still Running — restart should fail
        let result = mgr.restart_run(&run.id);
        assert!(result.is_err());
    }

    #[test]
    fn test_restart_run_not_found() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);
        let result = mgr.restart_run("nonexistent-id");
        assert!(result.is_err());
    }

    #[test]
    fn test_restart_run_preserves_repo_scope() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_repo_run("r1", "Analyse the repo", Some("repo-test-abc"), None)
            .unwrap();
        mgr.update_run_failed(&run.id, "Crashed").unwrap();

        let restarted = mgr.restart_run(&run.id).unwrap();
        assert_eq!(restarted.repo_id.as_deref(), Some("r1"));
        assert!(restarted.worktree_id.is_none());
        assert_eq!(restarted.prompt, "Analyse the repo");
        assert_eq!(restarted.parent_run_id.as_deref(), Some(run.id.as_str()));
    }

    #[test]
    fn test_create_repo_run() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_repo_run("r1", "Analyse the repo", Some("repo-test-abc"), None)
            .unwrap();

        assert_eq!(run.repo_id.as_deref(), Some("r1"));
        assert!(run.worktree_id.is_none());
        assert_eq!(run.prompt, "Analyse the repo");
        assert_eq!(run.tmux_window.as_deref(), Some("repo-test-abc"));
        assert_eq!(run.status, AgentRunStatus::Running);

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.repo_id.as_deref(), Some("r1"));
        assert!(fetched.worktree_id.is_none());
    }
}
