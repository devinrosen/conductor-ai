-- Backfill target_label for workflow_runs that have a worktree_id but
-- a NULL or empty target_label. These rows were created before migration 033
-- added the column, or via the TUI race condition where the in-memory cache
-- hadn't refreshed yet after worktree creation.
UPDATE workflow_runs
SET target_label = (
    SELECT repos.slug || '/' || worktrees.slug
    FROM worktrees
    JOIN repos ON worktrees.repo_id = repos.id
    WHERE worktrees.id = workflow_runs.worktree_id
)
WHERE worktree_id IS NOT NULL
  AND (target_label IS NULL OR target_label = '');
