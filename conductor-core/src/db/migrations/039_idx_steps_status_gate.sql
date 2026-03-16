-- Index for list_all_waiting_gate_steps: avoids a full table scan on every
-- TUI/web poll tick when filtering by status + gate_type.
CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_status_gate
  ON workflow_run_steps(status, gate_type);
