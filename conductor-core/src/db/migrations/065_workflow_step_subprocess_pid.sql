-- Migration 065: subprocess_pid for script step orphan detection (RFC 016)
ALTER TABLE workflow_run_steps ADD COLUMN subprocess_pid INTEGER;
