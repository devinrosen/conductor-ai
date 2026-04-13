-- Migration 068: add 'foreach' to the role CHECK constraint on workflow_run_steps.
-- SQLite cannot alter CHECK constraints in-place; use table-recreation pattern (same as 058).
-- Includes ALL columns ever added: base set from 058, subprocess_pid from 065,
-- and fan_out_* columns from 067.
BEGIN;
CREATE TABLE workflow_run_steps_new (
    id                TEXT PRIMARY KEY,
    workflow_run_id   TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
    step_name         TEXT NOT NULL,
    role              TEXT NOT NULL CHECK (role IN ('actor','reviewer','gate','workflow','script','foreach')),
    can_commit        INTEGER NOT NULL DEFAULT 0,
    condition_expr    TEXT,
    status            TEXT NOT NULL DEFAULT 'pending'
                      CHECK (status IN ('pending','running','waiting','completed','failed','skipped','timed_out')),
    child_run_id      TEXT,
    position          INTEGER NOT NULL,
    started_at        TEXT,
    ended_at          TEXT,
    result_text       TEXT,
    condition_met     INTEGER,
    iteration         INTEGER NOT NULL DEFAULT 0,
    parallel_group_id TEXT,
    context_out       TEXT,
    markers_out       TEXT,
    retry_count       INTEGER NOT NULL DEFAULT 0,
    gate_type         TEXT,
    gate_prompt       TEXT,
    gate_timeout      TEXT,
    gate_approved_by  TEXT,
    gate_approved_at  TEXT,
    gate_feedback     TEXT,
    structured_output TEXT,
    output_file       TEXT,
    gate_options      TEXT,
    gate_selections   TEXT,
    subprocess_pid    INTEGER,
    fan_out_total     INTEGER,
    fan_out_completed INTEGER DEFAULT 0,
    fan_out_failed    INTEGER DEFAULT 0,
    fan_out_skipped   INTEGER DEFAULT 0
);
INSERT INTO workflow_run_steps_new SELECT
    id, workflow_run_id, step_name, role, can_commit, condition_expr,
    status, child_run_id, position, started_at, ended_at, result_text,
    condition_met, iteration, parallel_group_id, context_out, markers_out,
    retry_count, gate_type, gate_prompt, gate_timeout, gate_approved_by,
    gate_approved_at, gate_feedback, structured_output, output_file,
    gate_options, gate_selections, subprocess_pid,
    fan_out_total, fan_out_completed, fan_out_failed, fan_out_skipped
    FROM workflow_run_steps;
DROP TABLE workflow_run_steps;
ALTER TABLE workflow_run_steps_new RENAME TO workflow_run_steps;
CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_run
  ON workflow_run_steps(workflow_run_id);
CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_status_gate
  ON workflow_run_steps(status, gate_type);
COMMIT;
