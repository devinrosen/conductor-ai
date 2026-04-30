-- Migration 081: hoist token / cost / duration columns onto workflow_run_steps.
--
-- Path X.1 of the persistence-layer cleanup (Phase 4 prep). These columns
-- previously lived only on agent_runs and were JOINed at read time via
-- workflow_run_steps.child_run_id. Moving them onto workflow_run_steps lets
-- runkon-flow's persistence layer read step rows without reaching into the
-- conductor-specific agent_runs table.
--
-- Backfill of existing data is performed in Rust (see migrations.rs) so that
-- partial-schema test setups without agent_runs.input_tokens still pass the
-- ALTER TABLE step. agent_runs keeps its copy of the same columns for now;
-- deduplication is tracked as Path X.2.

ALTER TABLE workflow_run_steps ADD COLUMN input_tokens INTEGER;
ALTER TABLE workflow_run_steps ADD COLUMN output_tokens INTEGER;
ALTER TABLE workflow_run_steps ADD COLUMN cache_read_input_tokens INTEGER;
ALTER TABLE workflow_run_steps ADD COLUMN cache_creation_input_tokens INTEGER;
ALTER TABLE workflow_run_steps ADD COLUMN cost_usd REAL;
ALTER TABLE workflow_run_steps ADD COLUMN num_turns INTEGER;
ALTER TABLE workflow_run_steps ADD COLUMN duration_ms INTEGER;
