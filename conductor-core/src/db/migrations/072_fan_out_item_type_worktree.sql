-- Migration 072: Add 'worktree' to item_type CHECK constraint on workflow_run_step_fan_out_items.
-- SQLite cannot ALTER CHECK constraints in place — uses the table-swap pattern.
-- Foreign keys must be disabled during the swap (the table references workflow_run_steps).

CREATE TABLE workflow_run_step_fan_out_items_new (
    id            TEXT PRIMARY KEY,
    step_run_id   TEXT NOT NULL REFERENCES workflow_run_steps(id) ON DELETE CASCADE,
    item_type     TEXT NOT NULL CHECK (item_type IN ('ticket', 'repo', 'workflow_run', 'worktree')),
    item_id       TEXT NOT NULL,
    item_ref      TEXT NOT NULL,
    child_run_id  TEXT,
    status        TEXT NOT NULL DEFAULT 'pending'
                  CHECK (status IN ('pending', 'running', 'completed', 'failed', 'skipped')),
    dispatched_at TEXT,
    completed_at  TEXT,
    UNIQUE (step_run_id, item_type, item_id)
);

INSERT INTO workflow_run_step_fan_out_items_new
    SELECT id, step_run_id, item_type, item_id, item_ref, child_run_id, status, dispatched_at, completed_at
    FROM workflow_run_step_fan_out_items;

DROP TABLE workflow_run_step_fan_out_items;
ALTER TABLE workflow_run_step_fan_out_items_new RENAME TO workflow_run_step_fan_out_items;

CREATE INDEX idx_fan_out_items_step ON workflow_run_step_fan_out_items(step_run_id, status);
