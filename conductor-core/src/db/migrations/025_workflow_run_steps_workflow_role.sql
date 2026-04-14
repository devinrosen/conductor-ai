BEGIN;
CREATE TABLE workflow_run_steps_new (
    id                TEXT PRIMARY KEY,
    workflow_run_id   TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
    step_name         TEXT NOT NULL,
    role              TEXT NOT NULL CHECK (role IN ('actor','reviewer','gate','workflow')),
    can_commit        INTEGER NOT NULL DEFAULT 0,
    condition_expr    TEXT,
    status            TEXT NOT NULL DEFAULT 'pending'
                      CHECK (status IN ('pending','running','waiting','completed','failed','skipped','timed_out')),
    child_run_id      TEXT REFERENCES agent_runs(id) ON DELETE SET NULL,
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
    structured_output TEXT
);
INSERT INTO workflow_run_steps_new SELECT
    id, workflow_run_id, step_name, role, can_commit, condition_expr,
    status, child_run_id, position, started_at, ended_at, result_text,
    condition_met, iteration, parallel_group_id, context_out, markers_out,
    retry_count, gate_type, gate_prompt, gate_timeout, gate_approved_by,
    gate_approved_at, gate_feedback, structured_output
    FROM workflow_run_steps;
DROP TABLE workflow_run_steps;
ALTER TABLE workflow_run_steps_new RENAME TO workflow_run_steps;
CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_run ON workflow_run_steps(workflow_run_id);
COMMIT;
