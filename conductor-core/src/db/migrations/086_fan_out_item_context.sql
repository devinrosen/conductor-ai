-- Add context column to fan-out items so per-item metadata (ticket title, worktree
-- branch, etc.) can be injected into child workflow inputs as {{item.*}} variables.
ALTER TABLE workflow_run_step_fan_out_items ADD COLUMN context TEXT;
