-- Migration 045: drop model and default_branch columns from repos.
-- SQLite requires a table rebuild for column drops.
-- NOTE: This migration must be run with foreign_keys OFF (handled in Rust).

BEGIN;

CREATE TABLE repos_new (
    id                          TEXT PRIMARY KEY,
    slug                        TEXT NOT NULL UNIQUE,
    local_path                  TEXT NOT NULL,
    remote_url                  TEXT NOT NULL,
    workspace_dir               TEXT NOT NULL,
    created_at                  TEXT NOT NULL,
    allow_agent_issue_creation  INTEGER NOT NULL DEFAULT 0
);

INSERT INTO repos_new (id, slug, local_path, remote_url, workspace_dir, created_at, allow_agent_issue_creation)
    SELECT id, slug, local_path, remote_url, workspace_dir, created_at,
           COALESCE(allow_agent_issue_creation, 0)
    FROM repos;

DROP TABLE repos;
ALTER TABLE repos_new RENAME TO repos;

COMMIT;
