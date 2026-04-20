ALTER TABLE agent_runs ADD COLUMN runtime TEXT NOT NULL DEFAULT 'claude';
ALTER TABLE repos ADD COLUMN runtime_overrides TEXT;
