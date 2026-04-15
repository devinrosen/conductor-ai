BEGIN;

-- 1. Add new columns while table still exists (safe ADD COLUMN)
ALTER TABLE features ADD COLUMN source_type TEXT;
ALTER TABLE features ADD COLUMN source_id   TEXT;
ALTER TABLE features ADD COLUMN tickets_total   INTEGER NOT NULL DEFAULT 0;
ALTER TABLE features ADD COLUMN tickets_merged  INTEGER NOT NULL DEFAULT 0;

-- 2. Rebuild table to widen CHECK constraint and data-migrate active → in_progress.
-- The UPDATE approach cannot be used here because the old table still has
-- CHECK (status IN ('active', 'merged', 'closed')) which rejects 'in_progress'.
-- Instead, the CASE expression in the INSERT…SELECT performs the rename atomically
-- during the table rebuild, after which the new CHECK constraint takes over.
CREATE TABLE features_new (
    id           TEXT PRIMARY KEY,
    repo_id      TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    branch       TEXT NOT NULL,
    base_branch  TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'in_progress'
                 CHECK (status IN ('in_progress', 'ready_for_review', 'approved', 'merged', 'closed')),
    created_at   TEXT NOT NULL,
    merged_at    TEXT,
    last_commit_at TEXT,
    source_type  TEXT,
    source_id    TEXT,
    tickets_total   INTEGER NOT NULL DEFAULT 0,
    tickets_merged  INTEGER NOT NULL DEFAULT 0,
    UNIQUE(repo_id, name)
);

INSERT INTO features_new
    SELECT id, repo_id, name, branch, base_branch,
           CASE WHEN status = 'active' THEN 'in_progress' ELSE status END,
           created_at, merged_at,
           last_commit_at, source_type, source_id, tickets_total, tickets_merged
    FROM features;

DROP TABLE features;
ALTER TABLE features_new RENAME TO features;

CREATE INDEX IF NOT EXISTS idx_features_repo ON features(repo_id);

COMMIT;
