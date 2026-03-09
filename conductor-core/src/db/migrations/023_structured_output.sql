-- Add structured_output column to workflow_run_steps for schema-validated JSON output.
ALTER TABLE workflow_run_steps ADD COLUMN structured_output TEXT;
