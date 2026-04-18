use std::collections::HashMap;

use crate::error::Result;

use super::super::context::PR_REVIEW_SWARM_PROMPT_PREFIX;
use super::super::types::{ActiveAgentCounts, CostPhase, RunTreeTotals, TicketAgentTotals};
use super::AgentManager;

impl<'a> AgentManager<'a> {
    /// Shared implementation for ticket-level aggregation with optional repo filter.
    fn totals_by_ticket_inner(
        &self,
        repo_id: Option<&str>,
    ) -> Result<HashMap<String, TicketAgentTotals>> {
        let (where_clause, param_values): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) =
            match repo_id {
                Some(id) => (
                    "WHERE w.ticket_id IS NOT NULL AND a.status = 'completed' AND w.repo_id = ?",
                    vec![Box::new(id.to_string())],
                ),
                None => (
                    "WHERE w.ticket_id IS NOT NULL AND a.status = 'completed'",
                    vec![],
                ),
            };

        let sql = format!(
            "SELECT w.ticket_id, \
                    COUNT(*) AS total_runs, \
                    COALESCE(SUM(a.cost_usd), 0.0) AS total_cost, \
                    COALESCE(SUM(a.num_turns), 0) AS total_turns, \
                    COALESCE(SUM(a.duration_ms), 0) AS total_duration_ms, \
                    COALESCE(SUM(a.input_tokens), 0) AS total_input_tokens, \
                    COALESCE(SUM(a.output_tokens), 0) AS total_output_tokens, \
                    COALESCE(SUM(a.cache_read_input_tokens), 0) AS total_cache_read_tokens, \
                    COALESCE(SUM(a.cache_creation_input_tokens), 0) AS total_cache_creation_tokens \
             FROM agent_runs a \
             JOIN worktrees w ON a.worktree_id = w.id \
             {where_clause} \
             GROUP BY w.ticket_id"
        );

