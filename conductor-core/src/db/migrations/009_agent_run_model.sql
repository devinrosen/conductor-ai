-- Migration 008: add model column to agent_runs table.
-- Nullable TEXT: records the model used for each agent run.
ALTER TABLE agent_runs ADD COLUMN model TEXT;
