ALTER TABLE agent_runs ADD COLUMN input_tokens INTEGER;
ALTER TABLE agent_runs ADD COLUMN output_tokens INTEGER;
ALTER TABLE agent_runs ADD COLUMN cache_read_input_tokens INTEGER;
ALTER TABLE agent_runs ADD COLUMN cache_creation_input_tokens INTEGER;
