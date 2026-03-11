ALTER TABLE workflow_runs ADD COLUMN ticket_id TEXT REFERENCES tickets(id);
ALTER TABLE workflow_runs ADD COLUMN repo_id   TEXT REFERENCES repos(id);
CREATE INDEX idx_workflow_runs_ticket ON workflow_runs(ticket_id);
CREATE INDEX idx_workflow_runs_repo   ON workflow_runs(repo_id);
