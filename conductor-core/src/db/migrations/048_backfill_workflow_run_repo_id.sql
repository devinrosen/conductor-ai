UPDATE workflow_runs
SET repo_id = (SELECT repo_id FROM worktrees WHERE worktrees.id = workflow_runs.worktree_id)
WHERE repo_id IS NULL AND worktree_id IS NOT NULL;
