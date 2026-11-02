-- Migration 087: RFC 021 interactive session tracking (#3157).
-- Adds 'idle' to the agent_runs.status CHECK constraint and the nullable columns
-- later RFC 021 tickets need (idle_since, subprocess_started_at, tmux_pane,
-- transcript_path, transcript_offset). SQLite cannot ALTER a CHECK in place, so
-- this is a table swap. Must run with PRAGMA foreign_keys = OFF (handled in Rust via
-- with_foreign_keys_off). Triggers on agent_runs are captured and replayed in Rust,
-- because DROP TABLE silently drops them.
--
-- The INSERT uses an explicit column list: column order in the live table differs from
-- the CREATE below after the ALTER ADD/DROP/RENAME history.

BEGIN;

CREATE TABLE agent_runs_new (
    id                          TEXT PRIMARY KEY,
    worktree_id                 TEXT REFERENCES worktrees(id) ON DELETE CASCADE,
    session_id                  TEXT,
    prompt                      TEXT NOT NULL,
    status                      TEXT NOT NULL DEFAULT 'running'
                                CHECK (status IN ('running','completed','failed','cancelled','waiting_for_feedback','idle')),
    result_text                 TEXT,
    cost_usd                    REAL,
    num_turns                   INTEGER,
    duration_ms                 INTEGER,
    started_at                  TEXT NOT NULL,
    ended_at                    TEXT,
    log_file                    TEXT,
    model                       TEXT,
    plan                        TEXT,
    parent_run_id               TEXT REFERENCES agent_runs_new(id) ON DELETE SET NULL,
    input_tokens                INTEGER,
    output_tokens               INTEGER,
    cache_read_input_tokens     INTEGER,
    cache_creation_input_tokens INTEGER,
    bot_name                    TEXT,
    repo_id                     TEXT REFERENCES repos(id),
    conversation_id             TEXT REFERENCES conversations(id),
    subprocess_pid              INTEGER,
    runtime                     TEXT NOT NULL DEFAULT 'claude',
    idle_since                  TEXT,
    subprocess_started_at       TEXT,
    tmux_pane                   TEXT,
    transcript_path             TEXT,
    transcript_offset           INTEGER
);

INSERT INTO agent_runs_new (
    id, worktree_id, session_id, prompt, status, result_text, cost_usd, num_turns,
    duration_ms, started_at, ended_at, log_file, model, plan, parent_run_id,
    input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens,
    bot_name, repo_id, conversation_id, subprocess_pid, runtime
) SELECT
    id, worktree_id, session_id, prompt, status, result_text, cost_usd, num_turns,
    duration_ms, started_at, ended_at, log_file, model, plan, parent_run_id,
    input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens,
    bot_name, repo_id, conversation_id, subprocess_pid, runtime
    FROM agent_runs;

DROP TABLE agent_runs;
ALTER TABLE agent_runs_new RENAME TO agent_runs;

CREATE INDEX IF NOT EXISTS idx_agent_runs_parent ON agent_runs(parent_run_id);
CREATE INDEX IF NOT EXISTS idx_agent_runs_worktree ON agent_runs(worktree_id);
CREATE INDEX IF NOT EXISTS idx_agent_runs_repo_id ON agent_runs(repo_id);
CREATE INDEX IF NOT EXISTS idx_agent_runs_worktree_started ON agent_runs(worktree_id, started_at);
CREATE INDEX IF NOT EXISTS idx_agent_runs_conversation_id ON agent_runs(conversation_id);

COMMIT;
