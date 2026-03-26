use chrono::Utc;
use rusqlite::params;

use crate::db::query_collect;
use crate::error::Result;

use super::super::db::{
    row_to_agent_created_issue, row_to_agent_run_event, AGENT_CREATED_ISSUES_SELECT,
    AGENT_RUN_EVENTS_SELECT,
};
use super::super::types::{AgentCreatedIssue, AgentRunEvent};
use super::AgentManager;

impl<'a> AgentManager<'a> {
    /// Persist a new event span for a run. Returns the created event.
    pub fn create_event(
        &self,
        run_id: &str,
        kind: &str,
        summary: &str,
        started_at: &str,
        metadata: Option<&str>,
    ) -> Result<AgentRunEvent> {
        let id = crate::new_id();
        let event = AgentRunEvent {
            id: id.clone(),
            run_id: run_id.to_string(),
            kind: kind.to_string(),
            summary: summary.to_string(),
            started_at: started_at.to_string(),
            ended_at: None,
            metadata: metadata.map(String::from),
        };
        self.conn.execute(
            "INSERT INTO agent_run_events (id, run_id, kind, summary, started_at, metadata) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.id,
                event.run_id,
                event.kind,
                event.summary,
                event.started_at,
                event.metadata
            ],
        )?;
        Ok(event)
    }

    /// Set the ended_at timestamp for a previously created event span.
    pub fn update_event_ended_at(&self, event_id: &str, ended_at: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE agent_run_events SET ended_at = ?1 WHERE id = ?2",
            params![ended_at, event_id],
        )?;
        Ok(())
    }

    /// List all events for a run in chronological order.
    pub fn list_events_for_run(&self, run_id: &str) -> Result<Vec<AgentRunEvent>> {
        query_collect(
            self.conn,
            &format!("{AGENT_RUN_EVENTS_SELECT} WHERE run_id = ?1 ORDER BY started_at ASC"),
            params![run_id],
            row_to_agent_run_event,
        )
    }

    /// List all events across all runs for a worktree, in chronological order.
    pub fn list_events_for_worktree(&self, worktree_id: &str) -> Result<Vec<AgentRunEvent>> {
        // Cannot use AGENT_RUN_EVENTS_SELECT here: the JOIN requires the `e.` alias.
        query_collect(
            self.conn,
            "SELECT e.id, e.run_id, e.kind, e.summary, e.started_at, e.ended_at, e.metadata \
             FROM agent_run_events e \
             JOIN agent_runs r ON e.run_id = r.id \
             WHERE r.worktree_id = ?1 \
             ORDER BY e.started_at ASC",
            params![worktree_id],
            row_to_agent_run_event,
        )
    }

    /// Return all worktree-scoped agent events, grouped by `worktree_id`.
    /// Single SQL JOIN — no per-worktree round trips.
    pub fn list_all_events_by_worktree(&self) -> Result<std::collections::HashMap<String, Vec<AgentRunEvent>>> {
        let rows = query_collect(
            self.conn,
            "SELECT e.id, e.run_id, e.kind, e.summary, e.started_at, e.ended_at, e.metadata, r.worktree_id \
             FROM agent_run_events e \
             JOIN agent_runs r ON e.run_id = r.id \
             WHERE r.worktree_id IS NOT NULL \
             ORDER BY r.worktree_id, e.started_at ASC",
            [],
            |row| {
                let event = row_to_agent_run_event(row)?;
                let wt_id: String = row.get(7)?;
                Ok((wt_id, event))
            },
        )?;
        let mut map: std::collections::HashMap<String, Vec<AgentRunEvent>> = std::collections::HashMap::new();
        for (wt_id, event) in rows {
            map.entry(wt_id).or_default().push(event);
        }
        Ok(map)
    }

    /// Return all repo-scoped agent events, grouped by `repo_id`.
    /// Only includes runs where `worktree_id IS NULL` (repo-level agents).
    pub fn list_all_repo_events_by_repo(&self) -> Result<std::collections::HashMap<String, Vec<AgentRunEvent>>> {
        let rows = query_collect(
            self.conn,
            "SELECT e.id, e.run_id, e.kind, e.summary, e.started_at, e.ended_at, e.metadata, r.repo_id \
             FROM agent_run_events e \
             JOIN agent_runs r ON e.run_id = r.id \
             WHERE r.worktree_id IS NULL \
             ORDER BY r.repo_id, e.started_at ASC",
            [],
            |row| {
                let event = row_to_agent_run_event(row)?;
                let repo_id: String = row.get(7)?;
                Ok((repo_id, event))
            },
        )?;
        let mut map: std::collections::HashMap<String, Vec<AgentRunEvent>> = std::collections::HashMap::new();
        for (repo_id, event) in rows {
            map.entry(repo_id).or_default().push(event);
        }
        Ok(map)
    }

    /// List all events across repo-scoped runs for a repo, in chronological order.
    /// Only includes runs where `worktree_id IS NULL` (repo-level agents).
    pub fn list_events_for_repo(&self, repo_id: &str) -> Result<Vec<AgentRunEvent>> {
        query_collect(
            self.conn,
            "SELECT e.id, e.run_id, e.kind, e.summary, e.started_at, e.ended_at, e.metadata \
             FROM agent_run_events e \
             JOIN agent_runs r ON e.run_id = r.id \
             WHERE r.repo_id = ?1 AND r.worktree_id IS NULL \
             ORDER BY e.started_at ASC",
            params![repo_id],
            row_to_agent_run_event,
        )
    }

    /// Record a GitHub issue created by an agent run.
    pub fn record_created_issue(
        &self,
        agent_run_id: &str,
        repo_id: &str,
        source_type: &str,
        source_id: &str,
        title: &str,
        url: &str,
    ) -> Result<AgentCreatedIssue> {
        let id = crate::new_id();
        let now = Utc::now().to_rfc3339();

        let issue = AgentCreatedIssue {
            id: id.clone(),
            agent_run_id: agent_run_id.to_string(),
            repo_id: repo_id.to_string(),
            source_type: source_type.to_string(),
            source_id: source_id.to_string(),
            title: title.to_string(),
            url: url.to_string(),
            created_at: now.clone(),
        };

        self.conn.execute(
            "INSERT INTO agent_created_issues \
             (id, agent_run_id, repo_id, source_type, source_id, title, url, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                issue.id,
                issue.agent_run_id,
                issue.repo_id,
                issue.source_type,
                issue.source_id,
                issue.title,
                issue.url,
                issue.created_at,
            ],
        )?;

        Ok(issue)
    }

    /// List all issues created by a specific agent run.
    pub fn list_created_issues_for_run(
        &self,
        agent_run_id: &str,
    ) -> Result<Vec<AgentCreatedIssue>> {
        query_collect(
            self.conn,
            &format!(
                "{AGENT_CREATED_ISSUES_SELECT} WHERE agent_run_id = ?1 ORDER BY created_at ASC"
            ),
            params![agent_run_id],
            row_to_agent_created_issue,
        )
    }

    /// List all issues created by all runs for a worktree.
    pub fn list_created_issues_for_worktree(
        &self,
        worktree_id: &str,
    ) -> Result<Vec<AgentCreatedIssue>> {
        // Cannot use AGENT_CREATED_ISSUES_SELECT here: the JOIN requires the `aci.` alias.
        query_collect(
            self.conn,
            "SELECT aci.id, aci.agent_run_id, aci.repo_id, aci.source_type, \
             aci.source_id, aci.title, aci.url, aci.created_at \
             FROM agent_created_issues aci \
             JOIN agent_runs ar ON aci.agent_run_id = ar.id \
             WHERE ar.worktree_id = ?1 \
             ORDER BY aci.created_at ASC",
            params![worktree_id],
            row_to_agent_created_issue,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::setup_db;
    use super::super::AgentManager;

    #[test]
    fn test_create_and_list_events() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let t0 = "2024-01-01T00:00:00Z";
        let t1 = "2024-01-01T00:00:02Z";
        let t2 = "2024-01-01T00:00:05Z";

        let ev1 = mgr
            .create_event(&run.id, "system", "Session started", t0, None)
            .unwrap();
        let ev2 = mgr
            .create_event(&run.id, "tool", "[Bash] cargo build", t1, None)
            .unwrap();
        mgr.update_event_ended_at(&ev1.id, t1).unwrap();
        mgr.update_event_ended_at(&ev2.id, t2).unwrap();

        let events = mgr.list_events_for_run(&run.id).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "system");
        assert_eq!(events[0].ended_at.as_deref(), Some(t1));
        assert_eq!(events[1].kind, "tool");
        assert_eq!(events[1].summary, "[Bash] cargo build");
        assert_eq!(events[1].ended_at.as_deref(), Some(t2));

        // duration_ms computed from timestamps
        let dur = events[1].duration_ms().unwrap();
        assert_eq!(dur, 3000);
    }

    #[test]
    fn test_list_events_for_worktree() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run1 = mgr
            .create_run(Some("w1"), "First task", None, None)
            .unwrap();
        let run2 = mgr
            .create_run(Some("w1"), "Second task", None, None)
            .unwrap();
        let run3 = mgr
            .create_run(Some("w2"), "Other task", None, None)
            .unwrap();

        let t = "2024-01-01T00:00:00Z";
        mgr.create_event(&run1.id, "text", "Planning", t, None)
            .unwrap();
        mgr.create_event(&run1.id, "tool", "[Read] file.rs", t, None)
            .unwrap();
        mgr.create_event(&run2.id, "result", "$0.0010 · 1 turns · 1.0s", t, None)
            .unwrap();
        // run3 belongs to a different worktree
        mgr.create_event(&run3.id, "text", "Other wt event", t, None)
            .unwrap();

        let w1_events = mgr.list_events_for_worktree("w1").unwrap();
        assert_eq!(w1_events.len(), 3);

        let w2_events = mgr.list_events_for_worktree("w2").unwrap();
        assert_eq!(w2_events.len(), 1);
        assert_eq!(w2_events[0].summary, "Other wt event");
    }

    #[test]
    fn test_prompt_event_appears_first() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let prompt_text = "Fix the login bug";
        let run = mgr.create_run(Some("w1"), prompt_text, None, None).unwrap();

        let t0 = "2024-01-01T00:00:00Z";
        let t1 = "2024-01-01T00:00:01Z";
        let t2 = "2024-01-01T00:00:05Z";

        mgr.create_event(&run.id, "prompt", prompt_text, t0, None)
            .unwrap();
        mgr.create_event(&run.id, "system", "Session started", t1, None)
            .unwrap();
        mgr.create_event(&run.id, "tool", "[Bash] cargo test", t2, None)
            .unwrap();

        let events = mgr.list_events_for_run(&run.id).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].kind, "prompt");
        assert_eq!(events[0].summary, prompt_text);
        assert_eq!(events[1].kind, "system");
        assert_eq!(events[2].kind, "tool");

        let wt_events = mgr.list_events_for_worktree("w1").unwrap();
        assert_eq!(wt_events[0].kind, "prompt");
        assert_eq!(wt_events[0].run_id, run.id);
    }

    #[test]
    fn test_events_cascade_delete_on_run_removal() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let t = "2024-01-01T00:00:00Z";
        mgr.create_event(&run.id, "text", "hello", t, None).unwrap();
        mgr.create_event(&run.id, "tool", "[Bash] ls", t, None)
            .unwrap();

        conn.execute(
            "DELETE FROM agent_runs WHERE id = ?1",
            rusqlite::params![run.id],
        )
        .unwrap();

        let events = mgr.list_events_for_run(&run.id).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_record_and_list_created_issues() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run1 = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let run2 = mgr
            .create_run(Some("w2"), "Other task", None, None)
            .unwrap();

        // No issues yet
        assert!(mgr
            .list_created_issues_for_run(&run1.id)
            .unwrap()
            .is_empty());
        assert!(mgr
            .list_created_issues_for_worktree("w1")
            .unwrap()
            .is_empty());

        let issue1 = mgr
            .record_created_issue(
                &run1.id,
                "r1",
                "github",
                "101",
                "Found a memory leak",
                "https://github.com/test/repo/issues/101",
            )
            .unwrap();
        let issue2 = mgr
            .record_created_issue(
                &run1.id,
                "r1",
                "github",
                "102",
                "Needs follow-up refactor",
                "https://github.com/test/repo/issues/102",
            )
            .unwrap();

        mgr.record_created_issue(
            &run2.id,
            "r1",
            "github",
            "103",
            "Another issue",
            "https://github.com/test/repo/issues/103",
        )
        .unwrap();

        let run1_issues = mgr.list_created_issues_for_run(&run1.id).unwrap();
        assert_eq!(run1_issues.len(), 2);
        assert_eq!(run1_issues[0].source_id, "101");
        assert_eq!(run1_issues[1].source_id, "102");
        assert_eq!(run1_issues[0].title, "Found a memory leak");

        let w1_issues = mgr.list_created_issues_for_worktree("w1").unwrap();
        assert_eq!(w1_issues.len(), 2);
        assert_eq!(w1_issues[0].id, issue1.id);
        assert_eq!(w1_issues[1].id, issue2.id);

        let w2_issues = mgr.list_created_issues_for_worktree("w2").unwrap();
        assert_eq!(w2_issues.len(), 1);
        assert_eq!(w2_issues[0].source_id, "103");
    }

    #[test]
    fn test_list_events_for_repo() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);
        let t = "2024-01-01T00:00:00Z";

        // Create repo-scoped runs for r1
        let repo_run1 = mgr
            .create_repo_run("r1", "Repo task 1", None, None)
            .unwrap();
        let repo_run2 = mgr
            .create_repo_run("r1", "Repo task 2", None, None)
            .unwrap();

        // Create a worktree-scoped run for the same repo — should be excluded
        let wt_run = mgr.create_run(Some("w1"), "WT task", None, None).unwrap();

        mgr.create_event(&repo_run1.id, "text", "Planning repo", t, None)
            .unwrap();
        mgr.create_event(&repo_run2.id, "tool", "[Read] file.rs", t, None)
            .unwrap();
        mgr.create_event(&wt_run.id, "text", "WT event", t, None)
            .unwrap();

        let events = mgr.list_events_for_repo("r1").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].summary, "Planning repo");
        assert_eq!(events[1].summary, "[Read] file.rs");
    }
}