        let mut stmt = self.conn.prepare_cached(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok(TicketAgentTotals {
                ticket_id: row.get("ticket_id")?,
                total_runs: row.get("total_runs")?,
                total_cost: row.get("total_cost")?,
                total_turns: row.get("total_turns")?,
                total_duration_ms: row.get("total_duration_ms")?,
                total_input_tokens: row.get("total_input_tokens")?,
                total_output_tokens: row.get("total_output_tokens")?,
                total_cache_read_tokens: row.get("total_cache_read_tokens")?,
                total_cache_creation_tokens: row.get("total_cache_creation_tokens")?,
            })
        })?;

        let mut map = HashMap::new();
        for totals in rows {
            let totals = totals?;
            map.insert(totals.ticket_id.clone(), totals);
        }
        Ok(map)
    }

    /// Returns aggregated agent stats per ticket (across all linked worktrees).
    /// Only includes completed runs with recorded metrics.
    pub fn totals_by_ticket_all(&self) -> Result<HashMap<String, TicketAgentTotals>> {
        self.totals_by_ticket_inner(None)
    }

    /// Returns aggregated agent stats per ticket for a specific repo.
    /// Only includes completed runs with recorded metrics.
    pub fn totals_by_ticket_for_repo(
        &self,
        repo_id: &str,
    ) -> Result<HashMap<String, TicketAgentTotals>> {
        self.totals_by_ticket_inner(Some(repo_id))
    }

    /// Build a per-phase cost breakdown for all runs in a worktree.
    ///
    /// Top-level runs (no parent) are classified as either "Initial run" or
    /// "Review fix #N" based on prompt content. Each phase aggregates cost
    /// from the run tree (parent + all child/grandchild runs).
    pub fn worktree_cost_phases(&self, worktree_id: &str) -> Result<Vec<CostPhase>> {
        // Single recursive-CTE query: find all root runs and aggregate each
        // tree's cost/duration in one round-trip instead of 1+N queries.
        let mut stmt = self.conn.prepare_cached(
            "WITH RECURSIVE tree(root_id, node_id) AS ( \
                 SELECT id, id \
                 FROM agent_runs \
                 WHERE worktree_id = :worktree_id AND parent_run_id IS NULL \
                 UNION ALL \
                 SELECT t.root_id, a.id \
                 FROM agent_runs a \
                 JOIN tree t ON a.parent_run_id = t.node_id \
             ), \
             agg(root_id, total_cost, total_duration_ms) AS ( \
                 SELECT t.root_id, \
                        COALESCE(SUM(CASE WHEN a.status = 'completed' THEN a.cost_usd ELSE 0.0 END), 0.0), \
                        COALESCE(SUM(CASE WHEN a.status = 'completed' THEN a.duration_ms ELSE 0 END), 0) \
                 FROM tree t \
                 LEFT JOIN agent_runs a ON a.id = t.node_id \
                 GROUP BY t.root_id \
             ) \
             SELECT r.id, r.model, r.prompt, \
                    COALESCE(agg.total_cost, 0.0) AS total_cost, \
                    COALESCE(agg.total_duration_ms, 0) AS total_duration_ms \
             FROM agent_runs r \
             LEFT JOIN agg ON agg.root_id = r.id \
             WHERE r.worktree_id = :worktree_id AND r.parent_run_id IS NULL \
             ORDER BY r.started_at ASC",
        )?;

        let rows = stmt.query_map(
            rusqlite::named_params! { ":worktree_id": worktree_id },
            |row| {
                Ok((
                    row.get::<_, Option<String>>("model")?,  // model
                    row.get::<_, String>("prompt")?,         // prompt
                    row.get::<_, f64>("total_cost")?,        // total_cost
                    row.get::<_, i64>("total_duration_ms")?, // total_duration_ms
                ))
            },
        )?;

        let mut phases = Vec::new();
        let mut review_count = 0u32;

        for row in rows {
            let (model, prompt, cost_usd, duration_ms) = row?;
            let is_review = prompt.starts_with(PR_REVIEW_SWARM_PROMPT_PREFIX);
            let label = if is_review {
                review_count += 1;
                format!("Review #{review_count}")
            } else if review_count > 0 {
                format!("Review fix #{review_count}")
            } else {
                "Initial run".to_string()
            };

            phases.push(CostPhase {
                label,
                model,
                cost_usd,
                duration_ms,
            });
        }

        Ok(phases)
    }

    /// Compute aggregated cost/turns/duration for a run and all its descendants.
    pub fn aggregate_run_tree(&self, root_run_id: &str) -> Result<RunTreeTotals> {
        let row = self.conn.query_row(
            "WITH RECURSIVE tree(id) AS ( \
                 SELECT id FROM agent_runs WHERE id = :root_run_id \
                 UNION ALL \
                 SELECT a.id FROM agent_runs a JOIN tree t ON a.parent_run_id = t.id \
             ) \
             SELECT COUNT(*) AS total_runs, \
                    COALESCE(SUM(a.cost_usd), 0.0) AS total_cost, \
                    COALESCE(SUM(a.num_turns), 0) AS total_turns, \
                    COALESCE(SUM(a.duration_ms), 0) AS total_duration_ms, \
                    COALESCE(SUM(a.input_tokens), 0) AS total_input_tokens, \
                    COALESCE(SUM(a.output_tokens), 0) AS total_output_tokens \
             FROM agent_runs a \
             JOIN tree t ON a.id = t.id \
             WHERE a.status = 'completed'",
            rusqlite::named_params! { ":root_run_id": root_run_id },
            |row| {
                Ok(RunTreeTotals {
                    total_runs: row.get("total_runs")?,
                    total_cost: row.get("total_cost")?,
                    total_turns: row.get("total_turns")?,
                    total_duration_ms: row.get("total_duration_ms")?,
                    total_input_tokens: row.get("total_input_tokens")?,
                    total_output_tokens: row.get("total_output_tokens")?,
                })
            },
        )?;
        Ok(row)
    }

    /// Returns cumulative completed-run token totals per worktree.
    ///
    /// Only `completed` runs are included so the caller can safely add live-run
    /// tokens on top without double-counting.
    ///
    /// Returns `worktree_id -> (total_input_tokens, total_output_tokens)`.
    pub fn totals_by_worktree(&self) -> Result<HashMap<String, (i64, i64)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT a.worktree_id, \
                    COALESCE(SUM(a.input_tokens), 0) AS total_input_tokens, \
                    COALESCE(SUM(a.output_tokens), 0) AS total_output_tokens \
             FROM agent_runs a \
             WHERE a.status = 'completed' \
               AND a.worktree_id IS NOT NULL \
             GROUP BY a.worktree_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>("worktree_id")?,
                row.get::<_, i64>("total_input_tokens")?,
                row.get::<_, i64>("total_output_tokens")?,
            ))
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let (wt_id, input, output) = row?;
            map.insert(wt_id, (input, output));
        }
        Ok(map)
    }

    /// Returns counts of active agent runs (running / waiting_for_feedback) per repo_id.
    /// Repos with no active runs are absent from the map.
    pub fn active_run_counts_by_repo(&self) -> Result<HashMap<String, ActiveAgentCounts>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT w.repo_id, a.status, COUNT(*) AS cnt \
             FROM agent_runs a \
             JOIN worktrees w ON w.id = a.worktree_id \
             WHERE a.status IN ('running', 'waiting_for_feedback') \
             GROUP BY w.repo_id, a.status",
        )?;
        let rows = stmt.query_map([], |row| {
            let repo_id: String = row.get("repo_id")?;
            let status: String = row.get("status")?;
            let cnt: u32 = row.get("cnt")?;
            Ok((repo_id, status, cnt))
        })?;
        let mut map: HashMap<String, ActiveAgentCounts> = HashMap::new();
        for row in rows {
            let (repo_id, status, cnt) = row?;
            let entry = map.entry(repo_id).or_default();
            match status.as_str() {
                "running" => entry.running += cnt,
                "waiting_for_feedback" => entry.waiting += cnt,
                _ => {}
            }
        }
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::super::setup_db;
    use super::super::AgentManager;

    #[test]
    fn test_totals_by_worktree_empty() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let totals = mgr.totals_by_worktree().unwrap();
        assert!(totals.is_empty());
    }

    #[test]
    fn test_totals_by_worktree() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Two completed runs on w1
        let run1 = mgr.create_run(Some("w1"), "Task 1", None, None).unwrap();
        mgr.update_run_completed(
            &run1.id,
            None,
            None,
            None,
            None,
            None,
            Some(1000),
            Some(500),
            None,
            None,
        )
        .unwrap();

        let run2 = mgr.create_run(Some("w1"), "Task 2", None, None).unwrap();
        mgr.update_run_completed(
            &run2.id,
            None,
            None,
            None,
            None,
            None,
            Some(600),
            Some(300),
            None,
            None,
        )
        .unwrap();

        // One completed run on w2
        let run3 = mgr.create_run(Some("w2"), "Task 3", None, None).unwrap();
        mgr.update_run_completed(
            &run3.id,
            None,
            None,
            None,
            None,
            None,
            Some(400),
            Some(200),
            None,
            None,
        )
        .unwrap();

        // A running (non-completed) run on w1 — must NOT be included
        let _run4 = mgr
            .create_run(Some("w1"), "In progress", None, None)
            .unwrap();

        let totals = mgr.totals_by_worktree().unwrap();
        assert_eq!(totals.len(), 2);

        let (in1, out1) = totals["w1"];
        assert_eq!(in1, 1600);
        assert_eq!(out1, 800);

        let (in2, out2) = totals["w2"];
        assert_eq!(in2, 400);
        assert_eq!(out2, 200);
    }

    #[test]
    fn test_active_run_counts_by_repo() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // No active runs yet — map should be empty
        let counts = mgr.active_run_counts_by_repo().unwrap();
        assert!(counts.is_empty());

        // Create runs: two running in w1 (repo r1), one waiting_for_feedback in w2 (repo r1)
        let _run1 = mgr.create_run(Some("w1"), "Task 1", None, None).unwrap();
        let _run2 = mgr.create_run(Some("w1"), "Task 2", None, None).unwrap();
        let run3 = mgr.create_run(Some("w2"), "Task 3", None, None).unwrap();
        // Set run3 to waiting_for_feedback via request_feedback
        mgr.request_feedback(&run3.id, "What should I do?", None)
            .unwrap();

        // Also create a completed run — should not appear in counts
        let run4 = mgr.create_run(Some("w1"), "Task 4", None, None).unwrap();
        mgr.update_run_completed(
            &run4.id,
            None,
            Some("done"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let counts = mgr.active_run_counts_by_repo().unwrap();
        assert_eq!(counts.len(), 1);
        let r1_counts = counts.get("r1").unwrap();
        assert_eq!(r1_counts.running, 2);
        assert_eq!(r1_counts.waiting, 1);
    }

    #[test]
    fn test_active_run_counts_multiple_repos() {
        let conn = setup_db();
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
        let _r1 = mgr.create_run(Some("w1"), "r1 task", None, None).unwrap();
        let _r2 = mgr.create_run(Some("w3"), "r2 task", None, None).unwrap();

        let counts = mgr.active_run_counts_by_repo().unwrap();
        assert_eq!(counts.len(), 2);
        assert_eq!(counts["r1"].running, 1);
        assert_eq!(counts["r1"].waiting, 0);
        assert_eq!(counts["r2"].running, 1);
        assert_eq!(counts["r2"].waiting, 0);
    }

    #[test]
    fn test_totals_by_ticket_all() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
             VALUES ('t1', 'r1', 'github', '42', 'Test ticket', '', 'open', '', 'https://example.com', '2024-01-01T00:00:00Z', '{}')",
            [],
        ).unwrap();
        conn.execute("UPDATE worktrees SET ticket_id = 't1' WHERE id = 'w1'", [])
            .unwrap();
        conn.execute("UPDATE worktrees SET ticket_id = 't1' WHERE id = 'w2'", [])
            .unwrap();

        let run1 = mgr
            .create_run(Some("w1"), "First task", None, None)
            .unwrap();
        mgr.update_run_completed(
            &run1.id,
            None,
            None,
            Some(0.10),
            Some(5),
            Some(30000),
            Some(1000),
            Some(500),
            None,
            None,
        )
        .unwrap();
        let run2 = mgr
            .create_run(Some("w1"), "Second task", None, None)
            .unwrap();
        mgr.update_run_completed(
            &run2.id,
            None,
            None,
            Some(0.05),
            Some(3),
            Some(15000),
            Some(600),
            Some(300),
            None,
            None,
        )
        .unwrap();
        let run3 = mgr
            .create_run(Some("w2"), "Third task", None, None)
            .unwrap();
        mgr.update_run_completed(
            &run3.id,
            None,
            None,
            Some(0.08),
            Some(4),
            Some(20000),
            Some(400),
            Some(200),
            None,
            None,
        )
        .unwrap();

        let _run4 = mgr
            .create_run(Some("w1"), "In progress", None, None)
            .unwrap();

        let totals = mgr.totals_by_ticket_all().unwrap();
        assert_eq!(totals.len(), 1);

        let t1 = totals.get("t1").unwrap();
        assert_eq!(t1.total_runs, 3);
        assert!((t1.total_cost - 0.23).abs() < 0.001);
        assert_eq!(t1.total_turns, 12);
        assert_eq!(t1.total_duration_ms, 65000);
        assert_eq!(t1.total_input_tokens, 2000);
        assert_eq!(t1.total_output_tokens, 1000);
    }

    #[test]
    fn test_totals_by_ticket_empty() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let totals = mgr.totals_by_ticket_all().unwrap();
        assert!(totals.is_empty());
    }

    #[test]
    fn test_aggregate_run_tree() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let parent = mgr
            .create_run(Some("w1"), "Supervisor", None, None)
            .unwrap();
        mgr.update_run_completed(
            &parent.id,
            None,
            None,
            Some(0.10),
            Some(5),
            Some(30000),
            Some(1000),
            Some(500),
            None,
            None,
        )
        .unwrap();

        let child1 = mgr
            .create_child_run(Some("w1"), "Child 1", None, None, &parent.id, None)
            .unwrap();
        mgr.update_run_completed(
            &child1.id,
            None,
            None,
            Some(0.05),
            Some(3),
            Some(15000),
            Some(600),
            Some(300),
            None,
            None,
        )
        .unwrap();

        let child2 = mgr
            .create_child_run(Some("w2"), "Child 2", None, None, &parent.id, None)
            .unwrap();
        mgr.update_run_completed(
            &child2.id,
            None,
            None,
            Some(0.08),
            Some(4),
            Some(20000),
            Some(400),
            Some(200),
            None,
            None,
        )
        .unwrap();

        let _running = mgr
            .create_child_run(Some("w1"), "Still running", None, None, &parent.id, None)
            .unwrap();

        let totals = mgr.aggregate_run_tree(&parent.id).unwrap();
        assert_eq!(totals.total_runs, 3);
        assert!((totals.total_cost - 0.23).abs() < 0.001);
        assert_eq!(totals.total_turns, 12);
        assert_eq!(totals.total_duration_ms, 65000);
        assert_eq!(totals.total_input_tokens, 2000);
        assert_eq!(totals.total_output_tokens, 1000);
    }

    #[test]
    fn test_worktree_cost_phases_empty() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let phases = mgr.worktree_cost_phases("w1").unwrap();
        assert!(phases.is_empty());
    }

    #[test]
    fn test_worktree_cost_phases_initial_run() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, Some("claude-sonnet-4-6"))
            .unwrap();
        mgr.update_run_completed(
            &run.id,
            None,
            Some("done"),
            Some(0.031),
            Some(10),
            Some(492_000),
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let phases = mgr.worktree_cost_phases("w1").unwrap();
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0].label, "Initial run");
        assert_eq!(phases[0].model.as_deref(), Some("claude-sonnet-4-6"));
        assert!((phases[0].cost_usd - 0.031).abs() < 1e-6);
        assert_eq!(phases[0].duration_ms, 492_000);
    }

    #[test]
    fn test_worktree_cost_phases_with_review() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run1 = mgr
            .create_run(Some("w1"), "Implement feature", None, Some("sonnet"))
            .unwrap();
        mgr.update_run_completed(
            &run1.id,
            None,
            Some("done"),
            Some(0.03),
            Some(10),
            Some(400_000),
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let review = mgr
            .create_run(
                Some("w1"),
                "PR review swarm for branch 'feat/test'. Coordinating 2 reviewer agents.",
                None,
                Some("haiku"),
            )
            .unwrap();
        let child = mgr
            .create_child_run(
                Some("w1"),
                "Review correctness",
                None,
                Some("haiku"),
                &review.id,
                None,
            )
            .unwrap();
        mgr.update_run_completed(
            &child.id,
            None,
            Some("approved"),
            Some(0.002),
            Some(3),
            Some(60_000),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        mgr.update_run_completed(
            &review.id,
            None,
            Some("all approved"),
            Some(0.001),
            Some(1),
            Some(70_000),
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let phases = mgr.worktree_cost_phases("w1").unwrap();
        assert_eq!(phases.len(), 2);
        assert_eq!(phases[0].label, "Initial run");
        assert_eq!(phases[1].label, "Review #1");
        assert!((phases[1].cost_usd - 0.003).abs() < 1e-6);
    }

    #[test]
    fn test_worktree_cost_phases_review_fix() {
        // Exercises the `else if review_count > 0` branch: a non-review run that
        // follows a review run should be labelled "Review fix #N".
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // 1. Initial implementation run
        let run1 = mgr
            .create_run(Some("w1"), "Implement the feature", None, Some("sonnet"))
            .unwrap();
        mgr.update_run_completed(
            &run1.id,
            None,
            Some("done"),
            Some(0.03),
            Some(10),
            Some(300_000),
            None,
            None,
            None,
            None,
        )
        .unwrap();

        // 2. Review run (prompt starts with PR_REVIEW_SWARM_PROMPT_PREFIX)
        let review = mgr
            .create_run(
                Some("w1"),
                "PR review swarm for branch 'feat/test'.",
                None,
                Some("haiku"),
            )
            .unwrap();
        mgr.update_run_completed(
            &review.id,
            None,
            Some("changes requested"),
            Some(0.005),
            Some(2),
            Some(60_000),
            None,
            None,
            None,
            None,
        )
        .unwrap();

        // 3. Fix run — non-review prompt after a review: triggers the `else if review_count > 0` branch
        let fix = mgr
            .create_run(Some("w1"), "Address review comments", None, Some("sonnet"))
            .unwrap();
        mgr.update_run_completed(
            &fix.id,
            None,
            Some("done"),
            Some(0.02),
            Some(6),
            Some(200_000),
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let phases = mgr.worktree_cost_phases("w1").unwrap();
        assert_eq!(phases.len(), 3);
        assert_eq!(phases[0].label, "Initial run");
        assert_eq!(phases[1].label, "Review #1");
        assert_eq!(phases[2].label, "Review fix #1");
    }

    #[test]
    fn test_worktree_cost_phases_excludes_child_runs() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let parent = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let child = mgr
            .create_child_run(Some("w1"), "Sub-task", None, None, &parent.id, None)
            .unwrap();
        mgr.update_run_completed(
            &parent.id,
            None,
            Some("done"),
            Some(0.01),
            Some(5),
            Some(100_000),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        mgr.update_run_completed(
            &child.id,
            None,
            Some("done"),
            Some(0.005),
            Some(2),
            Some(50_000),
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let phases = mgr.worktree_cost_phases("w1").unwrap();
        assert_eq!(phases.len(), 1);
        assert!((phases[0].cost_usd - 0.015).abs() < 1e-6);
    }

    #[test]
    fn test_totals_by_ticket_for_repo() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Insert a second repo with its own worktree and ticket
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

        // Create tickets for both repos
        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
             VALUES ('t1', 'r1', 'github', '42', 'R1 ticket', '', 'open', '', 'https://example.com', '2024-01-01T00:00:00Z', '{}')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
             VALUES ('t2', 'r2', 'github', '99', 'R2 ticket', '', 'open', '', 'https://example.com', '2024-01-01T00:00:00Z', '{}')",
            [],
        ).unwrap();
        conn.execute("UPDATE worktrees SET ticket_id = 't1' WHERE id = 'w1'", [])
            .unwrap();
        conn.execute("UPDATE worktrees SET ticket_id = 't2' WHERE id = 'w3'", [])
            .unwrap();

        // Create completed runs for both repos
        let run1 = mgr.create_run(Some("w1"), "r1 task", None, None).unwrap();
        mgr.update_run_completed(
            &run1.id,
            None,
            None,
            Some(0.10),
            Some(5),
            Some(30000),
            Some(1000),
            Some(500),
            None,
            None,
        )
        .unwrap();

        let run2 = mgr.create_run(Some("w3"), "r2 task", None, None).unwrap();
        mgr.update_run_completed(
            &run2.id,
            None,
            None,
            Some(0.20),
            Some(8),
            Some(60000),
            Some(2000),
            Some(1000),
            None,
            None,
        )
        .unwrap();

        // Add a non-completed (running) run for r1/t1 — should be excluded by the
        // `status = 'completed'` filter and not inflate totals.
        let _running_run = mgr
            .create_run(Some("w1"), "still running", None, None)
            .unwrap();

        // Repo r1 should only include t1, and the running run should be excluded
        let r1_totals = mgr.totals_by_ticket_for_repo("r1").unwrap();
        assert_eq!(r1_totals.len(), 1);
        let t1 = r1_totals.get("t1").unwrap();
        assert_eq!(t1.total_runs, 1);
        assert!((t1.total_cost - 0.10).abs() < 0.001);
        assert!(!r1_totals.contains_key("t2"));

        // Repo r2 should only include t2
        let r2_totals = mgr.totals_by_ticket_for_repo("r2").unwrap();
        assert_eq!(r2_totals.len(), 1);
        let t2 = r2_totals.get("t2").unwrap();
        assert_eq!(t2.total_runs, 1);
        assert!((t2.total_cost - 0.20).abs() < 0.001);
    }
}
