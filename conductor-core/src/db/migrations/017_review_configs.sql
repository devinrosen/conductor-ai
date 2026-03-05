-- Per-repo review configuration for multi-agent PR review swarms.
CREATE TABLE IF NOT EXISTS review_configs (
    id          TEXT PRIMARY KEY,
    repo_id     TEXT NOT NULL REFERENCES repos(id),
    -- JSON array of reviewer role objects: [{name, focus, system_prompt, required}]
    roles_json  TEXT NOT NULL DEFAULT '[]',
    -- Whether to auto-post aggregated review as a GitHub PR comment.
    post_to_pr  INTEGER NOT NULL DEFAULT 1,
    -- Whether to auto-enqueue to merge queue on full approval.
    auto_merge  INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE(repo_id)
);

CREATE INDEX IF NOT EXISTS idx_review_configs_repo_id ON review_configs(repo_id);
