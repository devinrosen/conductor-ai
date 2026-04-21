use chrono::Utc;
use rusqlite::named_params;

use crate::error::Result;

use super::super::status::AgentRunStatus;
use super::super::types::{AgentRun, LogResult};
use super::AgentManager;

impl<'a> AgentManager<'a> {
    pub fn create_run(
        &self,
        worktree_id: Option<&str>,
        prompt: &str,
        model: Option<&str>,
    ) -> Result<AgentRun> {
        self.create_run_with_parent(worktree_id, None, prompt, model, None, None, None, None)
    }

    /// Create a run scoped to a repo (no worktree). Used for read-only repo agents.
    pub fn create_repo_run(
        &self,
        repo_id: &str,
        prompt: &str,
        model: Option<&str>,
    ) -> Result<AgentRun> {
        self.create_run_with_parent(None, Some(repo_id), prompt, model, None, None, None, None)
    }

    pub fn create_child_run(
        &self,
        worktree_id: Option<&str>,
        prompt: &str,
        model: Option<&str>,
        parent_run_id: &str,
        bot_name: Option<&str>,
    ) -> Result<AgentRun> {
        self.create_run_with_parent(
            worktree_id,
            None,
            prompt,
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
        model: Option<&str>,
        conversation_id: &str,
    ) -> Result<AgentRun> {
        self.create_run_with_parent(
            Some(worktree_id),
            None,
            prompt,
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
        model: Option<&str>,
        conversation_id: &str,
    ) -> Result<AgentRun> {
        self.create_run_with_parent(
            None,
            Some(repo_id),
            prompt,
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
            runtime: "claude".to_string(),
        };

        self.conn.execute(
            "INSERT INTO agent_runs \
             (id, worktree_id, repo_id, prompt, status, started_at, model, \
              parent_run_id, bot_name, log_file, conversation_id, runtime) \
             VALUES (:id, :worktree_id, :repo_id, :prompt, :status, :started_at, \
                     :model, :parent_run_id, :bot_name, :log_file, :conversation_id, :runtime)",
            named_params! {
                ":id": run.id,
                ":worktree_id": run.worktree_id,
                ":repo_id": run.repo_id,
                ":prompt": run.prompt,
                ":status": run.status,
                ":started_at": run.started_at,
                ":model": run.model,
                ":parent_run_id": run.parent_run_id,
                ":bot_name": run.bot_name,
                ":log_file": run.log_file,
                ":conversation_id": run.conversation_id,
                ":runtime": run.runtime,
            },
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
            "UPDATE agent_runs SET status = 'completed', claude_session_id = :session_id, \
             result_text = :result_text, cost_usd = :cost_usd, num_turns = :num_turns, \
             duration_ms = :duration_ms, ended_at = :ended_at, \
             input_tokens = :input_tokens, output_tokens = :output_tokens, \
             cache_read_input_tokens = :cache_read_input_tokens, \
             cache_creation_input_tokens = :cache_creation_input_tokens \
             WHERE id = :id",
            named_params! {
                ":session_id": session_id,
                ":result_text": result_text,
                ":cost_usd": cost_usd,
                ":num_turns": num_turns,
                ":duration_ms": duration_ms,
                ":ended_at": now,
                ":id": run_id,
                ":input_tokens": input_tokens,
                ":output_tokens": output_tokens,
                ":cache_read_input_tokens": cache_read_input_tokens,
                ":cache_creation_input_tokens": cache_creation_input_tokens,
            },
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
            "UPDATE agent_runs SET status = 'failed', result_text = :error, ended_at = :ended_at, \
             claude_session_id = COALESCE(:session_id, claude_session_id) \
             WHERE id = :id",
            named_params! {
                ":error": error,
                ":ended_at": now,
                ":session_id": session_id,
                ":id": run_id,
            },
        )?;
        Ok(())
    }

    /// Mark a run as failed only if it is currently `running` or `waiting_for_feedback`.
    /// Used by background reapers and panic monitors to avoid overwriting a run that has
    /// already been finalized (e.g. `completed`, `failed`, `cancelled`) by another path.
    pub fn update_run_failed_if_running(&self, run_id: &str, error: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE agent_runs SET status = 'failed', result_text = :error, ended_at = :ended_at \
             WHERE id = :id AND status IN ('running', 'waiting_for_feedback')",
            named_params! {
                ":error": error,
                ":ended_at": now,
                ":id": run_id,
            },
        )?;
        Ok(())
    }

    /// Mark a run as completed (with a summary) only if it is currently `running`.
    /// Used by background reapers to avoid overwriting a run that has already
    /// been finalized by another path.
    pub fn update_run_completed_if_running(&self, run_id: &str, result_text: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE agent_runs SET status = 'completed', result_text = :result_text, ended_at = :ended_at \
             WHERE id = :id AND status = 'running'",
            named_params! {
                ":result_text": result_text,
                ":ended_at": now,
                ":id": run_id,
            },
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
             SET status = 'completed', result_text = :result_text, ended_at = :ended_at, \
                 claude_session_id = COALESCE(:session_id, claude_session_id), \
                 cost_usd = COALESCE(:cost_usd, cost_usd), \
                 num_turns = COALESCE(:num_turns, num_turns), \
                 duration_ms = COALESCE(:duration_ms, duration_ms), \
                 input_tokens = COALESCE(:input_tokens, input_tokens), \
                 output_tokens = COALESCE(:output_tokens, output_tokens), \
                 cache_read_input_tokens = COALESCE(:cache_read_input_tokens, cache_read_input_tokens), \
                 cache_creation_input_tokens = COALESCE(:cache_creation_input_tokens, cache_creation_input_tokens) \
             WHERE id = :id AND status = 'running'",
            named_params! {
                ":result_text": result_text,
                ":ended_at": now,
                ":session_id": log_result.session_id.as_deref(),
                ":cost_usd": log_result.cost_usd,
                ":num_turns": log_result.num_turns,
                ":duration_ms": log_result.duration_ms,
                ":input_tokens": log_result.input_tokens,
                ":output_tokens": log_result.output_tokens,
                ":cache_read_input_tokens": log_result.cache_read_input_tokens,
                ":cache_creation_input_tokens": log_result.cache_creation_input_tokens,
                ":id": run_id,
            },
        )?;
        Ok(())
    }

