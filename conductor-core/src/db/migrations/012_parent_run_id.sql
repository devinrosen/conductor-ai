-- Add parent_run_id to agent_runs for parent/child run relationships.
-- A supervisor agent run can spawn child runs; this FK tracks that tree.
ALTER TABLE agent_runs ADD COLUMN parent_run_id TEXT REFERENCES agent_runs(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS idx_agent_runs_parent ON agent_runs(parent_run_id);
