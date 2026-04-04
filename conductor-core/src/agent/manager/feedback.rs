use chrono::Utc;
use rusqlite::params;

use crate::db::query_collect;
use crate::error::{ConductorError, Result};

use super::super::db::{optional_row, row_to_feedback_request, FEEDBACK_SELECT};
use super::super::status::truncate_utf8;
use super::super::status::{FeedbackStatus, FeedbackType, FEEDBACK_MAX_LEN};
use super::super::types::{FeedbackOption, FeedbackRequest, FeedbackRequestParams};
use super::AgentManager;

/// Normalize a raw user response based on feedback type and available options.
///
/// For `Confirm`: normalizes to "yes" / "no".
/// For `SingleSelect`: maps a 1-based index to the option value.
/// For `MultiSelect`: maps comma-separated 1-based indices to a JSON array of option values.
/// For `Text`: returns the raw value unchanged.
///
/// Returns `Err` only if JSON serialization fails for multi-select.
pub fn normalize_feedback_response(
    feedback_type: &FeedbackType,
    options: Option<&[FeedbackOption]>,
    raw_value: &str,
) -> Result<String> {
    match feedback_type {
        FeedbackType::Confirm => {
            let trimmed = raw_value.trim().to_lowercase();
            if trimmed.starts_with('y') {
                Ok("yes".to_string())
            } else {
                Ok("no".to_string())
            }
        }
        FeedbackType::SingleSelect => {
            if let Some(opts) = options {
                if let Ok(idx) = raw_value.trim().parse::<usize>() {
                    if idx >= 1 && idx <= opts.len() {
                        return Ok(opts[idx - 1].value.clone());
                    }
                }
            }
            Ok(raw_value.to_string())
        }
        FeedbackType::MultiSelect => {
            if let Some(opts) = options {
                let selected: Vec<String> = raw_value
                    .split(',')
                    .filter_map(|s| {
                        let idx = s.trim().parse::<usize>().ok()?;
                        if idx >= 1 && idx <= opts.len() {
                            Some(opts[idx - 1].value.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                serde_json::to_string(&selected).map_err(|e| {
                    ConductorError::Agent(format!("Failed to serialize multi-select response: {e}"))
                })
            } else {
                Ok(raw_value.to_string())
            }
        }
        FeedbackType::Text => Ok(raw_value.to_string()),
    }
}

impl<'a> AgentManager<'a> {
    /// Transition a run to "waiting_for_feedback" and create a feedback request.
    ///
    /// The optional `params` argument carries structured feedback metadata
    /// (type, selectable options, timeout). Pass `None` for plain text feedback
    /// (backward-compatible default).
    pub fn request_feedback(
        &self,
        run_id: &str,
        prompt: &str,
        params: Option<&FeedbackRequestParams>,
    ) -> Result<FeedbackRequest> {
        let prompt = truncate_utf8(prompt, FEEDBACK_MAX_LEN);
        let id = crate::new_id();
        let now = Utc::now().to_rfc3339();

        let feedback_type = params.map(|p| p.feedback_type.clone()).unwrap_or_default();
        let options = params.and_then(|p| p.options.clone());
        let options_json = options
            .as_ref()
            .map(|o| {
                serde_json::to_string(o).map_err(|e| {
                    ConductorError::Agent(format!("Failed to serialize feedback options: {e}"))
                })
            })
            .transpose()?;
        let timeout_secs = params.and_then(|p| p.timeout_secs);

        // Validate: select types require options
        if matches!(
            feedback_type,
            FeedbackType::SingleSelect | FeedbackType::MultiSelect
        ) && options.as_ref().is_none_or(|o| o.is_empty())
        {
            return Err(ConductorError::Agent(
                "SingleSelect and MultiSelect feedback types require at least one option"
                    .to_string(),
            ));
        }

        // Update run status
        self.conn.execute(
            "UPDATE agent_runs SET status = 'waiting_for_feedback' WHERE id = ?1",
            params![run_id],
        )?;

        let req = FeedbackRequest {
            id: id.clone(),
            run_id: run_id.to_string(),
            prompt: prompt.to_string(),
            response: None,
            status: FeedbackStatus::Pending,
            created_at: now.clone(),
            responded_at: None,
            feedback_type: feedback_type.clone(),
            options: options.clone(),
            timeout_secs,
        };

        self.conn.execute(
            "INSERT INTO feedback_requests \
             (id, run_id, prompt, status, created_at, feedback_type, options_json, timeout_secs) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                req.id,
                req.run_id,
                req.prompt,
                req.status,
                req.created_at,
                feedback_type,
                options_json,
                timeout_secs,
            ],
        )?;

        Ok(req)
    }

    /// Submit a response to a pending feedback request and resume the run.
    pub fn submit_feedback(&self, feedback_id: &str, response: &str) -> Result<FeedbackRequest> {
        let response = truncate_utf8(response, FEEDBACK_MAX_LEN);
        let now = Utc::now().to_rfc3339();

        // Update feedback request
        let rows_affected = self.conn.execute(
            "UPDATE feedback_requests SET status = 'responded', response = ?1, responded_at = ?2 \
             WHERE id = ?3 AND status = 'pending'",
            params![response, now, feedback_id],
        )?;

        if rows_affected == 0 {
            return Err(self.feedback_not_pending_error(feedback_id));
        }

        self.resume_run_after_feedback(feedback_id)?;

        // Return updated feedback request
        let req = self.conn.query_row(
            &format!("{FEEDBACK_SELECT} WHERE id = ?1"),
            params![feedback_id],
            row_to_feedback_request,
        )?;

        Ok(req)
    }

    /// Dismiss a pending feedback request without responding; resume the run.
    pub fn dismiss_feedback(&self, feedback_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();

        let rows_affected = self.conn.execute(
            "UPDATE feedback_requests SET status = 'dismissed', responded_at = ?1 \
             WHERE id = ?2 AND status = 'pending'",
            params![now, feedback_id],
        )?;

        if rows_affected == 0 {
            return Err(self.feedback_not_pending_error(feedback_id));
        }

        self.resume_run_after_feedback(feedback_id)?;

        Ok(())
    }

    /// Build a `FeedbackNotPending` error by looking up the current status (or noting not found).
    fn feedback_not_pending_error(&self, feedback_id: &str) -> ConductorError {
        let status = self
            .get_feedback(feedback_id)
            .ok()
            .flatten()
            .map(|fb| fb.status.to_string())
            .unwrap_or_else(|| "not found".to_string());
        ConductorError::FeedbackNotPending {
            id: feedback_id.to_string(),
            status,
        }
    }

    /// Transition a run back to "running" after feedback is resolved.
    fn resume_run_after_feedback(&self, feedback_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE agent_runs SET status = 'running' \
             WHERE id = (SELECT run_id FROM feedback_requests WHERE id = ?1) \
             AND status = 'waiting_for_feedback'",
            params![feedback_id],
        )?;
        Ok(())
    }

    /// Get the pending feedback request for a run (if any).
    pub fn pending_feedback_for_run(&self, run_id: &str) -> Result<Option<FeedbackRequest>> {
        let result = self.conn.query_row(
            &format!(
                "{FEEDBACK_SELECT} WHERE run_id = ?1 AND status = 'pending' \
                 ORDER BY created_at DESC LIMIT 1"
            ),
            params![run_id],
            row_to_feedback_request,
        );

        optional_row(result)
    }

    /// Batch-fetch pending feedback requests for multiple run IDs at once.
    /// Returns a map of run_id → FeedbackRequest (most recent pending per run).
    pub fn pending_feedback_for_runs(
        &self,
        run_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, FeedbackRequest>> {
        if run_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let placeholders: Vec<String> = (1..=run_ids.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "{FEEDBACK_SELECT} WHERE run_id IN ({}) AND status = 'pending' \
             ORDER BY created_at DESC",
            placeholders.join(", ")
        );
        let params: Vec<&dyn rusqlite::types::ToSql> = run_ids
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let all: Vec<FeedbackRequest> =
            query_collect(self.conn, &sql, params.as_slice(), row_to_feedback_request)?;
        // Keep only the most recent pending per run (already ordered by created_at DESC).
        let mut map = std::collections::HashMap::new();
        for fb in all {
            map.entry(fb.run_id.clone()).or_insert(fb);
        }
        Ok(map)
    }

    /// Verify that `run_id` belongs to `conversation_id`.
    ///
    /// Returns `AgentRunNotFound` if the run does not exist, or
    /// `AgentRunNotInConversation` if it belongs to a different conversation.
    fn check_run_in_conversation(&self, run_id: &str, conversation_id: &str) -> Result<()> {
        let run = self
            .get_run(run_id)?
            .ok_or_else(|| ConductorError::AgentRunNotFound {
                id: run_id.to_string(),
            })?;
        if run.conversation_id.as_deref() != Some(conversation_id) {
            return Err(ConductorError::AgentRunNotInConversation {
                run_id: run_id.to_string(),
                conversation_id: conversation_id.to_string(),
            });
        }
        Ok(())
    }

    /// Submit a response to a feedback request, validating ownership.
    ///
    /// Verifies that `run_id` belongs to `conversation_id` and that `feedback_id`
    /// belongs to `run_id` before delegating to [`submit_feedback`].  Returns the
    /// refreshed `AgentRun` so callers have a consistent response surface.  Returns
    /// structured errors (`AgentRunNotFound`, `AgentRunNotInConversation`,
    /// `FeedbackNotFound`, `FeedbackRunMismatch`) so callers can map them to
    /// appropriate HTTP status codes without duplicating validation logic.
    pub fn submit_feedback_for_conversation(
        &self,
        conversation_id: &str,
        run_id: &str,
        feedback_id: &str,
        response: &str,
    ) -> Result<super::super::types::AgentRun> {
        self.check_run_in_conversation(run_id, conversation_id)?;
        let feedback =
            self.get_feedback(feedback_id)?
                .ok_or_else(|| ConductorError::FeedbackNotFound {
                    id: feedback_id.to_string(),
                })?;
        if feedback.run_id != run_id {
            return Err(ConductorError::FeedbackRunMismatch {
                feedback_id: feedback_id.to_string(),
                run_id: run_id.to_string(),
            });
        }
        self.submit_feedback(feedback_id, response)?;
        let updated = self
            .get_run(run_id)?
            .ok_or_else(|| ConductorError::AgentRunNotFound {
                id: run_id.to_string(),
            })?;
        Ok(updated)
    }

    /// Submit the pending feedback response for a run, validating conversation ownership.
    ///
    /// Verifies that `run_id` belongs to `conversation_id`, finds the single
    /// pending feedback request, submits `response`, and returns the refreshed
    /// `AgentRun`.  Returns structured errors so the web layer can map them to
    /// 404/422 without duplicating validation.
    pub fn submit_pending_run_feedback_for_conversation(
        &self,
        conversation_id: &str,
        run_id: &str,
        response: &str,
    ) -> Result<super::super::types::AgentRun> {
        self.check_run_in_conversation(run_id, conversation_id)?;
        let feedback = self.pending_feedback_for_run(run_id)?.ok_or_else(|| {
            ConductorError::NoPendingFeedbackForRun {
                run_id: run_id.to_string(),
            }
        })?;
        self.submit_feedback(&feedback.id, response)?;
        let updated = self
            .get_run(run_id)?
            .ok_or_else(|| ConductorError::AgentRunNotFound {
                id: run_id.to_string(),
            })?;
        Ok(updated)
    }

    /// Get a feedback request by ID.
    pub fn get_feedback(&self, feedback_id: &str) -> Result<Option<FeedbackRequest>> {
        let result = self.conn.query_row(
            &format!("{FEEDBACK_SELECT} WHERE id = ?1"),
            params![feedback_id],
            row_to_feedback_request,
        );

        optional_row(result)
    }

    /// List all feedback requests for a run, newest first.
    pub fn list_feedback_for_run(&self, run_id: &str) -> Result<Vec<FeedbackRequest>> {
        query_collect(
            self.conn,
            &format!("{FEEDBACK_SELECT} WHERE run_id = ?1 ORDER BY created_at DESC"),
            params![run_id],
            row_to_feedback_request,
        )
    }

    /// Get the pending feedback request for a worktree's latest running agent.
    pub fn pending_feedback_for_worktree(
        &self,
        worktree_id: &str,
    ) -> Result<Option<FeedbackRequest>> {
        let result = self.conn.query_row(
            &format!(
                "{FEEDBACK_SELECT} WHERE run_id IN \
                 (SELECT id FROM agent_runs WHERE worktree_id = ?1) \
                 AND status = 'pending' ORDER BY created_at DESC LIMIT 1"
            ),
            params![worktree_id],
            row_to_feedback_request,
        );

        optional_row(result)
    }

    /// List all pending feedback requests across all agent runs, newest first.
    /// Used by the TUI background poller to fire cross-process notifications.
    pub fn list_all_pending_feedback_requests(&self) -> Result<Vec<FeedbackRequest>> {
        query_collect(
            self.conn,
            &format!("{FEEDBACK_SELECT} WHERE status = 'pending' ORDER BY created_at DESC"),
            [],
            row_to_feedback_request,
        )
    }

    /// Dismiss all pending feedback requests whose per-request timeout has expired.
    ///
    /// For each expired request the feedback is marked `dismissed` and the
    /// parent run is resumed (back to `running`).  Returns the number of
    /// requests that were auto-dismissed.
    pub fn dismiss_expired_feedback_requests(&self) -> Result<usize> {
        let now = Utc::now().to_rfc3339();

        // Find pending requests that have a timeout and have exceeded it.
        let expired: Vec<FeedbackRequest> = query_collect(
            self.conn,
            &format!(
                "{FEEDBACK_SELECT} WHERE status = 'pending' \
                 AND timeout_secs IS NOT NULL \
                 AND datetime(created_at, '+' || timeout_secs || ' seconds') <= datetime(?1)"
            ),
            params![now],
            row_to_feedback_request,
        )?;

        let mut dismissed = 0;
        for fb in &expired {
            // Use dismiss_feedback which handles status update + run resume.
            match self.dismiss_feedback(&fb.id) {
                Ok(()) => dismissed += 1,
                Err(e) => {
                    eprintln!(
                        "warn: failed to dismiss expired feedback {} for run {}: {e}",
                        fb.id, fb.run_id
                    );
                }
            }
        }
        Ok(dismissed)
    }
}

#[cfg(test)]
mod tests {
    use super::super::setup_db;
    use super::super::AgentManager;
    use crate::agent::status::{AgentRunStatus, FeedbackStatus, FeedbackType};
    use crate::agent::types::{FeedbackOption, FeedbackRequestParams};

    fn insert_conversation(conn: &rusqlite::Connection, id: &str, scope_id: &str) {
        conn.execute(
            "INSERT INTO conversations (id, scope, scope_id, created_at, last_active_at) \
             VALUES (?1, 'worktree', ?2, '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z')",
            rusqlite::params![id, scope_id],
        )
        .unwrap();
    }

    #[test]
    fn test_request_feedback() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        assert_eq!(run.status, AgentRunStatus::Running);

        let fb = mgr
            .request_feedback(&run.id, "Should I refactor this module?", None)
            .unwrap();
        assert_eq!(fb.run_id, run.id);
        assert_eq!(fb.prompt, "Should I refactor this module?");
        assert_eq!(fb.status, FeedbackStatus::Pending);
        assert!(fb.response.is_none());
        assert!(fb.responded_at.is_none());

        let fetched_run = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched_run.status, AgentRunStatus::WaitingForFeedback);
    }

    #[test]
    fn test_submit_feedback() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let fb = mgr
            .request_feedback(&run.id, "Proceed with refactor?", None)
            .unwrap();

        let updated = mgr.submit_feedback(&fb.id, "Yes, go ahead").unwrap();
        assert_eq!(updated.status, FeedbackStatus::Responded);
        assert_eq!(updated.response.as_deref(), Some("Yes, go ahead"));
        assert!(updated.responded_at.is_some());

        let fetched_run = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched_run.status, AgentRunStatus::Running);
    }

    #[test]
    fn test_dismiss_feedback() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let fb = mgr
            .request_feedback(&run.id, "Need approval", None)
            .unwrap();

        mgr.dismiss_feedback(&fb.id).unwrap();

        let fetched_fb = mgr.get_feedback(&fb.id).unwrap().unwrap();
        assert_eq!(fetched_fb.status, FeedbackStatus::Dismissed);

        let fetched_run = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched_run.status, AgentRunStatus::Running);
    }

    #[test]
    fn test_pending_feedback_for_run() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();

        assert!(mgr.pending_feedback_for_run(&run.id).unwrap().is_none());

        let fb = mgr.request_feedback(&run.id, "Need input", None).unwrap();
        let pending = mgr.pending_feedback_for_run(&run.id).unwrap().unwrap();
        assert_eq!(pending.id, fb.id);

        mgr.submit_feedback(&fb.id, "Done").unwrap();
        assert!(mgr.pending_feedback_for_run(&run.id).unwrap().is_none());
    }

    #[test]
    fn test_pending_feedback_for_worktree() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();

        assert!(mgr.pending_feedback_for_worktree("w1").unwrap().is_none());

        let fb = mgr
            .request_feedback(&run.id, "Approve this?", None)
            .unwrap();
        let pending = mgr.pending_feedback_for_worktree("w1").unwrap().unwrap();
        assert_eq!(pending.id, fb.id);

        assert!(mgr.pending_feedback_for_worktree("w2").unwrap().is_none());
    }

    #[test]
    fn test_list_feedback_for_run() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();

        let fb1 = mgr.request_feedback(&run.id, "Question 1", None).unwrap();
        mgr.submit_feedback(&fb1.id, "Answer 1").unwrap();

        let fb2 = mgr.request_feedback(&run.id, "Question 2", None).unwrap();

        let all = mgr.list_feedback_for_run(&run.id).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].prompt, "Question 2");
        assert_eq!(all[0].status, FeedbackStatus::Pending);
        assert_eq!(all[1].prompt, "Question 1");
        assert_eq!(all[1].status, FeedbackStatus::Responded);

        let run2 = mgr
            .create_run(Some("w2"), "Other task", None, None)
            .unwrap();
        assert!(mgr.list_feedback_for_run(&run2.id).unwrap().is_empty());

        mgr.dismiss_feedback(&fb2.id).unwrap();
    }

    #[test]
    fn test_feedback_cascade_delete_on_run_removal() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let fb = mgr.request_feedback(&run.id, "Approve?", None).unwrap();

        conn.execute(
            "DELETE FROM agent_runs WHERE id = ?1",
            rusqlite::params![run.id],
        )
        .unwrap();

        assert!(mgr.get_feedback(&fb.id).unwrap().is_none());
    }

    #[test]
    fn test_submit_feedback_already_responded() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let fb = mgr.request_feedback(&run.id, "Proceed?", None).unwrap();

        mgr.submit_feedback(&fb.id, "Yes").unwrap();

        let err = mgr.submit_feedback(&fb.id, "Yes again").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not pending"),
            "expected not-pending error, got: {msg}"
        );
        assert!(
            msg.contains("responded"),
            "expected status 'responded' in error, got: {msg}"
        );
    }

    #[test]
    fn test_dismiss_feedback_already_dismissed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let fb = mgr.request_feedback(&run.id, "Approve?", None).unwrap();

        mgr.dismiss_feedback(&fb.id).unwrap();

        let err = mgr.dismiss_feedback(&fb.id).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not pending"),
            "expected not-pending error, got: {msg}"
        );
        assert!(
            msg.contains("dismissed"),
            "expected status 'dismissed' in error, got: {msg}"
        );
    }

    #[test]
    fn list_all_pending_feedback_requests_empty() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);
        let requests = mgr.list_all_pending_feedback_requests().unwrap();
        assert!(requests.is_empty(), "no feedback requests should exist yet");
    }

    #[test]
    fn list_all_pending_feedback_requests_returns_pending_only() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);
        let run = mgr
            .create_run(Some("w1"), "test prompt", None, None)
            .unwrap();

        let req1 = mgr.request_feedback(&run.id, "question 1", None).unwrap();
        let req2 = mgr.request_feedback(&run.id, "question 2", None).unwrap();
        let req3 = mgr.request_feedback(&run.id, "question 3", None).unwrap();
        mgr.submit_feedback(&req3.id, "answered").unwrap();

        let pending = mgr.list_all_pending_feedback_requests().unwrap();
        assert_eq!(pending.len(), 2, "only pending requests should be returned");
        let ids: Vec<_> = pending.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&req1.id.as_str()));
        assert!(ids.contains(&req2.id.as_str()));
    }

    #[test]
    fn list_all_pending_feedback_requests_across_runs() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);
        let run1 = mgr.create_run(Some("w1"), "run 1", None, None).unwrap();
        let run2 = mgr.create_run(Some("w2"), "run 2", None, None).unwrap();

        mgr.request_feedback(&run1.id, "from run 1", None).unwrap();
        mgr.request_feedback(&run2.id, "from run 2", None).unwrap();

        let pending = mgr.list_all_pending_feedback_requests().unwrap();
        assert_eq!(
            pending.len(),
            2,
            "pending requests from all runs should be returned"
        );
    }

    #[test]
    fn test_request_feedback_with_structured_params() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);
        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();

        let params = FeedbackRequestParams {
            feedback_type: FeedbackType::SingleSelect,
            options: Some(vec![
                FeedbackOption {
                    value: "p0".to_string(),
                    label: "P0 — Critical".to_string(),
                },
                FeedbackOption {
                    value: "p1".to_string(),
                    label: "P1 — High".to_string(),
                },
            ]),
            timeout_secs: Some(300),
        };

        let fb = mgr
            .request_feedback(&run.id, "Pick priority", Some(&params))
            .unwrap();
        assert_eq!(fb.feedback_type, FeedbackType::SingleSelect);
        assert_eq!(fb.options.as_ref().unwrap().len(), 2);
        assert_eq!(fb.options.as_ref().unwrap()[0].value, "p0");
        assert_eq!(fb.timeout_secs, Some(300));

        // Verify it round-trips through the DB
        let fetched = mgr.get_feedback(&fb.id).unwrap().unwrap();
        assert_eq!(fetched.feedback_type, FeedbackType::SingleSelect);
        assert_eq!(fetched.options.as_ref().unwrap().len(), 2);
        assert_eq!(fetched.timeout_secs, Some(300));
    }

    #[test]
    fn test_request_feedback_select_requires_options() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);
        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();

        let params = FeedbackRequestParams {
            feedback_type: FeedbackType::SingleSelect,
            options: None,
            timeout_secs: None,
        };

        let err = mgr
            .request_feedback(&run.id, "Pick one", Some(&params))
            .unwrap_err();
        assert!(
            err.to_string().contains("require at least one option"),
            "got: {err}"
        );
    }

    #[test]
    fn test_dismiss_expired_feedback_requests() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);
        let run = mgr.create_run(Some("w1"), "task", None, None).unwrap();

        // Create a feedback request with a 0-second timeout (already expired)
        let params = FeedbackRequestParams {
            feedback_type: FeedbackType::default(),
            options: None,
            timeout_secs: Some(0),
        };
        let fb = mgr
            .request_feedback(&run.id, "urgent question", Some(&params))
            .unwrap();

        // Also create one without a timeout (should NOT be dismissed)
        let run2 = mgr.create_run(Some("w2"), "task2", None, None).unwrap();
        let fb2 = mgr.request_feedback(&run2.id, "no timeout", None).unwrap();

        let dismissed = mgr.dismiss_expired_feedback_requests().unwrap();
        assert_eq!(dismissed, 1);

        let fetched = mgr.get_feedback(&fb.id).unwrap().unwrap();
        assert_eq!(fetched.status, FeedbackStatus::Dismissed);

        let fetched2 = mgr.get_feedback(&fb2.id).unwrap().unwrap();
        assert_eq!(fetched2.status, FeedbackStatus::Pending);

        // Run resumed after timeout dismiss
        let fetched_run = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched_run.status, AgentRunStatus::Running);
    }

    // ── submit_feedback_for_conversation ─────────────────────────────────────

    #[test]
    fn test_submit_feedback_for_conversation_success() {
        let conn = setup_db();
        insert_conversation(&conn, "conv1", "w1");
        let mgr = AgentManager::new(&conn);
        let run = mgr
            .create_run_for_conversation("w1", "task", None, None, "conv1")
            .unwrap();
        let fb = mgr.request_feedback(&run.id, "Approve?", None).unwrap();

        let updated_run = mgr
            .submit_feedback_for_conversation("conv1", &run.id, &fb.id, "yes")
            .unwrap();

        assert_eq!(updated_run.id, run.id);
        let fetched = mgr.get_feedback(&fb.id).unwrap().unwrap();
        assert_eq!(fetched.status, FeedbackStatus::Responded);
        assert_eq!(fetched.response.as_deref(), Some("yes"));
    }

    #[test]
    fn test_submit_feedback_for_conversation_run_not_found() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let err = mgr
            .submit_feedback_for_conversation("conv1", "nonexistent-run", "fb1", "yes")
            .unwrap_err();
        assert!(
            err.to_string().contains("nonexistent-run"),
            "expected run not found error, got: {err}"
        );
    }

    #[test]
    fn test_submit_feedback_for_conversation_run_not_in_conversation() {
        let conn = setup_db();
        insert_conversation(&conn, "conv1", "w1");
        let mgr = AgentManager::new(&conn);
        let run = mgr
            .create_run_for_conversation("w1", "task", None, None, "conv1")
            .unwrap();
        let fb = mgr.request_feedback(&run.id, "Approve?", None).unwrap();

        let err = mgr
            .submit_feedback_for_conversation("wrong-conv", &run.id, &fb.id, "yes")
            .unwrap_err();
        assert!(
            err.to_string().contains(&run.id) || err.to_string().contains("wrong-conv"),
            "expected not-in-conversation error, got: {err}"
        );
    }

    #[test]
    fn test_submit_feedback_for_conversation_feedback_not_found() {
        let conn = setup_db();
        insert_conversation(&conn, "conv1", "w1");
        let mgr = AgentManager::new(&conn);
        let run = mgr
            .create_run_for_conversation("w1", "task", None, None, "conv1")
            .unwrap();

        let err = mgr
            .submit_feedback_for_conversation("conv1", &run.id, "nonexistent-fb", "yes")
            .unwrap_err();
        assert!(
            err.to_string().contains("nonexistent-fb"),
            "expected feedback not found error, got: {err}"
        );
    }

    #[test]
    fn test_submit_feedback_for_conversation_feedback_run_mismatch() {
        let conn = setup_db();
        insert_conversation(&conn, "conv1", "w1");
        let mgr = AgentManager::new(&conn);
        let run1 = mgr
            .create_run_for_conversation("w1", "task1", None, None, "conv1")
            .unwrap();
        let run2 = mgr
            .create_run_for_conversation("w2", "task2", None, None, "conv1")
            .unwrap();
        let fb2 = mgr.request_feedback(&run2.id, "Approve?", None).unwrap();

        // fb2 belongs to run2, but we pass run1's id
        let err = mgr
            .submit_feedback_for_conversation("conv1", &run1.id, &fb2.id, "yes")
            .unwrap_err();
        assert!(
            err.to_string().contains(&fb2.id) || err.to_string().contains(&run1.id),
            "expected feedback run mismatch error, got: {err}"
        );
    }

    // ── submit_pending_run_feedback_for_conversation ─────────────────────────

    #[test]
    fn test_submit_pending_run_feedback_for_conversation_success() {
        let conn = setup_db();
        insert_conversation(&conn, "conv1", "w1");
        let mgr = AgentManager::new(&conn);
        let run = mgr
            .create_run_for_conversation("w1", "task", None, None, "conv1")
            .unwrap();
        mgr.request_feedback(&run.id, "Approve?", None).unwrap();

        let updated = mgr
            .submit_pending_run_feedback_for_conversation("conv1", &run.id, "yes")
            .unwrap();
        assert_eq!(updated.id, run.id);
        assert_eq!(updated.status, AgentRunStatus::Running);
    }

    #[test]
    fn test_submit_pending_run_feedback_run_not_found() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let err = mgr
            .submit_pending_run_feedback_for_conversation("conv1", "nonexistent", "yes")
            .unwrap_err();
        assert!(
            err.to_string().contains("nonexistent"),
            "expected run not found error, got: {err}"
        );
    }

    #[test]
    fn test_submit_pending_run_feedback_run_not_in_conversation() {
        let conn = setup_db();
        insert_conversation(&conn, "conv1", "w1");
        let mgr = AgentManager::new(&conn);
        let run = mgr
            .create_run_for_conversation("w1", "task", None, None, "conv1")
            .unwrap();

        let err = mgr
            .submit_pending_run_feedback_for_conversation("wrong-conv", &run.id, "yes")
            .unwrap_err();
        assert!(
            err.to_string().contains(&run.id) || err.to_string().contains("wrong-conv"),
            "expected not-in-conversation error, got: {err}"
        );
    }

    #[test]
    fn test_submit_pending_run_feedback_no_pending_feedback() {
        let conn = setup_db();
        insert_conversation(&conn, "conv1", "w1");
        let mgr = AgentManager::new(&conn);
        let run = mgr
            .create_run_for_conversation("w1", "task", None, None, "conv1")
            .unwrap();

        let err = mgr
            .submit_pending_run_feedback_for_conversation("conv1", &run.id, "yes")
            .unwrap_err();
        assert!(
            err.to_string().contains(&run.id),
            "expected no-pending-feedback error, got: {err}"
        );
    }
}
