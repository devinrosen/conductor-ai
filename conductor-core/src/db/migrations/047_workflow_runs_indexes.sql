CREATE INDEX IF NOT EXISTS idx_workflow_runs_ticket ON workflow_runs(ticket_id);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_repo ON workflow_runs(repo_id);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_parent_wf ON workflow_runs(parent_workflow_run_id);
