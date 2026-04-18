-- Drop feature_id FK from workflow_runs first (FK constraint)
ALTER TABLE workflow_runs DROP COLUMN feature_id;
-- Drop join table before parent table (FK constraint)
DROP TABLE IF EXISTS feature_tickets;
DROP TABLE IF EXISTS features;
