BEGIN;

-- Recreate workflow_runs with nullable worktree_id
CREATE TABLE workflow_runs_new (
    id                  TEXT PRIMARY KEY,
    workflow_name       TEXT NOT NULL,
    worktree_id         TEXT REFERENCES worktrees(id) ON DELETE CASCADE,
    parent_run_id       TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    status              TEXT NOT NULL DEFAULT 'pending'
                        CHECK (status IN ('pending','running','completed','failed','cancelled','waiting')),
    dry_run             INTEGER NOT NULL DEFAULT 0,
    trigger             TEXT NOT NULL DEFAULT 'manual',
    started_at          TEXT NOT NULL,
    ended_at            TEXT,
    result_summary      TEXT,
    definition_snapshot TEXT,
    inputs              TEXT
);
INSERT INTO workflow_runs_new SELECT * FROM workflow_runs;
DROP TABLE workflow_runs;
ALTER TABLE workflow_runs_new RENAME TO workflow_runs;
CREATE INDEX IF NOT EXISTS idx_workflow_runs_worktree ON workflow_runs(worktree_id);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_parent ON workflow_runs(parent_run_id);

-- Recreate agent_runs with nullable worktree_id
CREATE TABLE agent_runs_new (
    id                TEXT PRIMARY KEY,
    worktree_id       TEXT REFERENCES worktrees(id) ON DELETE CASCADE,
    claude_session_id TEXT,
    prompt            TEXT NOT NULL,
    status            TEXT NOT NULL DEFAULT 'running'
                      CHECK (status IN ('running','completed','failed','cancelled','waiting_for_feedback')),
    result_text       TEXT,
    cost_usd          REAL,
    num_turns         INTEGER,
    duration_ms       INTEGER,
    started_at        TEXT NOT NULL,
    ended_at          TEXT,
    tmux_window       TEXT,
    log_file          TEXT,
    model             TEXT,
    plan              TEXT,
    parent_run_id     TEXT REFERENCES agent_runs_new(id) ON DELETE SET NULL
);
INSERT INTO agent_runs_new SELECT * FROM agent_runs;
DROP TABLE agent_runs;
ALTER TABLE agent_runs_new RENAME TO agent_runs;
CREATE INDEX IF NOT EXISTS idx_agent_runs_parent ON agent_runs(parent_run_id);
CREATE INDEX IF NOT EXISTS idx_agent_runs_worktree ON agent_runs(worktree_id);

COMMIT;
