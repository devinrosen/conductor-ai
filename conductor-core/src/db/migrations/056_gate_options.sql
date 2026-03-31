ALTER TABLE workflow_run_steps ADD COLUMN gate_options    TEXT; -- JSON [{value,label}]
ALTER TABLE workflow_run_steps ADD COLUMN gate_selections TEXT; -- JSON ["val1","val2"]
