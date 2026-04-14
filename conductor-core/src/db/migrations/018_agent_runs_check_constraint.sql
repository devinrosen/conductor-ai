CREATE TABLE agent_runs_new (
    id                TEXT PRIMARY KEY,
    worktree_id       TEXT NOT NULL REFERENCES worktrees(id) ON DELETE CASCADE,
    claude_session_id TEXT,
    prompt            TEXT NOT NULL,
    status            TEXT NOT NULL DEFAULT 'running'
                      CHECK (status IN ('running','completed','failed','cancelled','waiting_for_feedback')),
    result_text       TEXT,
    cost_usd          REAL,
    num_turns         INTEGER,
    duration_ms       INTEGER,
    started_at        TEXT NOT NULL,
    ended_at          TEXT,
    tmux_window       TEXT,
    log_file          TEXT,
    model             TEXT,
    plan              TEXT,
    parent_run_id     TEXT REFERENCES agent_runs_new(id) ON DELETE SET NULL
);
INSERT INTO agent_runs_new SELECT id, worktree_id, claude_session_id, prompt, status,
    result_text, cost_usd, num_turns, duration_ms, started_at, ended_at,
    tmux_window, log_file, model, plan, parent_run_id FROM agent_runs;
DROP TABLE agent_runs;
ALTER TABLE agent_runs_new RENAME TO agent_runs;
CREATE INDEX IF NOT EXISTS idx_agent_runs_parent ON agent_runs(parent_run_id);
CREATE INDEX IF NOT EXISTS idx_agent_runs_worktree ON agent_runs(worktree_id);
