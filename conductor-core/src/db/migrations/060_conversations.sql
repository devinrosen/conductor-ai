CREATE TABLE conversations (
  id             TEXT PRIMARY KEY,
  scope          TEXT NOT NULL CHECK (scope IN ('repo', 'worktree')),
  scope_id       TEXT NOT NULL,
  title          TEXT,
  created_at     TEXT NOT NULL,
  last_active_at TEXT NOT NULL
);

CREATE INDEX idx_conversations_scope ON conversations(scope, scope_id);
