-- Migration 018: human-in-the-loop feedback requests.
-- Note: agent_runs table recreation is handled in Rust code (migrations.rs)
-- because SQLite requires PRAGMA foreign_keys = OFF which can't run in a transaction.

-- Create feedback_requests table.
CREATE TABLE IF NOT EXISTS feedback_requests (
    id          TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    prompt      TEXT NOT NULL,
    response    TEXT,
    status      TEXT NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'responded', 'dismissed')),
    created_at  TEXT NOT NULL,
    responded_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_feedback_requests_run_id ON feedback_requests(run_id);
