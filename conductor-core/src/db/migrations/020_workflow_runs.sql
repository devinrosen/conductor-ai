-- Workflow run tracking: maps a workflow execution to its agent run steps.
-- If workflow_runs exists but lacks worktree_id (e.g., created by runkon's V001),
-- drop and recreate with the full conductor schema.
DROP TABLE IF EXISTS workflow_runs_new;
CREATE TABLE workflow_runs_new (
    id              TEXT PRIMARY KEY,
    workflow_name   TEXT NOT NULL,
    worktree_id     TEXT NOT NULL REFERENCES worktrees(id) ON DELETE CASCADE,
    parent_run_id   TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending','running','waiting','completed','failed','cancelled')),
    dry_run         INTEGER NOT NULL DEFAULT 0,
    trigger         TEXT NOT NULL DEFAULT 'manual'
                    CHECK (trigger IN ('manual','pr','scheduled')),
    started_at      TEXT NOT NULL,
    ended_at        TEXT,
    result_summary  TEXT
);

-- Migrate existing data if present (copy what we can; worktree_id defaults to NULL)
INSERT INTO workflow_runs_new (id, workflow_name, parent_run_id, status, dry_run, trigger, started_at, ended_at, result_summary)
SELECT id, workflow_name, parent_run_id, status, dry_run, trigger, started_at, ended_at, result_summary
FROM workflow_runs
WHERE EXISTS (SELECT 1 FROM workflow_runs)
ON CONFLICT(id) DO NOTHING;

DROP TABLE IF EXISTS workflow_runs;
ALTER TABLE workflow_runs_new RENAME TO workflow_runs;

CREATE INDEX IF NOT EXISTS idx_workflow_runs_worktree ON workflow_runs(worktree_id);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_parent ON workflow_runs(parent_run_id);

-- Individual workflow step execution records.
-- Links a workflow step definition to its agent_run_steps entry and optional child agent run.
CREATE TABLE IF NOT EXISTS workflow_run_steps (
    id              TEXT PRIMARY KEY,
    workflow_run_id TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
    step_name       TEXT NOT NULL,
    role            TEXT NOT NULL CHECK (role IN ('actor','reviewer','gate')),
    can_commit      INTEGER NOT NULL DEFAULT 0,
    condition_expr  TEXT,
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending','running','waiting','completed','failed','skipped')),
    child_run_id    TEXT REFERENCES agent_runs(id) ON DELETE SET NULL,
    position        INTEGER NOT NULL,
    started_at      TEXT,
    ended_at        TEXT,
    result_text     TEXT,
    condition_met   INTEGER
);

CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_run ON workflow_run_steps(workflow_run_id);
