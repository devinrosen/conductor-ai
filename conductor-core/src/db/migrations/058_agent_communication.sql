-- Wave 2 Sub-wave D: Agent communication tables
-- Part of: structured-handoff-protocol@1.1.0, decision-log-as-shared-memory@1.0.0,
-- threaded-blocker-comments@1.1.0, cross-agent-delegation-protocol@1.0.0,
-- council-decision-architecture@1.0.0

-- Decision log: append-only shared memory across agents
CREATE TABLE IF NOT EXISTS agent_decisions (
    id TEXT PRIMARY KEY,
    workflow_run_id TEXT REFERENCES workflow_runs(id),
    feature_id TEXT REFERENCES features(id),
    sequence_number INTEGER NOT NULL,
    context TEXT NOT NULL,
    decision TEXT NOT NULL,
    rationale TEXT NOT NULL,
    agent_run_id TEXT NOT NULL REFERENCES agent_runs(id),
    agent_name TEXT,
    supersedes_id TEXT REFERENCES agent_decisions(id),
    metadata TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_agent_decisions_workflow ON agent_decisions(workflow_run_id, sequence_number);
CREATE INDEX IF NOT EXISTS idx_agent_decisions_feature ON agent_decisions(feature_id, sequence_number);

-- Structured handoffs between workflow phases
CREATE TABLE IF NOT EXISTS agent_handoffs (
    id TEXT PRIMARY KEY,
    workflow_run_id TEXT NOT NULL REFERENCES workflow_runs(id),
    from_step_id TEXT REFERENCES workflow_run_steps(id),
    to_step_id TEXT REFERENCES workflow_run_steps(id),
    payload TEXT NOT NULL,
    producer_agent TEXT NOT NULL,
    consumer_agent TEXT,
    validated INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_agent_handoffs_workflow ON agent_handoffs(workflow_run_id);

-- Threaded blockers with parent-child threading
CREATE TABLE IF NOT EXISTS agent_blockers (
    id TEXT PRIMARY KEY,
    workflow_run_id TEXT REFERENCES workflow_runs(id),
    workflow_step_id TEXT REFERENCES workflow_run_steps(id),
    agent_run_id TEXT REFERENCES agent_runs(id),
    parent_blocker_id TEXT REFERENCES agent_blockers(id),
    severity TEXT NOT NULL DEFAULT 'medium',
    category TEXT,
    summary TEXT NOT NULL,
    detail TEXT,
    status TEXT NOT NULL DEFAULT 'open',
    resolved_by TEXT,
    resolution_note TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    resolved_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_agent_blockers_workflow ON agent_blockers(workflow_run_id, status);
CREATE INDEX IF NOT EXISTS idx_agent_blockers_parent ON agent_blockers(parent_blocker_id);

-- Agent delegations (cross-agent task routing)
CREATE TABLE IF NOT EXISTS agent_delegations (
    id TEXT PRIMARY KEY,
    delegator_run_id TEXT NOT NULL REFERENCES agent_runs(id),
    delegate_run_id TEXT REFERENCES agent_runs(id),
    target_role TEXT NOT NULL,
    context_envelope TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    outcome TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    completed_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_agent_delegations_delegator ON agent_delegations(delegator_run_id);

-- Council decision sessions (multi-agent voting)
CREATE TABLE IF NOT EXISTS council_sessions (
    id TEXT PRIMARY KEY,
    workflow_run_id TEXT REFERENCES workflow_runs(id),
    question TEXT NOT NULL,
    quorum INTEGER NOT NULL DEFAULT 3,
    decision_method TEXT NOT NULL DEFAULT 'majority',
    status TEXT NOT NULL DEFAULT 'voting',
    reconciled_decision TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    decided_at TEXT
);

CREATE TABLE IF NOT EXISTS council_votes (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES council_sessions(id),
    agent_run_id TEXT NOT NULL REFERENCES agent_runs(id),
    agent_role TEXT NOT NULL,
    vote TEXT NOT NULL,
    confidence REAL,
    rationale TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_council_votes_session ON council_votes(session_id);
