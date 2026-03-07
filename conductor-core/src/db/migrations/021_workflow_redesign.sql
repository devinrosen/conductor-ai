-- Workflow redesign: add new columns for structured output, iterations,
-- parallel groups, retries, gates, and workflow snapshots.

-- Store serialized WorkflowDef JSON so in-flight runs are not affected by
-- edits to the .wf source file.
ALTER TABLE workflow_runs ADD COLUMN definition_snapshot TEXT;

-- Iteration counter for while-loop steps.
ALTER TABLE workflow_run_steps ADD COLUMN iteration         INTEGER NOT NULL DEFAULT 0;

-- Shared ID for steps within a parallel group.
ALTER TABLE workflow_run_steps ADD COLUMN parallel_group_id TEXT;

-- CONDUCTOR_OUTPUT structured output fields.
ALTER TABLE workflow_run_steps ADD COLUMN context_out       TEXT;
ALTER TABLE workflow_run_steps ADD COLUMN markers_out       TEXT;

-- Retry tracking.
ALTER TABLE workflow_run_steps ADD COLUMN retry_count       INTEGER NOT NULL DEFAULT 0;

-- Gate state columns.
ALTER TABLE workflow_run_steps ADD COLUMN gate_type         TEXT;
ALTER TABLE workflow_run_steps ADD COLUMN gate_prompt       TEXT;
ALTER TABLE workflow_run_steps ADD COLUMN gate_timeout      TEXT;
ALTER TABLE workflow_run_steps ADD COLUMN gate_approved_by  TEXT;
ALTER TABLE workflow_run_steps ADD COLUMN gate_approved_at  TEXT;
ALTER TABLE workflow_run_steps ADD COLUMN gate_feedback     TEXT;
