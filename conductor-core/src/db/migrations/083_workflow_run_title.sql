-- Migration 083: add workflow_title column to workflow_runs for O(1) title access.
--
-- Previously every call to row_to_workflow_run() parsed the full
-- definition_snapshot JSON string to extract the title.  Storing it as a
-- dedicated column eliminates that per-row deserialization on list queries.
--
-- Backfill of existing rows is performed in Rust (see migrations.rs) so that
-- partial-schema test setups without definition_snapshot still pass this
-- ALTER TABLE step.

ALTER TABLE workflow_runs ADD COLUMN workflow_title TEXT;
