CREATE TABLE workflow_runs_new (
    id                  TEXT PRIMARY KEY,
    workflow_name       TEXT NOT NULL,
    worktree_id         TEXT NOT NULL REFERENCES worktrees(id) ON DELETE CASCADE,
    parent_run_id       TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    status              TEXT NOT NULL DEFAULT 'pending'
                        CHECK (status IN ('pending','running','completed','failed','cancelled','waiting')),
    dry_run             INTEGER NOT NULL DEFAULT 0,
    trigger             TEXT NOT NULL DEFAULT 'manual',
    started_at          TEXT NOT NULL,
    ended_at            TEXT,
    result_summary      TEXT,
    definition_snapshot TEXT
);
INSERT INTO workflow_runs_new SELECT id, workflow_name, worktree_id, parent_run_id,
    status, dry_run, trigger, started_at, ended_at, result_summary, definition_snapshot
    FROM workflow_runs;
DROP TABLE workflow_runs;
ALTER TABLE workflow_runs_new RENAME TO workflow_runs;
CREATE INDEX IF NOT EXISTS idx_workflow_runs_worktree ON workflow_runs(worktree_id);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_parent ON workflow_runs(parent_run_id);
