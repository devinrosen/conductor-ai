CREATE TABLE agent_run_events (
    id         TEXT PRIMARY KEY,
    run_id     TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL,
    summary    TEXT NOT NULL,
    started_at TEXT NOT NULL,
    ended_at   TEXT,
    metadata   TEXT
);

CREATE INDEX idx_agent_run_events_run_id ON agent_run_events(run_id);
