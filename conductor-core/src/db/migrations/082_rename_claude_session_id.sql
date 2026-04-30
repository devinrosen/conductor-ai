-- Migration 082: rename agent_runs.claude_session_id → session_id.
--
-- Closes the second half of issue #2709. The portable runkon-runtimes
-- `RunHandle` already uses the generic `session_id` field name (introduced in
-- migration 081's PR for #2711). This migration aligns the conductor side so
-- the persisted column matches the struct field and no rename happens at the
-- boundary projection.
--
-- SQLite 3.25+ supports direct column renames; conductor uses 3.45+ via
-- bundled rusqlite, so no table-rebuild dance is required.

ALTER TABLE agent_runs RENAME COLUMN claude_session_id TO session_id;
