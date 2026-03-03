-- Migration 007: add plan column to agent_runs
-- Stores the two-phase plan as a JSON array of {description, done} objects.
ALTER TABLE agent_runs ADD COLUMN plan TEXT;
