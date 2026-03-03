CREATE TABLE agent_created_issues (
    id           TEXT PRIMARY KEY,
    agent_run_id TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    repo_id      TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    source_type  TEXT NOT NULL DEFAULT 'github',
    source_id    TEXT NOT NULL,
    title        TEXT NOT NULL,
    url          TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL
);
