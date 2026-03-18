ALTER TABLE workflow_runs ADD COLUMN feature_id TEXT REFERENCES features(id);
