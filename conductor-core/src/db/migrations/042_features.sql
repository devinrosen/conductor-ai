CREATE TABLE IF NOT EXISTS features (
    id           TEXT PRIMARY KEY,
    repo_id      TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    branch       TEXT NOT NULL,
    base_branch  TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'active'
                 CHECK (status IN ('active', 'merged', 'closed')),
    created_at   TEXT NOT NULL,
    merged_at    TEXT,
    UNIQUE(repo_id, name)
);

CREATE TABLE IF NOT EXISTS feature_tickets (
    feature_id   TEXT NOT NULL REFERENCES features(id) ON DELETE CASCADE,
    ticket_id    TEXT NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    PRIMARY KEY (feature_id, ticket_id)
);

CREATE INDEX IF NOT EXISTS idx_features_repo ON features(repo_id);
CREATE INDEX IF NOT EXISTS idx_feature_tickets_feature ON feature_tickets(feature_id);
CREATE INDEX IF NOT EXISTS idx_feature_tickets_ticket ON feature_tickets(ticket_id);
