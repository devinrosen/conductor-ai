ALTER TABLE agent_runs ADD COLUMN conversation_id TEXT REFERENCES conversations(id);

CREATE INDEX idx_agent_runs_conversation_id ON agent_runs(conversation_id);
