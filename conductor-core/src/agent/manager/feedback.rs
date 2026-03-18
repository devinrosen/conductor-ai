use chrono::Utc;
use rusqlite::params;

use crate::db::query_collect;
use crate::error::Result;

use super::super::db::{optional_row, row_to_feedback_request, FEEDBACK_SELECT};
use super::super::status::truncate_utf8;
use super::super::status::{FeedbackStatus, FEEDBACK_MAX_LEN};
use super::super::types::FeedbackRequest;
use super::AgentManager;

impl<'a> AgentManager<'a> {
    /// Transition a run to "waiting_for_feedback" and create a feedback request.
    pub fn request_feedback(&self, run_id: &str, prompt: &str) -> Result<FeedbackRequest> {
        let prompt = truncate_utf8(prompt, FEEDBACK_MAX_LEN);
        let id = crate::new_id();
        let now = Utc::now().to_rfc3339();

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
        };

        self.conn.execute(
            "INSERT INTO feedback_requests (id, run_id, prompt, status, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![req.id, req.run_id, req.prompt, req.status, req.created_at],
        )?;

        Ok(req)
    }

    /// Submit a response to a pending feedback request and resume the run.
    pub fn submit_feedback(&self, feedback_id: &str, response: &str) -> Result<FeedbackRequest> {
        let response = truncate_utf8(response, FEEDBACK_MAX_LEN);
        let now = Utc::now().to_rfc3339();

        // Update feedback request
        self.conn.execute(
            "UPDATE feedback_requests SET status = 'responded', response = ?1, responded_at = ?2 \
             WHERE id = ?3 AND status = 'pending'",
            params![response, now, feedback_id],
        )?;

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

        self.conn.execute(
            "UPDATE feedback_requests SET status = 'dismissed', responded_at = ?1 \
             WHERE id = ?2 AND status = 'pending'",
            params![now, feedback_id],
        )?;

        self.resume_run_after_feedback(feedback_id)?;

        Ok(())
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
}

#[cfg(test)]
mod tests {
    use super::super::setup_db;
    use super::super::AgentManager;
    use crate::agent::status::{AgentRunStatus, FeedbackStatus};

    #[test]
    fn test_request_feedback() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        assert_eq!(run.status, AgentRunStatus::Running);

        let fb = mgr
            .request_feedback(&run.id, "Should I refactor this module?")
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
            .request_feedback(&run.id, "Proceed with refactor?")
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
        let fb = mgr.request_feedback(&run.id, "Need approval").unwrap();

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

        let fb = mgr.request_feedback(&run.id, "Need input").unwrap();
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

        let fb = mgr.request_feedback(&run.id, "Approve this?").unwrap();
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

        let fb1 = mgr.request_feedback(&run.id, "Question 1").unwrap();
        mgr.submit_feedback(&fb1.id, "Answer 1").unwrap();

        let fb2 = mgr.request_feedback(&run.id, "Question 2").unwrap();

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
        let fb = mgr.request_feedback(&run.id, "Approve?").unwrap();

        conn.execute(
            "DELETE FROM agent_runs WHERE id = ?1",
            rusqlite::params![run.id],
        )
        .unwrap();

        assert!(mgr.get_feedback(&fb.id).unwrap().is_none());
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

        let req1 = mgr.request_feedback(&run.id, "question 1").unwrap();
        let req2 = mgr.request_feedback(&run.id, "question 2").unwrap();
        let req3 = mgr.request_feedback(&run.id, "question 3").unwrap();
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

        mgr.request_feedback(&run1.id, "from run 1").unwrap();
        mgr.request_feedback(&run2.id, "from run 2").unwrap();

        let pending = mgr.list_all_pending_feedback_requests().unwrap();
        assert_eq!(
            pending.len(),
            2,
            "pending requests from all runs should be returned"
        );
    }
}
