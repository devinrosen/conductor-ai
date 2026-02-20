CREATE TABLE repos (
    id          TEXT PRIMARY KEY,
    slug        TEXT NOT NULL UNIQUE,
    local_path  TEXT NOT NULL,
    remote_url  TEXT NOT NULL,
    default_branch TEXT NOT NULL DEFAULT 'main',
    workspace_dir  TEXT NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE TABLE repo_issue_sources (
    id          TEXT PRIMARY KEY,
    repo_id     TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    source_type TEXT NOT NULL CHECK (source_type IN ('github', 'jira')),
    config_json TEXT NOT NULL
);

CREATE TABLE worktrees (
    id          TEXT PRIMARY KEY,
    repo_id     TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    slug        TEXT NOT NULL,
    branch      TEXT NOT NULL,
    path        TEXT NOT NULL,
    ticket_id   TEXT REFERENCES tickets(id) ON DELETE SET NULL,
    status      TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'merged', 'abandoned')),
    created_at  TEXT NOT NULL,
    UNIQUE(repo_id, slug)
);

CREATE TABLE tickets (
    id          TEXT PRIMARY KEY,
    repo_id     TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    source_type TEXT NOT NULL CHECK (source_type IN ('github', 'jira')),
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

CREATE TABLE sessions (
    id          TEXT PRIMARY KEY,
    started_at  TEXT NOT NULL,
    ended_at    TEXT,
    notes       TEXT
);

CREATE TABLE session_worktrees (
    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    worktree_id TEXT NOT NULL REFERENCES worktrees(id) ON DELETE CASCADE,
    PRIMARY KEY (session_id, worktree_id)
);
