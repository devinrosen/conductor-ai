-- Must be run with PRAGMA foreign_keys = OFF (handled in Rust)
BEGIN;

CREATE TABLE tickets_new (
    id          TEXT PRIMARY KEY,
    repo_id     TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    source_type TEXT NOT NULL,
    source_id   TEXT NOT NULL,
    title       TEXT NOT NULL,
    body        TEXT NOT NULL DEFAULT '',
    state       TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'in_progress', 'closed')),
    labels      TEXT NOT NULL DEFAULT '[]',
    assignee    TEXT,
    priority    TEXT,
    url         TEXT NOT NULL DEFAULT '',
    synced_at   TEXT NOT NULL,
    raw_json    TEXT NOT NULL DEFAULT '{}',
    UNIQUE(repo_id, source_type, source_id)
);
INSERT INTO tickets_new SELECT * FROM tickets;
DROP TABLE tickets;
ALTER TABLE tickets_new RENAME TO tickets;

CREATE TABLE repo_issue_sources_new (
    id          TEXT PRIMARY KEY,
    repo_id     TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    source_type TEXT NOT NULL,
    config_json TEXT NOT NULL
);
INSERT INTO repo_issue_sources_new SELECT * FROM repo_issue_sources;
DROP TABLE repo_issue_sources;
ALTER TABLE repo_issue_sources_new RENAME TO repo_issue_sources;

COMMIT;
