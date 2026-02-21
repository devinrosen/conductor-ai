CREATE TABLE agent_runs (
    id                TEXT PRIMARY KEY,
    worktree_id       TEXT NOT NULL REFERENCES worktrees(id) ON DELETE CASCADE,
    claude_session_id TEXT,
    prompt            TEXT NOT NULL,
    status            TEXT NOT NULL DEFAULT 'running'
                      CHECK (status IN ('running','completed','failed','cancelled')),
    result_text       TEXT,
    cost_usd          REAL,
    num_turns         INTEGER,
    duration_ms       INTEGER,
    started_at        TEXT NOT NULL,
    ended_at          TEXT
);
