use chrono::Utc;
use rusqlite::params;

use crate::error::Result;

use super::super::status::AgentRunStatus;
use super::super::types::{AgentRun, LogResult};
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
            None,
        )
    }

    /// Create a worktree-scoped run linked to a conversation.
    pub fn create_run_for_conversation(
        &self,
        worktree_id: &str,
        prompt: &str,
        tmux_window: Option<&str>,
        model: Option<&str>,
        conversation_id: &str,
    ) -> Result<AgentRun> {
        self.create_run_with_parent(
            Some(worktree_id),
            None,
            prompt,
            tmux_window,
            model,
            None,
            None,
            None,
            Some(conversation_id),
        )
    }

    /// Create a repo-scoped run linked to a conversation.
    pub fn create_repo_run_for_conversation(
        &self,
        repo_id: &str,
        prompt: &str,
        tmux_window: Option<&str>,
        model: Option<&str>,
        conversation_id: &str,
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
            Some(conversation_id),
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
        conversation_id: Option<&str>,
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
            conversation_id: conversation_id.map(String::from),
            subprocess_pid: None,
        };

        self.conn.execute(
            "INSERT INTO agent_runs \
             (id, worktree_id, repo_id, prompt, status, started_at, tmux_window, model, \
              parent_run_id, bot_name, log_file, conversation_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
                run.log_file,
                run.conversation_id,
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

    /// Mark a run as failed only if it is currently `running` or `waiting_for_feedback`.
    /// Used by background reapers and panic monitors to avoid overwriting a run that has
    /// already been finalized (e.g. `completed`, `failed`, `cancelled`) by another path.
    pub fn update_run_failed_if_running(&self, run_id: &str, error: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE agent_runs SET status = 'failed', result_text = ?1, ended_at = ?2 \
             WHERE id = ?3 AND status IN ('running', 'waiting_for_feedback')",
            params![error, now, run_id],
        )?;
        Ok(())
    }

    /// Mark a run as completed (with a summary) only if it is currently `running`.
    /// Used by background reapers to avoid overwriting a run that has already
    /// been finalized by another path.
    pub fn update_run_completed_if_running(&self, run_id: &str, result_text: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE agent_runs SET status = 'completed', result_text = ?1, ended_at = ?2 \
             WHERE id = ?3 AND status = 'running'",
            params![result_text, now, run_id],
        )?;
        Ok(())
    }

    /// Mark a run as completed with all result-event fields, only if it is currently `running`.
    ///
    /// This is the authoritative write for the headless drain path. It persists
    /// `cost_usd`, `num_turns`, `duration_ms`, all token counts, and optionally
    /// `claude_session_id` (via COALESCE so an eagerly-stored session_id is not
    /// clobbered). The `AND status = 'running'` guard prevents double-writes if
    /// the subprocess has already finalized the row.
    pub fn update_run_completed_if_running_full(
        &self,
        run_id: &str,
        log_result: &LogResult,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let result_text = log_result.result_text.as_deref().unwrap_or("");
        self.conn.execute(
            "UPDATE agent_runs \
             SET status = 'completed', result_text = ?1, ended_at = ?2, \
                 claude_session_id = COALESCE(?3, claude_session_id), \
                 cost_usd = COALESCE(?4, cost_usd), \
                 num_turns = COALESCE(?5, num_turns), \
                 duration_ms = COALESCE(?6, duration_ms), \
                 input_tokens = COALESCE(?7, input_tokens), \
                 output_tokens = COALESCE(?8, output_tokens), \
                 cache_read_input_tokens = COALESCE(?9, cache_read_input_tokens), \
                 cache_creation_input_tokens = COALESCE(?10, cache_creation_input_tokens) \
             WHERE id = ?11 AND status = 'running'",
            params![
                result_text,
                now,
                log_result.session_id.as_deref(),
                log_result.cost_usd,
                log_result.num_turns,
                log_result.duration_ms,
                log_result.input_tokens,
                log_result.output_tokens,
                log_result.cache_read_input_tokens,
                log_result.cache_creation_input_tokens,
                run_id,
            ],
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

    /// Cancel a run: mark DB as cancelled first, then best-effort kill the subprocess.
    ///
    /// The DB update precedes the process kill so that a concurrent drain cannot
    /// overwrite the `cancelled` status after the process exits.
    ///
    /// Returns `Err` only if the DB update fails; subprocess kill is best-effort.
    pub fn cancel_run(&self, run_id: &str, subprocess_pid: Option<i64>) -> Result<()> {
        // Step 1: persist cancellation in DB before touching the process.
        self.update_run_cancelled(run_id)?;

        // Step 2: best-effort terminate the subprocess (if any).
        if let Some(pid) = subprocess_pid {
            // subprocess_pid is i64 in DB (SQLite integer); cast to u32 is safe for
            // realistic PID values.
            crate::agent_runtime::cancel_subprocess(pid as u32);
        }

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

    /// Update token counts for a running agent run (best-effort, idempotent).
    ///
    /// Uses assignment (`=`) semantics — not increment — so repeated calls are
    /// safe.  Each call provides cumulative totals read from the log file, so
    /// using `+=` would multiply-count tokens.  The final [`update_run_completed`]
    /// call overwrites these columns with the authoritative values from the
    /// `result` event.
    pub fn update_run_tokens_partial(
        &self,
        run_id: &str,
        input_tokens: i64,
        output_tokens: i64,
        cache_read_input_tokens: i64,
        cache_creation_input_tokens: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE agent_runs \
             SET input_tokens = ?1, output_tokens = ?2, \
                 cache_read_input_tokens = ?3, cache_creation_input_tokens = ?4 \
             WHERE id = ?5",
            rusqlite::params![
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
                run_id,
            ],
        )?;
        Ok(())
    }

    /// Store the OS PID for a headless agent run immediately after spawn.
    pub fn update_run_subprocess_pid(&self, run_id: &str, pid: u32) -> Result<()> {
        self.conn.execute(
            "UPDATE agent_runs SET subprocess_pid = ?1 WHERE id = ?2",
            params![pid as i64, run_id],
        )?;
        Ok(())
    }

    /// Eagerly save model and session_id from the stream-json system/init event.
    ///
    /// Uses COALESCE so the write is idempotent and cannot clobber a value already
    /// written by the subprocess itself — only sets the column if the incoming value
    /// is not NULL.
    pub fn update_run_model_and_session(
        &self,
        run_id: &str,
        model: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE agent_runs \
             SET model = COALESCE(?1, model), claude_session_id = COALESCE(?2, claude_session_id) \
             WHERE id = ?3",
            params![model, session_id, run_id],
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

    /// Delete all agent runs for a conversation.
    ///
    /// Child tables (`agent_run_events`, `agent_run_steps`, etc.) are removed
    /// automatically via their `ON DELETE CASCADE` FK constraints.
    pub fn delete_runs_for_conversation(&self, conversation_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM agent_runs WHERE conversation_id = ?1",
            params![conversation_id],
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
            None,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::setup_db;
    use super::super::AgentManager;
    use crate::agent::status::AgentRunStatus;
    use crate::agent::types::LogResult;

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
    fn test_update_run_tokens_partial_writes_values() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        assert!(run.input_tokens.is_none());

        mgr.update_run_tokens_partial(&run.id, 100, 50, 20, 10)
            .unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.input_tokens, Some(100));
        assert_eq!(fetched.output_tokens, Some(50));
        assert_eq!(fetched.cache_read_input_tokens, Some(20));
        assert_eq!(fetched.cache_creation_input_tokens, Some(10));
        // Status must remain Running — partial update must not touch status
        assert_eq!(
            fetched.status,
            crate::agent::status::AgentRunStatus::Running
        );
    }

    #[test]
    fn test_update_run_tokens_partial_overwrites_not_accumulates() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();

        mgr.update_run_tokens_partial(&run.id, 100, 50, 20, 10)
            .unwrap();
        // Second call with larger cumulative totals — must overwrite, not add
        mgr.update_run_tokens_partial(&run.id, 200, 80, 30, 15)
            .unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.input_tokens, Some(200));
        assert_eq!(fetched.output_tokens, Some(80));
        assert_eq!(fetched.cache_read_input_tokens, Some(30));
        assert_eq!(fetched.cache_creation_input_tokens, Some(15));
    }

    #[test]
    fn test_update_run_completed_overwrites_partial_tokens() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        mgr.update_run_tokens_partial(&run.id, 100, 50, 20, 10)
            .unwrap();

        // Authoritative final values from result event
        mgr.update_run_completed(
            &run.id,
            None,
            Some("Done"),
            Some(0.01),
            Some(3),
            Some(5000),
            Some(999),
            Some(888),
            Some(777),
            Some(666),
        )
        .unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.input_tokens, Some(999));
        assert_eq!(fetched.output_tokens, Some(888));
        assert_eq!(fetched.cache_read_input_tokens, Some(777));
        assert_eq!(fetched.cache_creation_input_tokens, Some(666));
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

    #[test]
    fn test_create_run_with_parent_log_file() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run_with_parent(
                Some("w1"),
                None,
                "Fix the bug",
                Some("feat-test"),
                None,
                None,
                None,
                Some("/tmp/agent-logs/run.log"),
                None,
            )
            .unwrap();

        assert_eq!(run.log_file.as_deref(), Some("/tmp/agent-logs/run.log"));

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.log_file.as_deref(), Some("/tmp/agent-logs/run.log"));
    }

    #[test]
    fn test_update_run_failed_if_running_noop_when_already_failed() {
        // The `AND status = 'running'` guard must prevent overwriting a run that
        // has already been finalized (e.g. by another reaper path).
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "task", None, None).unwrap();
        mgr.update_run_failed(&run.id, "original error").unwrap();

        // Calling the if_running variant on an already-failed run must be a no-op.
        mgr.update_run_failed_if_running(&run.id, "overwritten error")
            .unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.status, AgentRunStatus::Failed);
        assert_eq!(
            fetched.result_text.as_deref(),
            Some("original error"),
            "result_text must not be overwritten when run is not running"
        );
    }

    #[test]
    fn test_update_run_completed_if_running_noop_when_already_failed() {
        // The `AND status = 'running'` guard must prevent overwriting a run that
        // has already been finalized (e.g. by another reaper path).
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "task", None, None).unwrap();
        mgr.update_run_failed(&run.id, "original error").unwrap();

        // Calling the if_running variant on an already-failed run must be a no-op.
        mgr.update_run_completed_if_running(&run.id, "overwritten result")
            .unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.status, AgentRunStatus::Failed);
        assert_eq!(
            fetched.result_text.as_deref(),
            Some("original error"),
            "result_text must not be overwritten when run is not running"
        );
    }

    #[test]
    fn test_update_run_completed_if_running_full_persists_all_fields() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "task", None, None).unwrap();

        mgr.update_run_completed_if_running_full(
            &run.id,
            &LogResult {
                result_text: Some("All done".into()),
                session_id: Some("sess-result".into()),
                cost_usd: Some(0.05),
                num_turns: Some(3),
                duration_ms: Some(5000),
                is_error: false,
                input_tokens: Some(200),
                output_tokens: Some(100),
                cache_read_input_tokens: Some(50),
                cache_creation_input_tokens: Some(25),
            },
        )
        .unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.status, AgentRunStatus::Completed);
        assert_eq!(fetched.result_text.as_deref(), Some("All done"));
        assert_eq!(fetched.claude_session_id.as_deref(), Some("sess-result"));
        assert_eq!(fetched.cost_usd, Some(0.05));
        assert_eq!(fetched.num_turns, Some(3));
        assert_eq!(fetched.duration_ms, Some(5000));
        assert_eq!(fetched.input_tokens, Some(200));
        assert_eq!(fetched.output_tokens, Some(100));
        assert_eq!(fetched.cache_read_input_tokens, Some(50));
        assert_eq!(fetched.cache_creation_input_tokens, Some(25));
        assert!(fetched.ended_at.is_some());
    }

    #[test]
    fn test_update_run_completed_if_running_full_coalesce_preserves_eager_session_when_none() {
        // When the result event does not carry a session_id (None), the COALESCE
        // guard must preserve the session_id written eagerly from the system/init event.
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "task", None, None).unwrap();
        mgr.update_run_session_id(&run.id, "sess-early").unwrap();

        mgr.update_run_completed_if_running_full(
            &run.id,
            &LogResult {
                result_text: Some("All done".into()),
                session_id: None, // no session_id in result event
                cost_usd: Some(0.01),
                num_turns: Some(1),
                duration_ms: Some(1000),
                is_error: false,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            },
        )
        .unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.status, AgentRunStatus::Completed);
        // COALESCE(NULL, "sess-early") → preserves eagerly stored session_id
        assert_eq!(
            fetched.claude_session_id.as_deref(),
            Some("sess-early"),
            "eagerly stored session_id must be preserved when result event has none"
        );
    }

    #[test]
    fn test_update_run_completed_if_running_full_noop_when_not_running() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "task", None, None).unwrap();
        mgr.update_run_failed(&run.id, "original error").unwrap();

        // Guard must prevent overwriting a finalized run
        mgr.update_run_completed_if_running_full(
            &run.id,
            &LogResult {
                result_text: Some("overwritten result".into()),
                session_id: None,
                cost_usd: Some(0.99),
                num_turns: Some(99),
                duration_ms: Some(99999),
                is_error: false,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            },
        )
        .unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.status, AgentRunStatus::Failed);
        assert_eq!(
            fetched.result_text.as_deref(),
            Some("original error"),
            "result_text must not be overwritten when run is not running"
        );
        assert!(
            fetched.cost_usd.is_none(),
            "cost_usd must not be written when run is not running"
        );
    }

    #[test]
    fn test_update_run_model_and_session_coalesce_idempotency() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Create a run with a known model
        let run = mgr
            .create_run(Some("w1"), "test", None, Some("original-model"))
            .unwrap();
        assert_eq!(run.model.as_deref(), Some("original-model"));

        // Update with model=None — COALESCE should preserve original
        mgr.update_run_model_and_session(&run.id, None, Some("sess-abc"))
            .unwrap();
        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(
            fetched.model.as_deref(),
            Some("original-model"),
            "model should not be clobbered by NULL"
        );
        assert_eq!(fetched.claude_session_id.as_deref(), Some("sess-abc"));

        // Update again with session_id=None — COALESCE should preserve original session
        mgr.update_run_model_and_session(&run.id, Some("new-model"), None)
            .unwrap();
        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(
            fetched.model.as_deref(),
            Some("new-model"),
            "model should be updated when not NULL"
        );
        assert_eq!(
            fetched.claude_session_id.as_deref(),
            Some("sess-abc"),
            "session should not be clobbered by NULL"
        );
    }

    #[test]
    fn test_update_run_failed_if_running_transitions_waiting_for_feedback() {
        // `update_run_failed_if_running` guards on `status IN ('running',
        // 'waiting_for_feedback')`.  A run stuck in `waiting_for_feedback`
        // (e.g. the drain thread panicked before the agent could answer a
        // feedback request) must be moved to `failed` so it does not stay
        // stuck indefinitely.
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "task", None, None).unwrap();
        mgr.request_feedback(&run.id, "what should I do?", None)
            .expect("request_feedback must succeed");

        // Confirm transition to WaitingForFeedback.
        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.status, AgentRunStatus::WaitingForFeedback);

        // Simulate drain-panic monitor: must succeed and flip to Failed.
        mgr.update_run_failed_if_running(&run.id, "drain thread panicked")
            .expect("update_run_failed_if_running must not return an error");

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(
            fetched.status,
            AgentRunStatus::Failed,
            "waiting_for_feedback run must be transitioned to failed"
        );
        assert!(
            fetched
                .result_text
                .as_deref()
                .unwrap_or("")
                .contains("drain thread panicked"),
            "result_text should record the panic reason"
        );
    }

    #[test]
    fn test_cancel_run_no_subprocess_pid() {
        // cancel_run with subprocess_pid=None should only update the DB.
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "task", None, None).unwrap();
        mgr.cancel_run(&run.id, None).unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.status, AgentRunStatus::Cancelled);
        assert!(fetched.ended_at.is_some());
    }

    #[test]
    fn test_cancel_run_idempotent() {
        // Calling cancel_run twice on an already-cancelled run must not return an error.
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "task", None, None).unwrap();
        mgr.cancel_run(&run.id, None).unwrap();
        // Second cancel should succeed — the UPDATE is a no-op but still OK.
        mgr.cancel_run(&run.id, None).unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.status, AgentRunStatus::Cancelled);
    }
}
