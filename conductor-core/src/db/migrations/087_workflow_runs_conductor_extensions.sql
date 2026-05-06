-- Migration 087: drop FK constraints on worktree_id/ticket_id/repo_id/parent_run_id
-- from workflow_runs, add last_position_advanced_at column, reinstall indexes, and
-- install AFTER DELETE cascade triggers on worktrees and agent_runs.
--
-- SQLite cannot drop FK constraints in-place; table-recreation pattern required.
-- Must be run with PRAGMA foreign_keys = OFF (handled in Rust via with_foreign_keys_off).
--
-- Column lineage:
--   020/021 base: id, workflow_name, worktree_id, parent_run_id, status, dry_run,
--                 trigger, started_at, ended_at, result_summary, definition_snapshot
--   026: inputs
--   027: nullable worktree_id + ticket_id, repo_id, parent_workflow_run_id,
--        target_label, default_bot_name
--   040: iteration
--   041: blocked_on
--   059: total_input_tokens, total_output_tokens, total_cache_read_input_tokens,
--        total_cache_creation_input_tokens, total_turns, total_cost_usd,
--        total_duration_ms, model
--   063: error
--   066: last_heartbeat
--   075: dismissed
--   079: status widened (cancelling) — table swap; timed_out dropped in 080
--   083: workflow_title
--   084: owner_token, lease_until, generation
--   087 (this migration): drop FKs on worktree_id/ticket_id/repo_id/parent_run_id;
--                          add last_position_advanced_at; install cascade triggers

BEGIN;

CREATE TABLE workflow_runs_new (
    id                                TEXT PRIMARY KEY,
    workflow_name                     TEXT NOT NULL,
    worktree_id                       TEXT,                       -- FK dropped (was → worktrees ON DELETE CASCADE)
    parent_run_id                     TEXT NOT NULL,              -- FK dropped (was → agent_runs ON DELETE CASCADE); NOT NULL preserved
    status                            TEXT NOT NULL DEFAULT 'pending'
                                      CHECK (status IN ('pending','running','waiting','completed','failed','cancelled','needs_resume','cancelling')),
    dry_run                           INTEGER NOT NULL DEFAULT 0,
    trigger                           TEXT NOT NULL DEFAULT 'manual'
                                      CHECK (trigger IN ('manual','pr','scheduled','hook')),
    started_at                        TEXT NOT NULL,
    ended_at                          TEXT,
    result_summary                    TEXT,
    definition_snapshot               TEXT,
    inputs                            TEXT,
    ticket_id                         TEXT,                        -- FK dropped (was → tickets)
    repo_id                           TEXT,                        -- FK dropped (was → repos)
    parent_workflow_run_id            TEXT REFERENCES workflow_runs(id),       -- fixed: was broken REFERENCES workflow_runs_new(id) from mig 079
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
    dismissed                         INTEGER NOT NULL DEFAULT 0,
    workflow_title                    TEXT,
    owner_token                       TEXT,
    lease_until                       TEXT,
    generation                        INTEGER NOT NULL DEFAULT 0,
    last_position_advanced_at         TEXT                          -- new in 087
);

INSERT INTO workflow_runs_new (
    id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger,
    started_at, ended_at, result_summary, definition_snapshot, inputs,
    ticket_id, repo_id, parent_workflow_run_id, target_label, default_bot_name,
    iteration, blocked_on,
    total_input_tokens, total_output_tokens, total_cache_read_input_tokens,
    total_cache_creation_input_tokens, total_turns, total_cost_usd, total_duration_ms,
    model, error, last_heartbeat, dismissed,
    workflow_title, owner_token, lease_until, generation
) SELECT
    id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger,
    started_at, ended_at, result_summary, definition_snapshot, inputs,
    ticket_id, repo_id, parent_workflow_run_id, target_label, default_bot_name,
    iteration, blocked_on,
    total_input_tokens, total_output_tokens, total_cache_read_input_tokens,
    total_cache_creation_input_tokens, total_turns, total_cost_usd, total_duration_ms,
    model, error, last_heartbeat, dismissed,
    workflow_title, owner_token, lease_until, generation
  FROM workflow_runs;

DROP TABLE workflow_runs;
ALTER TABLE workflow_runs_new RENAME TO workflow_runs;

CREATE INDEX IF NOT EXISTS idx_workflow_runs_worktree   ON workflow_runs(worktree_id);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_parent     ON workflow_runs(parent_run_id);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_ticket     ON workflow_runs(ticket_id);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_repo       ON workflow_runs(repo_id);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_parent_wf  ON workflow_runs(parent_workflow_run_id);

CREATE TRIGGER IF NOT EXISTS trg_worktree_delete_cascade_workflow_runs
AFTER DELETE ON worktrees
BEGIN
    DELETE FROM workflow_runs WHERE worktree_id = OLD.id;
END;

CREATE TRIGGER IF NOT EXISTS trg_agent_run_delete_cascade_workflow_runs
AFTER DELETE ON agent_runs
BEGIN
    DELETE FROM workflow_runs WHERE parent_run_id = OLD.id;
END;

COMMIT;
