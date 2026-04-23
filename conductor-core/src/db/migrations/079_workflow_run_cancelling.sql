-- Migration 079: add 'cancelling' to the workflow_runs.status CHECK constraint.
-- SQLite cannot ALTER CHECK constraints in-place; table-recreation pattern required.
-- Must be run with PRAGMA foreign_keys = OFF (handled in Rust via with_foreign_keys_off).
--
-- Columns preserved verbatim from current schema (all ALTER TABLE additions through 078):
--   base set (021): id, workflow_name, worktree_id, parent_run_id, status, dry_run,
--                   trigger, started_at, ended_at, result_summary, definition_snapshot
--   026: inputs
--   027: nullable worktree_id + ticket_id, repo_id, parent_workflow_run_id,
--        target_label, default_bot_name
--   040: iteration
--   041: blocked_on
--   044: feature_id — dropped in 073
--   047: trigger widened (hook), status widened (timed_out) — table swap
--   059: total_input_tokens, total_output_tokens, total_cache_read_input_tokens,
--        total_cache_creation_input_tokens, total_turns, total_cost_usd,
--        total_duration_ms, model
--   063: error
--   066: last_heartbeat
--   071: status widened (needs_resume) — table swap
--   075: dismissed

BEGIN;

CREATE TABLE workflow_runs_new (
    id                                TEXT PRIMARY KEY,
    workflow_name                     TEXT NOT NULL,
    worktree_id                       TEXT REFERENCES worktrees(id) ON DELETE CASCADE,
    parent_run_id                     TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    status                            TEXT NOT NULL DEFAULT 'pending'
                                      CHECK (status IN ('pending','running','waiting','completed','failed','cancelled','timed_out','needs_resume','cancelling')),
    dry_run                           INTEGER NOT NULL DEFAULT 0,
    trigger                           TEXT NOT NULL DEFAULT 'manual'
                                      CHECK (trigger IN ('manual','pr','scheduled','hook')),
    started_at                        TEXT NOT NULL,
    ended_at                          TEXT,
    result_summary                    TEXT,
    definition_snapshot               TEXT,
    inputs                            TEXT,
    ticket_id                         TEXT REFERENCES tickets(id),
    repo_id                           TEXT REFERENCES repos(id),
    parent_workflow_run_id            TEXT REFERENCES workflow_runs_new(id),
    target_label                      TEXT,
    default_bot_name                  TEXT,
    iteration                         INTEGER NOT NULL DEFAULT 0,
    blocked_on                        TEXT,
    total_input_tokens                INTEGER,
    total_output_tokens               INTEGER,
    total_cache_read_input_tokens     INTEGER,
    total_cache_creation_input_tokens INTEGER,
    total_turns                       INTEGER,
    total_cost_usd                    REAL,
    total_duration_ms                 INTEGER,
    model                             TEXT,
    error                             TEXT,
    last_heartbeat                    TEXT,
    dismissed                         INTEGER NOT NULL DEFAULT 0
);

INSERT INTO workflow_runs_new SELECT
    id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger,
    started_at, ended_at, result_summary, definition_snapshot, inputs,
    ticket_id, repo_id, parent_workflow_run_id, target_label, default_bot_name,
    iteration, blocked_on,
    total_input_tokens, total_output_tokens, total_cache_read_input_tokens,
    total_cache_creation_input_tokens, total_turns, total_cost_usd, total_duration_ms,
    model, error, last_heartbeat, dismissed
    FROM workflow_runs;

DROP TABLE workflow_runs;
ALTER TABLE workflow_runs_new RENAME TO workflow_runs;

CREATE INDEX IF NOT EXISTS idx_workflow_runs_worktree   ON workflow_runs(worktree_id);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_parent     ON workflow_runs(parent_run_id);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_ticket     ON workflow_runs(ticket_id);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_repo       ON workflow_runs(repo_id);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_parent_wf  ON workflow_runs(parent_workflow_run_id);

COMMIT;
