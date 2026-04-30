ALTER TABLE workflow_runs ADD COLUMN workflow_title TEXT;
UPDATE workflow_runs
   SET workflow_title = json_extract(definition_snapshot, '$.title')
 WHERE definition_snapshot IS NOT NULL;
