ALTER TABLE workflow_runs ADD COLUMN total_input_tokens INTEGER;
ALTER TABLE workflow_runs ADD COLUMN total_output_tokens INTEGER;
ALTER TABLE workflow_runs ADD COLUMN total_cache_read_input_tokens INTEGER;
ALTER TABLE workflow_runs ADD COLUMN total_cache_creation_input_tokens INTEGER;
ALTER TABLE workflow_runs ADD COLUMN total_turns INTEGER;
ALTER TABLE workflow_runs ADD COLUMN total_cost_usd REAL;
ALTER TABLE workflow_runs ADD COLUMN total_duration_ms INTEGER;
ALTER TABLE workflow_runs ADD COLUMN model TEXT;
