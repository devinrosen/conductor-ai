-- Migration 067: Add workflow_run_step_fan_out_items table for foreach step type
-- Each row tracks one item in a foreach fan-out (ticket, repo, or workflow_run).
-- Counter columns on workflow_run_steps aggregate progress for display.

CREATE TABLE workflow_run_step_fan_out_items (
    id           TEXT PRIMARY KEY,
    step_run_id  TEXT NOT NULL REFERENCES workflow_run_steps(id) ON DELETE CASCADE,
    item_type    TEXT NOT NULL CHECK (item_type IN ('ticket', 'repo', 'workflow_run')),
    item_id      TEXT NOT NULL,
    item_ref     TEXT NOT NULL,
    child_run_id TEXT,   -- FK-less per RFC decision 11
    status       TEXT NOT NULL DEFAULT 'pending'
                 CHECK (status IN ('pending', 'running', 'completed', 'failed', 'skipped')),
    dispatched_at TEXT,
    completed_at  TEXT,
    UNIQUE (step_run_id, item_type, item_id)
);

CREATE INDEX idx_fan_out_items_step ON workflow_run_step_fan_out_items(step_run_id, status);

ALTER TABLE workflow_run_steps ADD COLUMN fan_out_total     INTEGER;
ALTER TABLE workflow_run_steps ADD COLUMN fan_out_completed INTEGER NOT NULL DEFAULT 0;
ALTER TABLE workflow_run_steps ADD COLUMN fan_out_failed    INTEGER NOT NULL DEFAULT 0;
ALTER TABLE workflow_run_steps ADD COLUMN fan_out_skipped   INTEGER NOT NULL DEFAULT 0;
