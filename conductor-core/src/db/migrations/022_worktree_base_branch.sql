-- Store the base branch a worktree was created from.
-- NULL means the repo's default branch at creation time (backwards-compatible).
ALTER TABLE worktrees ADD COLUMN base_branch TEXT;
