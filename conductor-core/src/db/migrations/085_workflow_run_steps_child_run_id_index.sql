CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_child_run_id
    ON workflow_run_steps(child_run_id);
