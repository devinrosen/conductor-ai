-- Add repo_id column to agent_runs for repo-scoped agents.
ALTER TABLE agent_runs ADD COLUMN repo_id TEXT REFERENCES repos(id);

-- Backfill existing rows from their worktree's repo_id.
UPDATE agent_runs SET repo_id = (
    SELECT w.repo_id FROM worktrees w WHERE w.id = agent_runs.worktree_id
) WHERE worktree_id IS NOT NULL AND repo_id IS NULL;

-- Index for queries filtering on repo_id (repo-scoped agent listing).
CREATE INDEX IF NOT EXISTS idx_agent_runs_repo_id ON agent_runs(repo_id);
