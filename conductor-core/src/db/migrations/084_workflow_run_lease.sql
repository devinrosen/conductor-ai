-- Migration 084: add lease-based ownership columns to workflow_runs.
--
-- owner_token TEXT (nullable) — identifies the current lease holder; NULL means unowned.
-- lease_until TEXT (nullable, ISO 8601) — expiry timestamp of the current lease; NULL means no expiry.
-- generation INTEGER NOT NULL DEFAULT 0 — monotonic counter used by generation-check predicates
--   to detect stale ownership claims.  DEFAULT 0 ensures all existing rows start at generation 0
--   without any Rust-side backfill.

ALTER TABLE workflow_runs ADD COLUMN owner_token TEXT;
ALTER TABLE workflow_runs ADD COLUMN lease_until TEXT;
ALTER TABLE workflow_runs ADD COLUMN generation  INTEGER NOT NULL DEFAULT 0;
