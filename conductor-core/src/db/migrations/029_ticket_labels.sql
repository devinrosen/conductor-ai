CREATE TABLE ticket_labels (
    ticket_id TEXT NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    label     TEXT NOT NULL,
    color     TEXT,
    PRIMARY KEY (ticket_id, label)
);