    pub fn update_run_cancelled(&self, run_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE agent_runs SET status = 'cancelled', ended_at = :ended_at WHERE id = :id",
            named_params! {
                ":ended_at": now,
                ":id": run_id,
            },
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
            crate::process_utils::cancel_subprocess(pid as u32);
        }

        Ok(())
    }

    /// Save the claude session_id as soon as it's known (before run completes).
    /// This enables resume even if the run fails or is cancelled.
    pub fn update_run_session_id(&self, run_id: &str, session_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE agent_runs SET claude_session_id = :session_id WHERE id = :id",
            named_params! {
                ":session_id": session_id,
                ":id": run_id,
            },
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
             SET input_tokens = :input_tokens, output_tokens = :output_tokens, \
                 cache_read_input_tokens = :cache_read_input_tokens, \
                 cache_creation_input_tokens = :cache_creation_input_tokens \
             WHERE id = :id",
            named_params! {
                ":input_tokens": input_tokens,
                ":output_tokens": output_tokens,
                ":cache_read_input_tokens": cache_read_input_tokens,
                ":cache_creation_input_tokens": cache_creation_input_tokens,
                ":id": run_id,
            },
        )?;
        Ok(())
    }

    /// Record the runtime name for an agent run.
    pub fn update_run_runtime(&self, run_id: &str, runtime: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE agent_runs SET runtime = :runtime WHERE id = :id",
            named_params! { ":runtime": runtime, ":id": run_id },
        )?;
        Ok(())
    }

    /// Store the OS PID for a headless agent run immediately after spawn.
    pub fn update_run_subprocess_pid(&self, run_id: &str, pid: u32) -> Result<()> {
        self.conn.execute(
            "UPDATE agent_runs SET subprocess_pid = :pid WHERE id = :id",
            named_params! {
                ":pid": pid as i64,
                ":id": run_id,
            },
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
             SET model = COALESCE(:model, model), claude_session_id = COALESCE(:session_id, claude_session_id) \
             WHERE id = :id",
            named_params! {
                ":model": model,
                ":session_id": session_id,
                ":id": run_id,
            },
        )?;
        Ok(())
    }

    pub fn update_run_log_file(&self, run_id: &str, path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE agent_runs SET log_file = :path WHERE id = :id",
            named_params! {
                ":path": path,
                ":id": run_id,
            },
        )?;
        Ok(())
    }

    /// Delete all agent runs for a conversation.
    ///
    /// Child tables (`agent_run_events`, `agent_run_steps`, etc.) are removed
    /// automatically via their `ON DELETE CASCADE` FK constraints.
    pub fn delete_runs_for_conversation(&self, conversation_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM agent_runs WHERE conversation_id = :conversation_id",
            named_params! { ":conversation_id": conversation_id },
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
            original.model.as_deref(),
            Some(run_id),
            original.bot_name.as_deref(),
            None,
            None,
        )
    }

    /// Open the database and mark the run failed if it is still in `running` status.
    ///
    /// Best-effort: logs a warning if the DB cannot be opened or the update fails, but
    /// never panics. Used to clean up on drain-thread DB-open errors and drain-thread
    /// panics so the run does not stay stuck in `running` until the orphan reaper fires.
    pub fn try_mark_run_failed_in_db(
        db_path: &std::path::Path,
        run_id: &str,
        msg: &str,
        log_prefix: &str,
    ) {
        match crate::db::open_database(db_path) {
            Err(open_err) => {
                tracing::warn!("[{log_prefix}] could not open DB for failure recovery: {open_err}");
            }
            Ok(conn) => {
                if let Err(update_err) =
                    AgentManager::new(&conn).update_run_failed_if_running(run_id, msg)
                {
                    tracing::warn!("[{log_prefix}] failed to mark run failed: {update_err}");
                }
            }
        }
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

        let run = mgr.create_run(Some("w1"), "Fix the bug", None).unwrap();
        assert_eq!(run.status, AgentRunStatus::Running);
        assert_eq!(run.prompt, "Fix the bug");

        let runs = mgr.list_for_worktree("w1").unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, run.id);
    }

    #[test]
    fn test_update_completed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "Fix the bug", None).unwrap();
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

        let run = mgr.create_run(Some("w1"), "Fix the bug", None).unwrap();
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

        let run = mgr.create_run(Some("w1"), "Fix the bug", None).unwrap();
        mgr.update_run_cancelled(&run.id).unwrap();

        let latest = mgr.latest_for_worktree("w1").unwrap().unwrap();
        assert_eq!(latest.status, AgentRunStatus::Cancelled);
        assert!(latest.ended_at.is_some());
    }

    #[test]
    fn test_update_log_file() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "Fix the bug", None).unwrap();
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

        let run = mgr.create_run(Some("w1"), "Fix the bug", None).unwrap();
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

        let run = mgr.create_run(Some("w1"), "Fix the bug", None).unwrap();
        assert!(run.claude_session_id.is_none());

        mgr.update_run_session_id(&run.id, "sess-early").unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.claude_session_id.as_deref(), Some("sess-early"));
    }

    #[test]
    fn test_failed_with_session_preserves_eager_session_id() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "Fix the bug", None).unwrap();
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
            .create_run(Some("w1"), "Fix the bug", Some("claude-sonnet-4-6"))
            .unwrap();
        assert_eq!(run.model.as_deref(), Some("claude-sonnet-4-6"));

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.model.as_deref(), Some("claude-sonnet-4-6"));
    }

    #[test]
    fn test_update_run_tokens_partial_writes_values() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "Fix the bug", None).unwrap();
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

        let run = mgr.create_run(Some("w1"), "Fix the bug", None).unwrap();

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

        let run = mgr.create_run(Some("w1"), "Fix the bug", None).unwrap();
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

        let run = mgr.create_run(Some("w1"), "Fix the bug", None).unwrap();
        assert!(run.model.is_none());

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert!(fetched.model.is_none());
    }

    #[test]
    fn test_restart_run_creates_new_run_with_same_config() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", Some("claude-sonnet-4-6"))
            .unwrap();
        mgr.update_run_failed(&run.id, "Crashed").unwrap();

        let restarted = mgr.restart_run(&run.id).unwrap();
        assert_eq!(restarted.status, AgentRunStatus::Running);
        assert_eq!(restarted.prompt, "Fix the bug");
        assert_eq!(restarted.model.as_deref(), Some("claude-sonnet-4-6"));
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
            .create_run(Some("w1"), "Fix the bug", Some("claude-sonnet-4-6"))
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

        let run = mgr.create_run(Some("w1"), "Fix the bug", None).unwrap();
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

        let run = mgr.create_repo_run("r1", "Analyse the repo", None).unwrap();
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

        let run = mgr.create_repo_run("r1", "Analyse the repo", None).unwrap();

        assert_eq!(run.repo_id.as_deref(), Some("r1"));
        assert!(run.worktree_id.is_none());
        assert_eq!(run.prompt, "Analyse the repo");
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

        let run = mgr.create_run(Some("w1"), "task", None).unwrap();
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
    fn test_update_run_failed_if_running_noop_when_already_completed() {
        // The `AND status IN ('running', 'waiting_for_feedback')` guard must also
        // preserve a run already in `completed` state (drain-panic-monitor scenario:
        // drain_stream_json succeeds, then the panic monitor fires after the fact).
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "task", None).unwrap();
        mgr.update_run_completed_if_running(&run.id, "done")
            .unwrap();

        // Panic-monitor path must be a no-op on an already-completed run.
        mgr.update_run_failed_if_running(&run.id, "drain thread panicked")
            .unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(
            fetched.status,
            AgentRunStatus::Completed,
            "completed run must not be clobbered by drain panic handler"
        );
        assert_eq!(
            fetched.result_text.as_deref(),
            Some("done"),
            "result_text must not be overwritten when run is already completed"
        );
    }

    #[test]
    fn test_update_run_completed_if_running_noop_when_already_failed() {
        // The `AND status = 'running'` guard must prevent overwriting a run that
        // has already been finalized (e.g. by another reaper path).
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "task", None).unwrap();
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

        let run = mgr.create_run(Some("w1"), "task", None).unwrap();

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

        let run = mgr.create_run(Some("w1"), "task", None).unwrap();
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

        let run = mgr.create_run(Some("w1"), "task", None).unwrap();
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
            .create_run(Some("w1"), "test", Some("original-model"))
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

        let run = mgr.create_run(Some("w1"), "task", None).unwrap();
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

        let run = mgr.create_run(Some("w1"), "task", None).unwrap();
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

        let run = mgr.create_run(Some("w1"), "task", None).unwrap();
        mgr.cancel_run(&run.id, None).unwrap();
        // Second cancel should succeed — the UPDATE is a no-op but still OK.
        mgr.cancel_run(&run.id, None).unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.status, AgentRunStatus::Cancelled);
    }

    #[test]
    #[cfg(unix)]
    fn test_cancel_run_with_subprocess_pid() {
        // cancel_run with Some(pid) must update the DB and attempt a best-effort
        // subprocess kill. Using a nonexistent PID (i64::MAX) exercises the
        // Some(pid) branch without killing any real process; cancel_subprocess
        // handles ESRCH gracefully via a warn! log.
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "task", None).unwrap();
        // i64::MAX is guaranteed not to be a real PID on any platform.
        mgr.cancel_run(&run.id, Some(i64::MAX)).unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.status, AgentRunStatus::Cancelled);
        assert!(fetched.ended_at.is_some());
    }

    #[test]
    fn test_pid_persist_failure_path_marks_run_failed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "Fix the bug", None).unwrap();

        let pid_err = "disk I/O error";
        let msg = format!("failed to persist subprocess pid: {pid_err}");
        mgr.update_run_failed(&run.id, &msg)
            .expect("update_run_failed must succeed");

        let fetched = mgr.get_run(&run.id).unwrap().expect("run must exist");
        assert_eq!(
            fetched.status,
            AgentRunStatus::Failed,
            "run should be failed"
        );
        assert!(
            fetched
                .result_text
                .as_deref()
                .unwrap_or("")
                .contains("persist subprocess pid"),
            "result_text should reference 'persist subprocess pid', got: {:?}",
            fetched.result_text
        );
    }

    /// Verify that `AgentManager::try_mark_run_failed_in_db` transitions the run to `Failed`
    /// when the retry DB open succeeds.
    ///
    /// The `drain_db_open_failure_no_pipe_deadlock` test uses a permanently bad
    /// `db_path` so the retry also fails silently.  This test uses a real temp
    /// DB to confirm the status transition when the retry succeeds.
    #[test]
    fn drain_db_open_failure_marks_run_failed() {
        let tmp = tempfile::NamedTempFile::new().expect("temp db");
        let conn = crate::db::open_database(tmp.path()).expect("open db");
        crate::test_helpers::insert_test_repo(&conn, "r1", "test-repo", "/tmp/repo");
        crate::test_helpers::insert_test_worktree(
            &conn,
            "w1",
            "r1",
            "feat-test",
            "/tmp/ws/feat-test",
        );
        let run = AgentManager::new(&conn)
            .create_run(Some("w1"), "test prompt", None)
            .expect("create run");
        let run_id = run.id.clone();

        // Drop conn so the DB file is fully flushed before try_mark_run_failed_in_db
        // opens its own connection.
        drop(conn);

        let err_msg = "drain thread failed to open DB: io error";
        AgentManager::try_mark_run_failed_in_db(tmp.path(), &run_id, err_msg, "test");

        // Re-open to read back the final state.
        let conn2 = crate::db::open_database(tmp.path()).expect("re-open db");
        let run_after = AgentManager::new(&conn2)
            .get_run(&run_id)
            .unwrap()
            .expect("run must exist");

        assert_eq!(
            run_after.status,
            AgentRunStatus::Failed,
            "run should be marked failed after drain DB-open error"
        );
        assert!(
            run_after
                .result_text
                .as_deref()
                .unwrap_or("")
                .contains("drain thread failed to open DB"),
            "result_text should contain the error message, got: {:?}",
            run_after.result_text
        );

        let _ = tmp;
    }

    #[test]
    fn test_update_run_runtime() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);
        let run = mgr.create_run(Some("w1"), "prompt", None).unwrap();
        assert_eq!(run.runtime.as_str(), "claude");
        mgr.update_run_runtime(&run.id, "gemini").unwrap();
        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.runtime.as_str(), "gemini");
    }
}
