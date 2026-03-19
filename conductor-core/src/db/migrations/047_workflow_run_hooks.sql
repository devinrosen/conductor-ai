-- Must be run with PRAGMA foreign_keys = OFF (handled in Rust)
-- 1. Add triggered_by_hook column and widen the trigger CHECK to include 'hook'.
-- 2. SQLite cannot alter CHECK constraints in-place; table swap required.
BEGIN;

CREATE TABLE workflow_runs_new (
    id                      TEXT PRIMARY KEY,
    workflow_name           TEXT NOT NULL,
    worktree_id             TEXT REFERENCES worktrees(id) ON DELETE CASCADE,
    parent_run_id           TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    status                  TEXT NOT NULL DEFAULT 'pending'
                            CHECK (status IN ('pending','running','waiting','completed','failed','cancelled','timed_out')),
    dry_run                 INTEGER NOT NULL DEFAULT 0,
    trigger                 TEXT NOT NULL DEFAULT 'manual'
                            CHECK (trigger IN ('manual','pr','scheduled','hook')),
    started_at              TEXT NOT NULL,
    ended_at                TEXT,
    result_summary          TEXT,
    definition_snapshot     TEXT,
    inputs                  TEXT,
    ticket_id               TEXT REFERENCES tickets(id),
    repo_id                 TEXT REFERENCES repos(id),
    parent_workflow_run_id  TEXT REFERENCES workflow_runs_new(id),
    target_label            TEXT,
    default_bot_name        TEXT,
    iteration               INTEGER NOT NULL DEFAULT 0,
    blocked_on              TEXT,
    feature_id              TEXT REFERENCES features(id),
    triggered_by_hook       INTEGER NOT NULL DEFAULT 0
);

INSERT INTO workflow_runs_new
    SELECT id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger,
           started_at, ended_at, result_summary, definition_snapshot, inputs,
           ticket_id, repo_id, parent_workflow_run_id, target_label, default_bot_name,
           iteration, blocked_on, feature_id, 0
    FROM workflow_runs;

DROP TABLE workflow_runs;
ALTER TABLE workflow_runs_new RENAME TO workflow_runs;

CREATE INDEX IF NOT EXISTS idx_workflow_runs_worktree ON workflow_runs(worktree_id);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_parent ON workflow_runs(parent_run_id);

COMMIT;
