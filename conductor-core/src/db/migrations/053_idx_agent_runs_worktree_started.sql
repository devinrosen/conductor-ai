-- Covering index to speed up the latest-run-per-worktree subquery used by
-- list_all_with_status(). Without this index the GROUP BY + JOIN scans every
-- row in agent_runs for each GET /api/worktrees poll.
CREATE INDEX IF NOT EXISTS idx_agent_runs_worktree_started
    ON agent_runs(worktree_id, started_at);
