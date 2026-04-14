use chrono::Utc;
use rusqlite::{params, Connection};

use crate::agent::AgentManager;
use crate::error::{ConductorError, Result};

use super::types::{Conversation, ConversationScope, ConversationWithRuns};

pub struct ConversationManager<'a> {
    conn: &'a Connection,
}

impl<'a> ConversationManager<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Create a new conversation scoped to a repo or worktree.
    pub fn create(&self, scope: ConversationScope, scope_id: &str) -> Result<Conversation> {
        let id = crate::new_id();
        let now = Utc::now().to_rfc3339();

        let conv = Conversation {
            id: id.clone(),
            scope,
            scope_id: scope_id.to_string(),
            title: None,
            created_at: now.clone(),
            last_active_at: now,
        };

        self.conn.execute(
            "INSERT INTO conversations (id, scope, scope_id, title, created_at, last_active_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                conv.id,
                conv.scope.to_string(),
                conv.scope_id,
                conv.title,
                conv.created_at,
                conv.last_active_at,
            ],
        )?;

        Ok(conv)
    }

    /// Fetch a conversation by ID. Returns `None` if not found.
    pub fn get(&self, id: &str) -> Result<Option<Conversation>> {
        let result = self.conn.query_row(
            "SELECT id, scope, scope_id, title, created_at, last_active_at \
             FROM conversations WHERE id = ?1",
            params![id],
            row_to_conversation,
        );
        match result {
            Ok(conv) => Ok(Some(conv)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// List all conversations for a given scope, newest-active first.
    pub fn list(&self, scope: &ConversationScope, scope_id: &str) -> Result<Vec<Conversation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, scope, scope_id, title, created_at, last_active_at \
             FROM conversations \
             WHERE scope = ?1 AND scope_id = ?2 \
             ORDER BY last_active_at DESC",
        )?;
        let rows = stmt.query_map(params![scope.to_string(), scope_id], row_to_conversation)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Fetch a conversation along with all its associated agent runs (oldest first).
    pub fn get_with_runs(&self, id: &str) -> Result<Option<ConversationWithRuns>> {
        let conv = match self.get(id)? {
            Some(c) => c,
            None => return Ok(None),
        };

        let agent_mgr = AgentManager::new(self.conn);
        let runs = agent_mgr.list_for_conversation(id)?;

        Ok(Some(ConversationWithRuns {
            conversation: conv,
            runs,
        }))
    }

    /// Check whether the conversation has any currently active (running or
    /// waiting_for_feedback) agent run.
    pub fn has_active_run(&self, conversation_id: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM agent_runs \
             WHERE conversation_id = ?1 \
               AND status IN ('running', 'waiting_for_feedback')",
            params![conversation_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Return the `claude_session_id` of the most recent *completed* run in the
    /// conversation, or `None` if no such run exists (fresh session).
    pub fn last_completed_session_id(&self, conversation_id: &str) -> Result<Option<String>> {
        let result: rusqlite::Result<Option<String>> = self.conn.query_row(
            "SELECT claude_session_id FROM agent_runs \
             WHERE conversation_id = ?1 AND status = 'completed' \
             ORDER BY started_at DESC LIMIT 1",
            params![conversation_id],
            |row| row.get(0),
        );
        match result {
            Ok(v) => Ok(v),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Update the `last_active_at` timestamp to now.
    pub fn update_last_active(&self, conversation_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE conversations SET last_active_at = ?1 WHERE id = ?2",
            params![now, conversation_id],
        )?;
        Ok(())
    }

    /// Set the conversation title if it has not been set yet (first message).
    /// Truncates to 60 characters.
    pub fn set_title_if_unset(&self, conversation_id: &str, prompt: &str) -> Result<()> {
        let title: String = prompt.chars().take(60).collect();
        self.conn.execute(
            "UPDATE conversations SET title = ?1 WHERE id = ?2 AND title IS NULL",
            params![title, conversation_id],
        )?;
        Ok(())
    }

    /// Clear the most-recent conversation for a scope (repo or worktree).
    ///
    /// Equivalent to calling `list` then `delete` on the first result, but
    /// encapsulated here so callers do not need to duplicate the orchestration.
    ///
    /// # Errors
    /// - `ConductorError::ConversationNotFound` if no conversation exists for the scope.
    /// - `ConductorError::ConversationHasActiveRun` if the conversation has an active run.
    pub fn clear_for_scope(&self, scope: &ConversationScope, scope_id: &str) -> Result<()> {
        let conv = self
            .list(scope, scope_id)?
            .into_iter()
            .next()
            .ok_or_else(|| ConductorError::ConversationNotFound {
                id: scope_id.to_string(),
            })?;
        self.delete(&conv.id)
    }

    /// Hard-delete a conversation and all its associated agent runs.
    ///
    /// Child tables of `agent_runs` (events, steps, feedback, created issues) are
    /// removed automatically via their `ON DELETE CASCADE` FK constraints.
    ///
    /// # Errors
    /// - `ConductorError::ConversationNotFound` if the conversation does not exist.
    /// - `ConductorError::ConversationHasActiveRun` if there is an active or
    ///   waiting agent run — the caller must stop the run first.
    pub fn delete(&self, id: &str) -> Result<()> {
        self.get(id)?
            .ok_or_else(|| ConductorError::ConversationNotFound { id: id.to_string() })?;

        if self.has_active_run(id)? {
            return Err(ConductorError::ConversationHasActiveRun { id: id.to_string() });
        }

        AgentManager::new(self.conn).delete_runs_for_conversation(id)?;

        self.conn
            .execute("DELETE FROM conversations WHERE id = ?1", params![id])?;

        Ok(())
    }

    /// Validate, create an agent run record for this conversation, update metadata,
    /// and return the new run together with the resume session ID (if any).
    ///
    /// The caller is responsible for spawning the agent process after this returns.
    ///
    /// # Errors
    /// - `ConductorError::Agent` if the conversation is not found.
    /// - `ConductorError::Agent` if there is already an active run in this conversation.
    pub fn send_message(
        &self,
        conversation_id: &str,
        prompt: &str,
        tmux_window: Option<&str>,
        model: Option<&str>,
    ) -> Result<(crate::agent::AgentRun, Option<String>)> {
        let conv = self.get(conversation_id)?.ok_or_else(|| {
            ConductorError::Agent(format!("conversation {conversation_id} not found"))
        })?;

        if self.has_active_run(conversation_id)? {
            return Err(ConductorError::Agent(
                "conversation already has an active agent run".to_string(),
            ));
        }

        let resume_session_id = self.last_completed_session_id(conversation_id)?;

        let agent_mgr = AgentManager::new(self.conn);
        let run = match conv.scope {
            ConversationScope::Worktree => agent_mgr.create_run_for_conversation(
                &conv.scope_id,
                prompt,
                tmux_window,
                model,
                conversation_id,
            )?,
            ConversationScope::Repo => agent_mgr.create_repo_run_for_conversation(
                &conv.scope_id,
                prompt,
                tmux_window,
                model,
                conversation_id,
            )?,
        };

        self.set_title_if_unset(conversation_id, prompt)?;
        self.update_last_active(conversation_id)?;

        Ok((run, resume_session_id))
    }
}

fn row_to_conversation(row: &rusqlite::Row) -> rusqlite::Result<Conversation> {
    let scope_str: String = row.get(1)?;
    let scope = scope_str.parse::<ConversationScope>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;

    Ok(Conversation {
        id: row.get(0)?,
        scope,
        scope_id: row.get(2)?,
        title: row.get(3)?,
        created_at: row.get(4)?,
        last_active_at: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentRunStatus;

    fn setup_db() -> Connection {
        crate::test_helpers::setup_db()
    }

    #[test]
    fn test_create_and_get_conversation() {
        let conn = setup_db();
        let mgr = ConversationManager::new(&conn);

        let conv = mgr.create(ConversationScope::Repo, "repo-001").unwrap();
        assert_eq!(conv.scope, ConversationScope::Repo);
        assert_eq!(conv.scope_id, "repo-001");
        assert!(conv.title.is_none());

        let fetched = mgr.get(&conv.id).unwrap().unwrap();
        assert_eq!(fetched.id, conv.id);
        assert_eq!(fetched.scope, ConversationScope::Repo);
        assert_eq!(fetched.scope_id, "repo-001");
    }

    #[test]
    fn test_get_returns_none_for_missing() {
        let conn = setup_db();
        let mgr = ConversationManager::new(&conn);
        let result = mgr.get("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_list_conversations() {
        let conn = setup_db();
        let mgr = ConversationManager::new(&conn);

        mgr.create(ConversationScope::Repo, "r1").unwrap();
        mgr.create(ConversationScope::Repo, "r1").unwrap();
        mgr.create(ConversationScope::Repo, "r2").unwrap();

        let r1_convs = mgr.list(&ConversationScope::Repo, "r1").unwrap();
        assert_eq!(r1_convs.len(), 2);

        let r2_convs = mgr.list(&ConversationScope::Repo, "r2").unwrap();
        assert_eq!(r2_convs.len(), 1);

        let wt_convs = mgr.list(&ConversationScope::Worktree, "r1").unwrap();
        assert_eq!(wt_convs.len(), 0);
    }

    #[test]
    fn test_set_title_if_unset() {
        let conn = setup_db();
        let mgr = ConversationManager::new(&conn);

        let conv = mgr.create(ConversationScope::Repo, "r1").unwrap();
        assert!(conv.title.is_none());

        // First call sets the title.
        mgr.set_title_if_unset(&conv.id, "Tell me about the repo structure")
            .unwrap();
        let fetched = mgr.get(&conv.id).unwrap().unwrap();
        assert_eq!(
            fetched.title.as_deref(),
            Some("Tell me about the repo structure")
        );

        // Second call with a different prompt must NOT overwrite.
        mgr.set_title_if_unset(&conv.id, "Something else").unwrap();
        let fetched2 = mgr.get(&conv.id).unwrap().unwrap();
        assert_eq!(
            fetched2.title.as_deref(),
            Some("Tell me about the repo structure")
        );
    }

    #[test]
    fn test_title_truncated_to_60_chars() {
        let conn = setup_db();
        let mgr = ConversationManager::new(&conn);
        let conv = mgr.create(ConversationScope::Repo, "r1").unwrap();
        let long_prompt = "a".repeat(100);
        mgr.set_title_if_unset(&conv.id, &long_prompt).unwrap();
        let fetched = mgr.get(&conv.id).unwrap().unwrap();
        assert_eq!(fetched.title.as_ref().unwrap().len(), 60);
    }

    #[test]
    fn test_has_active_run_false_when_no_runs() {
        let conn = setup_db();
        let mgr = ConversationManager::new(&conn);
        let conv = mgr.create(ConversationScope::Repo, "r1").unwrap();
        assert!(!mgr.has_active_run(&conv.id).unwrap());
    }

    #[test]
    fn test_send_message_creates_run_and_blocks_second_message() {
        // setup_db() already seeds repo 'r1' and worktree 'w1'.
        let conn = setup_db();

        let mgr = ConversationManager::new(&conn);
        let conv = mgr.create(ConversationScope::Repo, "r1").unwrap();

        let (run, session_id) = mgr
            .send_message(&conv.id, "What does this repo do?", None, None)
            .unwrap();

        assert_eq!(run.conversation_id.as_deref(), Some(conv.id.as_str()));
        assert_eq!(run.repo_id.as_deref(), Some("r1"));
        assert_eq!(run.status, AgentRunStatus::Running);
        assert!(session_id.is_none()); // no prior completed run

        // Title should have been set.
        let updated = mgr.get(&conv.id).unwrap().unwrap();
        assert_eq!(updated.title.as_deref(), Some("What does this repo do?"));

        // Second message must be rejected — there's an active run.
        let err = mgr
            .send_message(&conv.id, "Another message", None, None)
            .unwrap_err();
        assert!(err.to_string().contains("active agent run"));
    }

    #[test]
    fn test_last_completed_session_id_returns_latest() {
        let conn = setup_db();
        let agent_mgr = crate::agent::AgentManager::new(&conn);
        let mgr = ConversationManager::new(&conn);

        let conv = mgr.create(ConversationScope::Repo, "r1").unwrap();

        // Create two completed runs: the second (newer) should win.
        let run1 = agent_mgr
            .create_repo_run_for_conversation("r1", "q1", None, None, &conv.id)
            .unwrap();
        agent_mgr
            .update_run_completed(
                &run1.id,
                Some("sess-aaa"),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();

        let run2 = agent_mgr
            .create_repo_run_for_conversation("r1", "q2", None, None, &conv.id)
            .unwrap();
        agent_mgr
            .update_run_completed(
                &run2.id,
                Some("sess-bbb"),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();

        let session = mgr.last_completed_session_id(&conv.id).unwrap();
        assert_eq!(session.as_deref(), Some("sess-bbb"));
    }

    #[test]
    fn test_delete_removes_conversation() {
        let conn = setup_db();
        let mgr = ConversationManager::new(&conn);
        let conv = mgr.create(ConversationScope::Repo, "r1").unwrap();

        mgr.delete(&conv.id).unwrap();

        assert!(mgr.get(&conv.id).unwrap().is_none());
    }

    #[test]
    fn test_delete_returns_not_found_for_unknown_id() {
        let conn = setup_db();
        let mgr = ConversationManager::new(&conn);
        let err = mgr.delete("nonexistent").unwrap_err();
        assert!(
            matches!(err, ConductorError::ConversationNotFound { .. }),
            "expected ConversationNotFound, got: {err}"
        );
    }

    #[test]
    fn test_delete_blocks_when_active_run_exists() {
        let conn = setup_db();
        let mgr = ConversationManager::new(&conn);
        let conv = mgr.create(ConversationScope::Repo, "r1").unwrap();

        // Create a running agent run linked to this conversation.
        let agent_mgr = crate::agent::AgentManager::new(&conn);
        agent_mgr
            .create_repo_run_for_conversation("r1", "hello", None, None, &conv.id)
            .unwrap();

        let err = mgr.delete(&conv.id).unwrap_err();
        assert!(
            matches!(err, ConductorError::ConversationHasActiveRun { .. }),
            "expected ConversationHasActiveRun, got: {err}"
        );
        // Conversation must still exist.
        assert!(mgr.get(&conv.id).unwrap().is_some());
    }

    #[test]
    fn test_delete_cascades_agent_runs() {
        let conn = setup_db();
        let mgr = ConversationManager::new(&conn);
        let agent_mgr = crate::agent::AgentManager::new(&conn);
        let conv = mgr.create(ConversationScope::Repo, "r1").unwrap();

        // Create a run and immediately mark it completed so delete is not blocked.
        let run = agent_mgr
            .create_repo_run_for_conversation("r1", "q", None, None, &conv.id)
            .unwrap();
        agent_mgr
            .update_run_completed(
                &run.id, None, None, None, None, None, None, None, None, None,
            )
            .unwrap();

        mgr.delete(&conv.id).unwrap();

        // Both conversation and run must be gone.
        assert!(mgr.get(&conv.id).unwrap().is_none());
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_runs WHERE conversation_id = ?1",
                params![conv.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn test_get_with_runs_returns_ordered_runs() {
        // setup_db() already seeds repo 'r1' and worktree 'w1'.
        let conn = setup_db();

        let mgr = ConversationManager::new(&conn);
        let conv = mgr.create(ConversationScope::Repo, "r1").unwrap();

        let agent_mgr = crate::agent::AgentManager::new(&conn);
        let run1 = agent_mgr
            .create_repo_run_for_conversation("r1", "first", None, None, &conv.id)
            .unwrap();
        let run2 = agent_mgr
            .create_repo_run_for_conversation("r1", "second", None, None, &conv.id)
            .unwrap();

        let with_runs = mgr.get_with_runs(&conv.id).unwrap().unwrap();
        assert_eq!(with_runs.runs.len(), 2);
        assert_eq!(with_runs.runs[0].id, run1.id);
        assert_eq!(with_runs.runs[1].id, run2.id);
    }
}
