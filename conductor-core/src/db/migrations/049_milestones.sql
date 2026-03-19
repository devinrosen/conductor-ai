-- Milestones: named goals with status, target date, linked to a repo.
CREATE TABLE IF NOT EXISTS milestones (
    id          TEXT PRIMARY KEY,
    repo_id     TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    status      TEXT NOT NULL DEFAULT 'planned'
                CHECK (status IN ('planned', 'in_progress', 'completed', 'blocked')),
    target_date TEXT,
    created_at  TEXT NOT NULL,
    completed_at TEXT,
    UNIQUE (repo_id, name)
);

CREATE INDEX IF NOT EXISTS idx_milestones_repo_id ON milestones(repo_id);
CREATE INDEX IF NOT EXISTS idx_milestones_status ON milestones(status);

-- Deliverables: work units within a milestone, linked to features and tickets.
CREATE TABLE IF NOT EXISTS deliverables (
    id            TEXT PRIMARY KEY,
    milestone_id  TEXT NOT NULL REFERENCES milestones(id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    description   TEXT NOT NULL DEFAULT '',
    status        TEXT NOT NULL DEFAULT 'planned'
                  CHECK (status IN ('planned', 'in_progress', 'completed', 'blocked')),
    feature_id    TEXT REFERENCES features(id) ON DELETE SET NULL,
    review_status TEXT NOT NULL DEFAULT 'pending'
                  CHECK (review_status IN ('pending', 'in_review', 'approved', 'rejected')),
    created_at    TEXT NOT NULL,
    completed_at  TEXT,
    UNIQUE (milestone_id, name)
);

CREATE INDEX IF NOT EXISTS idx_deliverables_milestone_id ON deliverables(milestone_id);
CREATE INDEX IF NOT EXISTS idx_deliverables_feature_id ON deliverables(feature_id);

-- Link deliverables to tickets.
CREATE TABLE IF NOT EXISTS deliverable_tickets (
    deliverable_id TEXT NOT NULL REFERENCES deliverables(id) ON DELETE CASCADE,
    ticket_id      TEXT NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    PRIMARY KEY (deliverable_id, ticket_id)
);
