-- Wave 2 Sub-wave A: Agent identity tables
-- Part of: agent-template-standardization@1.2.0, artifact-mediated-agent-communication@1.0.0

-- Agent template registry (extends file-based AgentDef)
CREATE TABLE IF NOT EXISTS agent_templates (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    persona_name TEXT,
    persona_depth TEXT NOT NULL DEFAULT 'minimal',
    persona_credentials TEXT,
    domain_grounding TEXT,
    philosophy TEXT,
    role TEXT NOT NULL DEFAULT 'reviewer',
    tier INTEGER NOT NULL DEFAULT 0,
    namespace TEXT NOT NULL DEFAULT 'user',
    model_tier TEXT,
    model_override TEXT,
    capabilities TEXT,
    delegation_table TEXT,
    output_contract TEXT,
    version TEXT NOT NULL DEFAULT '1.0.0',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Artifact registry for artifact-mediated communication
CREATE TABLE IF NOT EXISTS agent_artifacts (
    id TEXT PRIMARY KEY,
    workflow_run_id TEXT REFERENCES workflow_runs(id),
    agent_run_id TEXT NOT NULL REFERENCES agent_runs(id),
    artifact_type TEXT NOT NULL,
    name TEXT NOT NULL,
    content TEXT NOT NULL,
    format TEXT NOT NULL DEFAULT 'text',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_agent_artifacts_workflow ON agent_artifacts(workflow_run_id);
CREATE INDEX IF NOT EXISTS idx_agent_artifacts_agent ON agent_artifacts(agent_run_id);
