-- Migration 016: add merge_queue table for serializing parallel agent merges.
-- Each entry represents a worktree+run whose changes should be landed on a
-- target branch (typically main) by a dedicated "refinery" agent.

CREATE TABLE IF NOT EXISTS merge_queue (
    id            TEXT PRIMARY KEY,
    repo_id       TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    worktree_id   TEXT NOT NULL REFERENCES worktrees(id) ON DELETE CASCADE,
    run_id        TEXT REFERENCES agent_runs(id) ON DELETE SET NULL,
    target_branch TEXT NOT NULL DEFAULT 'main',
    position      INTEGER NOT NULL,
    status        TEXT NOT NULL DEFAULT 'queued'
                  CHECK (status IN ('queued', 'processing', 'merged', 'failed')),
    queued_at     TEXT NOT NULL,
    started_at    TEXT,
    completed_at  TEXT
);

CREATE INDEX IF NOT EXISTS idx_merge_queue_repo_id ON merge_queue(repo_id);
CREATE INDEX IF NOT EXISTS idx_merge_queue_worktree_id ON merge_queue(worktree_id);
CREATE INDEX IF NOT EXISTS idx_merge_queue_status ON merge_queue(status);
