-- Drop join table before parent table (FK constraint)
-- Note: ALTER TABLE workflow_runs DROP COLUMN feature_id is handled in Rust
-- because SQLite has no DROP COLUMN IF EXISTS; presence is checked before calling.
DROP TABLE IF EXISTS feature_tickets;
DROP TABLE IF EXISTS features;
