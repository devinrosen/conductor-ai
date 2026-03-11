ALTER TABLE workflow_runs ADD COLUMN parent_workflow_run_id TEXT REFERENCES workflow_runs(id);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_parent_wf ON workflow_runs(parent_workflow_run_id);
