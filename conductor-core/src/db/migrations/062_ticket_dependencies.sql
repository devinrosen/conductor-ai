CREATE TABLE ticket_dependencies (
    from_ticket_id TEXT NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    to_ticket_id   TEXT NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    dep_type       TEXT NOT NULL DEFAULT 'blocks' CHECK (dep_type IN ('blocks', 'parent_of')),
    PRIMARY KEY (from_ticket_id, to_ticket_id, dep_type)
);

CREATE INDEX idx_ticket_dependencies_to ON ticket_dependencies(to_ticket_id);
CREATE INDEX idx_ticket_dependencies_from ON ticket_dependencies(from_ticket_id);
