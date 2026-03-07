-- Workflow run tracking: maps a workflow execution to its agent run steps.
CREATE TABLE IF NOT EXISTS workflow_runs (
    id              TEXT PRIMARY KEY,
    workflow_name   TEXT NOT NULL,
    worktree_id     TEXT NOT NULL REFERENCES worktrees(id) ON DELETE CASCADE,
    parent_run_id   TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending','running','completed','failed','cancelled')),
    dry_run         INTEGER NOT NULL DEFAULT 0,
    trigger         TEXT NOT NULL DEFAULT 'manual',
    started_at      TEXT NOT NULL,
    ended_at        TEXT,
    result_summary  TEXT
);

CREATE INDEX IF NOT EXISTS idx_workflow_runs_worktree ON workflow_runs(worktree_id);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_parent ON workflow_runs(parent_run_id);

-- Individual workflow step execution records.
-- Links a workflow step definition to its agent_run_steps entry and optional child agent run.
CREATE TABLE IF NOT EXISTS workflow_run_steps (
    id              TEXT PRIMARY KEY,
    workflow_run_id TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
    step_name       TEXT NOT NULL,
    role            TEXT NOT NULL CHECK (role IN ('reviewer','actor')),
    can_commit      INTEGER NOT NULL DEFAULT 0,
    condition_expr  TEXT,
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending','running','completed','failed','skipped')),
    child_run_id    TEXT REFERENCES agent_runs(id) ON DELETE SET NULL,
    position        INTEGER NOT NULL,
    started_at      TEXT,
    ended_at        TEXT,
    result_text     TEXT,
    condition_met   INTEGER
);

CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_run ON workflow_run_steps(workflow_run_id);
