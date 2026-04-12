-- Migration 064: subprocess_pid for headless agent tracking (RFC 016)
ALTER TABLE agent_runs ADD COLUMN subprocess_pid INTEGER;
