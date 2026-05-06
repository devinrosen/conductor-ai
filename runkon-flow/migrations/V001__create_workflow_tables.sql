-- V001: Canonical engine-essential workflow schema for runkon-flow.
--
-- Frozen as of 0.16.0. Refinery checksum-validates this file on every run;
-- editing it after publication will fail every existing DB on boot.
-- Future schema changes must ship as V002+ with ALTER TABLE.
--
-- Deliberate omissions:
--   - No conductor FKs (worktree_id, ticket_id, repo_id, agent_runs ref).
--   - No harness-specific CHECK constraints on trigger, status, item_type, role.
--   - parent_run_id is plain TEXT (points at conductor's agent_runs — no FK).
--   - child_run_id is FK-less per RFC decision 11.
--
-- CREATE TABLE IF NOT EXISTS makes V001 a no-op on databases already
-- populated by conductor's 020/021 migration sequence.

CREATE TABLE IF NOT EXISTS workflow_runs (
    id                               TEXT PRIMARY KEY,
    workflow_name                    TEXT NOT NULL,
    parent_run_id                    TEXT NOT NULL,
    status                           TEXT NOT NULL DEFAULT 'pending',
    dry_run                          INTEGER NOT NULL DEFAULT 0,
    trigger                          TEXT NOT NULL DEFAULT 'manual',
    started_at                       TEXT NOT NULL,
    ended_at                         TEXT,
    result_summary                   TEXT,
    definition_snapshot              TEXT,
    inputs                           TEXT,
    parent_workflow_run_id           TEXT,
    iteration                        INTEGER NOT NULL DEFAULT 0,
    blocked_on                       TEXT,
    total_input_tokens               INTEGER,
    total_output_tokens              INTEGER,
    total_cache_read_input_tokens    INTEGER,
    total_cache_creation_input_tokens INTEGER,
    total_turns                      INTEGER,
    total_cost_usd                   REAL,
    total_duration_ms                INTEGER,
    model                            TEXT,
    error                            TEXT,
    dismissed                        INTEGER NOT NULL DEFAULT 0,
    workflow_title                   TEXT,
    owner_token                      TEXT,
    lease_until                      TEXT,
    generation                       INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS workflow_run_steps (
    id                TEXT PRIMARY KEY,
    workflow_run_id   TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
    step_name         TEXT NOT NULL,
    role              TEXT NOT NULL,
    can_commit        INTEGER NOT NULL DEFAULT 0,
    condition_expr    TEXT,
    status            TEXT NOT NULL DEFAULT 'pending',
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
    fan_out_total     INTEGER,
    fan_out_completed INTEGER NOT NULL DEFAULT 0,
    fan_out_failed    INTEGER NOT NULL DEFAULT 0,
    fan_out_skipped   INTEGER NOT NULL DEFAULT 0,
    step_error        TEXT
);

CREATE TABLE IF NOT EXISTS workflow_run_step_fan_out_items (
    id            TEXT PRIMARY KEY,
    step_run_id   TEXT NOT NULL REFERENCES workflow_run_steps(id) ON DELETE CASCADE,
    item_type     TEXT NOT NULL,
    item_id       TEXT NOT NULL,
    item_ref      TEXT NOT NULL,
    child_run_id  TEXT,
    status        TEXT NOT NULL DEFAULT 'pending',
    dispatched_at TEXT,
    completed_at  TEXT,
    context       TEXT,
    UNIQUE (step_run_id, item_type, item_id)
);

CREATE INDEX IF NOT EXISTS idx_workflow_runs_parent
    ON workflow_runs(parent_run_id);

CREATE INDEX IF NOT EXISTS idx_workflow_runs_parent_wf
    ON workflow_runs(parent_workflow_run_id);

CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_run
    ON workflow_run_steps(workflow_run_id);

CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_status_gate
    ON workflow_run_steps(status, gate_type);

CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_child_run_id
    ON workflow_run_steps(child_run_id);

CREATE INDEX IF NOT EXISTS idx_fan_out_items_step
    ON workflow_run_step_fan_out_items(step_run_id, status);
