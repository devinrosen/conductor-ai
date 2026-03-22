---
wave: 2
title: "Agent Layer: Agent Coordination + Agent Communication"
status: pending
pattern_count: 22
os_count: 0
depends_on: [1]
migrations: ["v050_agent_communication", "v051_agent_identity"]
sub_wave_order: ["D", "A", "B", "C", "E"]
estimated_effort: "6-10 weeks (1 engineer)"
---

# Wave 2: Agent Layer

## Executive Summary

Wave 2 integrates 22 patterns (15 agent-coordination, 7 agent-communication) into conductor-ai's agent infrastructure. The single most important adaptation is converting file-based inter-agent communication (markdown handoffs, YAML checkpoints, append-only logs) to SQLite-backed storage. This wave adds 7 new database tables across 2 migrations, extends `AgentDef` with 8 new optional fields, introduces dependency-aware parallel orchestration, and builds structured communication infrastructure (decision logs, handoffs, blockers, council voting).

Two patterns (human-checkpoint-protocol, human-escalation-artifact) already have full existing support and require only characterization tests. Nine patterns have no existing support and require new modules. Eleven patterns have partial support and require extensions to existing types and workflows.

**Backward compatibility guarantee**: Existing `.conductor/agents/*.md` files parse identically after all changes. All new `AgentDef` fields use `#[serde(default)]` and default to values that match current behavior.

---

## Patterns

### Agent Coordination (15)

| # | Pattern | Version | GRS | Strategy |
|---|---------|---------|-----|----------|
| 1 | persona-based-agent-specialization | 1.1.0 | medium | Anchored CoT |
| 2 | agent-template-standardization | 1.2.0 | medium | Anchored CoT |
| 3 | behavioral-trigger-dispatch | 1.2.0 | medium | Anchored CoT |
| 4 | model-tier-selection | 1.0.0 | medium | Anchored CoT |
| 5 | role-based-agent-hierarchy | 1.0.0 | medium | Anchored CoT |
| 6 | cross-agent-delegation-protocol | 1.0.0 | medium | Anchored CoT |
| 7 | dependency-aware-parallel-agent-spawning | 1.0.0 | medium | Anchored CoT |
| 8 | human-checkpoint-protocol | 1.0.0 | medium | Anchored CoT |
| 9 | generic-fsm-skeleton | 1.1.0 | medium | Anchored CoT |
| 10 | two-layer-agent-namespace-separation | 1.0.0 | medium | Anchored CoT |
| 11 | plan-then-swarm-execution | 1.0.0 | 50 | Anchored CoT |
| 12 | builder-validator-quality-gate | 1.0.0 | 55 | Anchored CoT |
| 13 | few-shot-example-dispatch-blocks | 1.0.0 | medium | Anchored CoT |
| 14 | fail-forward-with-blocker-aggregation | 1.0.0 | medium | Anchored CoT |
| 15 | council-decision-architecture | 1.0.0 | 60 | Anchored CoT |

### Agent Communication (7)

| # | Pattern | Version | GRS | Strategy |
|---|---------|---------|-----|----------|
| 16 | artifact-mediated-agent-communication | 1.0.0 | medium | Anchored CoT |
| 17 | human-escalation-artifact | 1.0.0 | medium | Anchored CoT |
| 18 | structured-handoff-protocol | 1.1.0 | medium | Anchored CoT |
| 19 | decision-log-as-shared-memory | 1.0.0 | medium | Anchored CoT |
| 20 | threaded-blocker-comments | 1.1.0 | medium | Anchored CoT |
| 21 | output-behavior-contract | 1.0.0 | medium | Anchored CoT |
| 22 | roundtable-structured-reconciliation | 1.0.0 | medium | Anchored CoT |

---

## Sub-wave Ordering

The recommended implementation order is **D -> A -> B -> C -> E**. This ordering is driven by dependency constraints:

```
Sub-wave D (Communication DB)     -- Creates all 7 new tables in v050
    |                                 Unblocks B (blockers, delegations) and C (council tables)
    v
Sub-wave A (Agent Identity)       -- Extends AgentDef, creates agent_templates in v051
    |                                 Unblocks B (tier, delegation_table fields) and C (persona for council)
    v
Sub-wave B (Orchestration)        -- DAG, parallel spawning, delegation, FSM, dispatch
    |                                 Unblocks C (parallel spawning needed for council)
    v
Sub-wave C (Quality)              -- Builder-validator, few-shot, fail-forward, council
    |
    v
Sub-wave E (Checkpoint)           -- Characterization tests only (full existing support)
```

**Rationale**:
1. **D first**: The `agent_blockers` and `council_sessions`/`council_votes` tables are prerequisites for Sub-waves B and C. Creating all communication tables in a single migration (v050) unblocks everything.
2. **A second**: Extends `AgentDef` with persona, tier, namespace, and delegation_table fields consumed by Sub-waves B and C.
3. **B third**: Depends on identity fields (tier for hierarchy enforcement, delegation_table for cross-agent delegation) and communication tables (delegations, blockers).
4. **C fourth**: Depends on orchestration infrastructure (parallel spawning for council) and communication infrastructure (blockers for fail-forward).
5. **E last**: No new code needed. Characterization tests verify existing `GateNode`/`FeedbackRequest` match pattern specs.

---

## SQLite Adaptation: File-Based to DB-Backed Communication

The source patterns assume file-based communication (markdown artifacts, YAML records). Conductor uses SQLite as its sole persistent state store. This section defines the complete schema for all new tables.

### Migration v050: Agent Communication Tables

**File**: `conductor-core/src/db/migrations/v050_agent_communication.sql`

```sql
-- Decision log: append-only shared memory across agents
CREATE TABLE IF NOT EXISTS agent_decisions (
    id TEXT PRIMARY KEY,                                        -- ULID
    workflow_run_id TEXT REFERENCES workflow_runs(id),           -- Scope (nullable)
    feature_id TEXT REFERENCES features(id),                    -- Scope (nullable)
    sequence_number INTEGER NOT NULL,                           -- Monotonic within scope
    context TEXT NOT NULL,                                      -- What prompted the decision
    decision TEXT NOT NULL,                                     -- The decision itself
    rationale TEXT NOT NULL,                                    -- Why this decision
    agent_run_id TEXT NOT NULL REFERENCES agent_runs(id),       -- Who made it
    agent_name TEXT,                                            -- Display name
    supersedes_id TEXT REFERENCES agent_decisions(id),          -- Supersession chain
    metadata TEXT,                                              -- JSON for optional fields
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_agent_decisions_workflow ON agent_decisions(workflow_run_id, sequence_number);
CREATE INDEX idx_agent_decisions_feature ON agent_decisions(feature_id, sequence_number);

-- Structured handoffs between workflow phases
CREATE TABLE IF NOT EXISTS agent_handoffs (
    id TEXT PRIMARY KEY,                                        -- ULID
    workflow_run_id TEXT NOT NULL REFERENCES workflow_runs(id),
    from_step_id TEXT REFERENCES workflow_run_steps(id),        -- Source step
    to_step_id TEXT REFERENCES workflow_run_steps(id),          -- Target step
    payload TEXT NOT NULL,                                      -- JSON: {overview, decisions[], patterns[], limitations[], gaps[]}
    producer_agent TEXT NOT NULL,                               -- Agent name that produced the handoff
    consumer_agent TEXT,                                        -- Agent name that consumed it (nullable until consumed)
    validated INTEGER NOT NULL DEFAULT 0,                       -- Whether payload passed schema validation
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_agent_handoffs_workflow ON agent_handoffs(workflow_run_id);

-- Threaded blockers with parent-child threading
CREATE TABLE IF NOT EXISTS agent_blockers (
    id TEXT PRIMARY KEY,                                        -- ULID
    workflow_run_id TEXT REFERENCES workflow_runs(id),
    workflow_step_id TEXT REFERENCES workflow_run_steps(id),
    agent_run_id TEXT REFERENCES agent_runs(id),
    parent_blocker_id TEXT REFERENCES agent_blockers(id),       -- Threading support
    severity TEXT NOT NULL DEFAULT 'medium',                    -- critical | high | medium | low
    category TEXT,                                              -- build_failure | test_failure | dependency | design_question
    summary TEXT NOT NULL,
    detail TEXT,
    status TEXT NOT NULL DEFAULT 'open',                        -- open | resolved | escalated | deferred
    resolved_by TEXT,                                           -- agent_run_id or 'human'
    resolution_note TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    resolved_at TEXT
);
CREATE INDEX idx_agent_blockers_workflow ON agent_blockers(workflow_run_id, status);
CREATE INDEX idx_agent_blockers_parent ON agent_blockers(parent_blocker_id);

-- Agent delegations (cross-agent task routing)
CREATE TABLE IF NOT EXISTS agent_delegations (
    id TEXT PRIMARY KEY,                                        -- ULID
    delegator_run_id TEXT NOT NULL REFERENCES agent_runs(id),   -- Who delegated
    delegate_run_id TEXT REFERENCES agent_runs(id),             -- Who received (NULL until spawned)
    target_role TEXT NOT NULL,                                  -- Role being delegated to
    context_envelope TEXT NOT NULL,                             -- JSON: {subtask, evidence, constraints, expected_return_format}
    status TEXT NOT NULL DEFAULT 'pending',                     -- pending | active | completed | failed
    outcome TEXT,                                               -- JSON result from delegate
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    completed_at TEXT
);
CREATE INDEX idx_agent_delegations_delegator ON agent_delegations(delegator_run_id);

-- Council decision sessions (multi-agent voting)
CREATE TABLE IF NOT EXISTS council_sessions (
    id TEXT PRIMARY KEY,                                        -- ULID
    workflow_run_id TEXT REFERENCES workflow_runs(id),
    question TEXT NOT NULL,                                     -- The decision question
    quorum INTEGER NOT NULL DEFAULT 3,                          -- Minimum votes required
    decision_method TEXT NOT NULL DEFAULT 'majority',           -- majority | unanimous | weighted
    status TEXT NOT NULL DEFAULT 'voting',                      -- voting | reconciling | decided | deadlocked
    reconciled_decision TEXT,                                   -- Final reconciled output (nullable)
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    decided_at TEXT
);

CREATE TABLE IF NOT EXISTS council_votes (
    id TEXT PRIMARY KEY,                                        -- ULID
    session_id TEXT NOT NULL REFERENCES council_sessions(id),
    agent_run_id TEXT NOT NULL REFERENCES agent_runs(id),
    agent_role TEXT NOT NULL,                                   -- Voter's role for context
    vote TEXT NOT NULL,                                         -- The agent's position/recommendation
    confidence REAL,                                            -- 0.0-1.0
    rationale TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_council_votes_session ON council_votes(session_id);
```

### Migration v051: Agent Identity Tables

**File**: `conductor-core/src/db/migrations/v051_agent_identity.sql`

```sql
-- Agent template registry (extends file-based AgentDef)
CREATE TABLE IF NOT EXISTS agent_templates (
    id TEXT PRIMARY KEY,                                        -- ULID
    name TEXT NOT NULL UNIQUE,                                  -- Template name for reference
    persona_name TEXT,                                          -- Named expert persona (nullable)
    persona_depth TEXT NOT NULL DEFAULT 'minimal',              -- rich | minimal | none
    persona_credentials TEXT,
    domain_grounding TEXT,
    philosophy TEXT,
    role TEXT NOT NULL DEFAULT 'reviewer',                      -- actor | reviewer | orchestrator | validator
    tier INTEGER NOT NULL DEFAULT 0,                            -- 0=execution, 1=specialist, 2=planning, 3=supervisor
    namespace TEXT NOT NULL DEFAULT 'user',                     -- system | user
    model_tier TEXT,                                            -- high (opus) | standard (sonnet) | efficient (haiku)
    model_override TEXT,                                        -- Explicit model string override
    capabilities TEXT,                                          -- JSON array of capability tags
    delegation_table TEXT,                                      -- JSON: {role -> purpose}
    output_contract TEXT,                                       -- JSON: output behavior rules
    version TEXT NOT NULL DEFAULT '1.0.0',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Artifact registry for artifact-mediated communication
CREATE TABLE IF NOT EXISTS agent_artifacts (
    id TEXT PRIMARY KEY,                                        -- ULID
    agent_run_id TEXT NOT NULL REFERENCES agent_runs(id),
    artifact_type TEXT NOT NULL,                                -- code | document | config | test | plan
    path TEXT NOT NULL,                                         -- Filesystem path relative to worktree
    description TEXT,
    version INTEGER NOT NULL DEFAULT 1,                         -- Version counter for artifact
    previous_artifact_id TEXT REFERENCES agent_artifacts(id),   -- Version chain
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_agent_artifacts_run ON agent_artifacts(agent_run_id);
```

### Migration Design Principles

1. **Additive only**: All new tables. No existing table modifications. Existing code paths are completely unaffected.
2. **JSON for semi-structured data**: Payloads, context envelopes, delegation tables, and output contracts use JSON columns. Queryable via SQLite `json_extract()`.
3. **Scoped to existing entities**: All new tables reference existing foreign keys (`workflow_runs`, `agent_runs`, `workflow_run_steps`, `features`). No orphaned data.
4. **ULID primary keys**: Consistent with all other conductor tables.

---

## AgentDef Extension

### New Frontmatter Fields (8 fields, all optional)

The `AgentFrontmatter` struct in `conductor-core/src/agent_config.rs` gains these fields:

```yaml
---
role: actor                    # Existing: actor | reviewer
can_commit: true               # Existing
model: claude-opus-4-6       # Existing: explicit model override
# --- New fields (all optional, backward compatible via #[serde(default)]) ---
persona_name: "Michael Feathers"          # String: named expert persona
persona_depth: rich                        # Enum: rich | minimal | none (default: none)
persona_credentials: "Author of ..."       # String: credentials/bio
domain_grounding: "Your expertise in ..."  # String: domain context injected into prompt
philosophy: "You believe ..."              # String: operating philosophy
tier: 2                                    # u8: 0=execution, 1=specialist, 2=planning, 3=supervisor (default: 0)
namespace: user                            # Enum: system | user (default: user)
model_tier: high                           # Enum: high | standard | efficient (default: null)
template_id: standard-actor                # String: reference to agent_templates.name (default: null)
capabilities:                              # Vec<String>: capability tags (default: [])
  - code_review
  - architecture
delegation_table:                          # HashMap<String, String>: {role -> purpose} (default: null)
  security-reviewer: "Delegate security analysis"
  test-engineer: "Delegate test writing"
output_contract:                           # JSON or reference: behavioral output rules (default: null)
  narration_policy: prohibited
  confirmation_required: true
---
```

### Extended AgentDef Struct

```rust
pub struct AgentDef {
    // Existing fields (unchanged)
    pub name: String,
    pub role: AgentRole,
    pub can_commit: bool,
    pub model: Option<String>,
    pub prompt: String,
    // New fields (all with #[serde(default)])
    pub persona: Option<AgentPersona>,           // From persona_name/depth/credentials/grounding/philosophy
    pub tier: u8,                                 // Default: 0
    pub namespace: AgentNamespace,                // Default: AgentNamespace::User
    pub model_tier: Option<ModelTier>,            // Default: None
    pub template_id: Option<String>,              // Default: None
    pub capabilities: Vec<String>,                // Default: vec![]
    pub delegation_table: Option<HashMap<String, String>>,  // Default: None
    pub output_contract: Option<OutputContract>,  // Default: None
}
```

### New Types

```rust
/// Agent persona configuration
pub struct AgentPersona {
    pub name: String,
    pub depth: PersonaDepth,
    pub credentials: Option<String>,
    pub domain_grounding: Option<String>,
    pub philosophy: Option<String>,
}

pub enum PersonaDepth { Rich, Minimal, None }

pub enum AgentNamespace { System, User }

pub enum ModelTier { High, Standard, Efficient }

/// Extended agent role (was: Actor, Reviewer)
pub enum AgentRole { Actor, Reviewer, Orchestrator, Validator }

pub struct OutputContract {
    pub narration_policy: NarrationPolicy,   // prohibited | constrained | unrestricted
    pub confirmation_required: bool,
    pub formatting_rules: Option<String>,
    pub tier_overrides: Option<HashMap<String, serde_json::Value>>,
}

pub enum NarrationPolicy { Prohibited, Constrained, Unrestricted }
```

### Backward Compatibility

All new fields have default values matching current behavior:

| Field | Default | Effect |
|-------|---------|--------|
| `persona` | `None` | No persona injection into prompt |
| `tier` | `0` | Execution tier (lowest) |
| `namespace` | `User` | User namespace (existing behavior) |
| `model_tier` | `None` | Use explicit `model` field or system default |
| `template_id` | `None` | No template validation |
| `capabilities` | `[]` | No declared capabilities |
| `delegation_table` | `None` | No delegation authority |
| `output_contract` | `None` | No behavioral contract enforcement |

Existing `.conductor/agents/*.md` files parse identically because `serde` skips missing fields when they have `#[serde(default)]`. No migration of existing agent files is required.

### Migration Path

1. **Wave 2 delivery**: All new fields optional. Existing agents work unchanged.
2. **Post-Wave 2**: Optional CLI command `conductor agent upgrade <name>` adds recommended fields based on role/usage.
3. **Future**: Consider making `namespace` and `tier` recommended-with-warning, but never required.

---

## Sub-wave D: Communication Infrastructure (7 patterns)

**Patterns**: artifact-mediated-agent-communication@1.0.0, human-escalation-artifact@1.0.0, structured-handoff-protocol@1.1.0, decision-log-as-shared-memory@1.0.0, threaded-blocker-comments@1.1.0, output-behavior-contract@1.0.0, roundtable-structured-reconciliation@1.0.0

**Goal**: Build the communication infrastructure: handoff protocol, decision log, blocker threads, output contracts, and reconciliation engine. Run v050 migration.

### Files to Create

| File | Purpose | Key Types |
|------|---------|-----------|
| `conductor-core/src/agent/handoff.rs` | Structured handoff: create/validate handoff records, payload schema | `HandoffPayload`, `HandoffManager` |
| `conductor-core/src/agent/decision_log.rs` | Decision log: append-only records, sequence numbering, supersession | `DecisionEntry`, `DecisionLogManager` |
| `conductor-core/src/agent/blockers.rs` | Blocker management: threaded blockers, severity, resolution tracking | `Blocker`, `BlockerSeverity`, `BlockerCategory`, `BlockerManager` |
| `conductor-core/src/agent/artifacts.rs` | Artifact registry: register/query by type, versioning | `AgentArtifact`, `ArtifactType`, `ArtifactManager` |
| `conductor-core/src/agent/output_contract.rs` | Output contracts: contract definition, tier profiles, validation | `OutputContract`, `NarrationPolicy` |
| `conductor-core/src/agent/reconciliation.rs` | Roundtable reconciliation: merge divergent outputs, conflict detection | `ReconciliationResult`, `ReconciliationAgent` |

### Files to Modify

| File | Change |
|------|--------|
| `conductor-core/src/agent/context.rs` | Inject decision log entries and prior handoff context into `build_startup_context()` |
| `conductor-core/src/agent/mod.rs` | Add module declarations: `pub(crate) mod handoff; pub(crate) mod decision_log; pub(crate) mod blockers; pub(crate) mod artifacts; pub(crate) mod output_contract; pub(crate) mod reconciliation;` |
| `conductor-core/src/workflow/engine.rs` | Insert handoff creation between sequential workflow steps |
| `conductor-core/src/workflow/executors.rs` | Record artifacts produced by agent calls; validate output contracts |
| `conductor-core/src/db/migrations.rs` | Add v050 migration registration |

### Data Flow: Handoffs

```
Step A (agent completes)
  |
  +-- Workflow executor parses structured output
  +-- Creates agent_handoffs record:
  |     {from_step_id, payload: {overview, decisions[], patterns[], limitations[]}}
  +-- Validates payload against handoff schema
  |
  v
Step B (agent starts)
  |
  +-- build_startup_context() queries agent_handoffs for from_step_id
  +-- Injects handoff content into agent prompt as structured section
  +-- Agent has full predecessor context without re-discovery
```

### Data Flow: Decision Log

```
Agent makes architectural decision during execution
  |
  +-- Agent outputs structured decision block:
  |     ## Decision: DEC-{next_seq}
  |     Context: ...  /  Decision: ...  /  Rationale: ...
  |
  +-- Workflow executor parses decision block
  +-- Inserts into agent_decisions (sequence_number auto-incremented within scope)
  |
  v
Subsequent agent starts
  |
  +-- build_startup_context() queries recent decisions for this workflow/feature
  +-- Injects as "Prior Decisions" section
  +-- Agent can reference DEC-NNN in its own output
```

### Data Flow: Blockers

```
Step fails or encounters obstacle
  |
  +-- Workflow executor creates agent_blockers record:
  |     {severity: high, category: test_failure, summary: "3 tests fail in auth module"}
  |
  +-- If fail_forward enabled:
  |     Record blocker, skip dependent steps, continue to independent steps
  |
  +-- If step receives resolution attempt from another agent:
  |     Create child blocker (parent_blocker_id = original)
  |
  v
Workflow completes (or reaches checkpoint)
  |
  +-- Aggregate all open blockers:
  |     SELECT * FROM agent_blockers WHERE workflow_run_id = ? AND status = 'open'
  |     ORDER BY severity DESC
  +-- Present in TUI or escalate to human
```

### Test Cases

| Test Type | Scope | Description |
|-----------|-------|-------------|
| Unit | `handoff.rs` | Payload validation: required sections present, malformed JSON rejected |
| Unit | `decision_log.rs` | Sequence numbering: monotonic within scope, supersession chain |
| Unit | `blockers.rs` | Threaded blocker creation, severity aggregation, resolution tracking |
| Unit | `artifacts.rs` | Artifact registration, version chain, type filtering |
| Unit | `output_contract.rs` | Contract parsing, tier override resolution |
| Unit | `reconciliation.rs` | Divergent input merging, agreement/disagreement detection |
| Integration | `engine.rs` | Handoff between steps: step A produces handoff, step B receives it in context |
| Integration | `context.rs` | Decision injection: decisions from prior runs appear in startup context |
| Migration | `db/migrations.rs` | v050 migration: all 7 new tables created correctly with indexes |

---

## Sub-wave A: Agent Identity (5 patterns)

**Patterns**: persona-based-agent-specialization@1.1.0, agent-template-standardization@1.2.0, role-based-agent-hierarchy@1.0.0, model-tier-selection@1.0.0, two-layer-agent-namespace-separation@1.0.0

**Goal**: Extend `AgentDef` and the agent configuration system to support rich identity, hierarchical roles, model tier selection, and namespace separation. Run v051 migration.

### Files to Create

| File | Purpose | Key Types |
|------|---------|-----------|
| `conductor-core/src/agent/persona.rs` | Persona types, persona validation, prompt injection | `AgentPersona`, `PersonaDepth` |
| `conductor-core/src/agent/namespace.rs` | Namespace types, resolution with namespace precedence | `AgentNamespace`, `resolve_agent_by_namespace()` |
| `conductor-core/src/agent/template.rs` | Template registry: CRUD for `agent_templates` table, template inheritance | `AgentTemplate`, `TemplateManager` |

### Files to Modify

| File | Change |
|------|--------|
| `conductor-core/src/agent_config.rs` | Extend `AgentFrontmatter` with optional persona, tier, namespace, model_tier, capabilities, delegation_table, template_id, output_contract fields. Extend `AgentDef` with corresponding fields. Extend `AgentRole` enum to include Orchestrator and Validator variants. |
| `conductor-core/src/agent/mod.rs` | Add `pub(crate) mod persona; pub(crate) mod namespace; pub(crate) mod template;` |
| `conductor-core/src/agent/context.rs` | Inject persona and namespace context into `build_startup_context()` |
| `conductor-core/src/db/migrations.rs` | Add v051 migration registration |

### Per-Pattern Details

**persona-based-agent-specialization@1.1.0** (Gap: Partial)
- Add optional `persona` section to agent `.md` frontmatter
- Fields: `persona_name`, `persona_depth` (rich/minimal/none), `persona_credentials`, `domain_grounding`, `philosophy`
- Defaults to `persona_depth: none`, preserving backward compatibility
- Persona content injected into agent prompt by `build_startup_context()` as structured section before agent's own template

**agent-template-standardization@1.2.0** (Gap: Partial)
- `agent_templates` table provides DB-backed registry complementing file-based system
- Templates define standard sections: persona, core principles, methodology, agent invocation, output format
- Template inheritance: `base_template` reference for shared sections
- `AgentDef` gains optional `template_id` frontmatter field linking to registered template
- Existing agents without `template_id` are unaffected

**role-based-agent-hierarchy@1.0.0** (Gap: Partial)
- Extend `AgentRole` enum: `{Actor, Reviewer}` becomes `{Actor, Reviewer, Orchestrator, Validator}`
- Add `tier: u8` field (0=execution, 1=specialist, 2=planning, 3=supervisor)
- Hierarchy enforcement: agent at tier N can only delegate to tier N-1 or below (enforced in `delegation.rs`)
- Existing agents default to `tier: 0`; `Orchestrator` role gets `can_delegate: true` by default

**model-tier-selection@1.0.0** (Gap: Low/Partial)
- Add `model_tier` field: `high` (opus), `standard` (sonnet), `efficient` (haiku)
- Resolved to concrete model string via `~/.conductor/config.toml` under `[model_tiers]`
- When both `model_tier` and `model` specified, `model` wins (explicit override)
- Existing `AgentDef.model` and `AgentRun.model` columns remain unchanged

**two-layer-agent-namespace-separation@1.0.0** (Gap: Full)
- Add `namespace` field: `system` (conductor-internal) and `user` (project-specific)
- Resolution order in `load_agent_by_name()`: system namespace searched first (`.conductor/agents/system/`), then user (`.conductor/agents/`)
- Prevents user agents from accidentally shadowing system infrastructure agents
- `agent_templates` table includes `namespace` column for same purpose
- All existing agents default to `namespace: user`

### Ordering Constraints

None within this sub-wave. All five patterns modify the same set of types. Implement as a single cohesive change.

### Test Cases

| Test Type | Scope | Description |
|-----------|-------|-------------|
| Unit | `persona.rs` | Parse persona frontmatter, validate depth levels, test defaults |
| Unit | `namespace.rs` | Namespace resolution precedence, system-before-user ordering |
| Unit | `template.rs` | Template CRUD, inheritance resolution, validation |
| Unit | `agent_config.rs` | Backward compat: existing agent files parse without new fields |
| Integration | `agent_config.rs` | Full roundtrip: write agent `.md` with new fields, load, verify all fields |
| Migration | `db/migrations.rs` | v051 migration: fresh DB and upgrade from v050 both produce correct schema |

---

## Sub-wave B: Orchestration (5 patterns)

**Patterns**: dependency-aware-parallel-agent-spawning@1.0.0, plan-then-swarm-execution@1.0.0, cross-agent-delegation-protocol@1.0.0, generic-fsm-skeleton@1.1.0, behavioral-trigger-dispatch@1.2.0

**Goal**: Enhance the orchestrator from sequential-only to dependency-aware parallel execution, add dynamic delegation, FSM lifecycle management, and event-driven dispatch.

### Files to Create

| File | Purpose | Key Types |
|------|---------|-----------|
| `conductor-core/src/agent/dispatch.rs` | Behavioral trigger dispatch: dispatch table, classification-driven agent selection | `DispatchTable`, `DispatchEntry`, `validate_dispatch_table()` |
| `conductor-core/src/agent/delegation.rs` | Cross-agent delegation: context envelope, outcome reporting, CRUD for `agent_delegations` | `DelegationRequest`, `DelegationManager` |
| `conductor-core/src/agent/dag.rs` | Dependency DAG: topological sort, level computation, cycle detection, parallel batch construction | `DependencyGraph`, `build_dependency_graph()`, `compute_levels()`, `detect_cycles()` |
| `conductor-core/src/agent/fsm.rs` | Generic FSM: `StateMachine` trait, transition validation, state history, resumability | `StateMachine` trait, `TransitionError` |

### Files to Modify

| File | Change |
|------|--------|
| `conductor-core/src/orchestrator.rs` | Add `orchestrate_parallel()` function alongside existing `orchestrate_run()`. Uses `dag.rs` for level computation, `std::thread::spawn` for concurrency, barrier sync between levels. |
| `conductor-core/src/agent/mod.rs` | Add module declarations: dispatch, delegation, dag, fsm |
| `conductor-core/src/workflow/executors.rs` | Extend `execute_parallel()` to support dependency declarations between parallel calls |
| `conductor-core/src/workflow_dsl/types.rs` | Add `depends_on: Vec<String>` field to `CallNode` for intra-parallel dependencies |
| `conductor-core/src/workflow_dsl/parser.rs` | Parse `depends_on` attribute in call nodes |

### orchestrate_parallel() Design

```rust
pub fn orchestrate_parallel(
    conn: &Connection,
    config: &Config,
    parent_run_id: &str,
    worktree_path: &str,
    model: Option<&str>,
    orch_config: &OrchestratorConfig,
) -> Result<OrchestrationResult>
```

**Algorithm**:
1. Fetch plan steps with dependency declarations
2. Build DAG via `dag::build_dependency_graph(steps)`
3. Validate: `dag::detect_cycles(graph)` -- error if cycles found
4. Compute levels: `dag::compute_levels(graph)` -- topological sort into dependency strata
5. For each level (sequentially):
   a. Spawn all steps in the level concurrently via `std::thread::spawn`
   b. Each thread: opens own `Connection`, creates child run, spawns tmux window, polls for completion
   c. Barrier: `join` all threads, collect results
   d. If `fail_fast` and any thread failed, stop
   e. If not `fail_fast`, record blockers for failed steps, continue
6. Aggregate results

**Concurrency model**: `std::thread::spawn` (matching TUI threading pattern, no async runtime). Each thread opens its own `Connection` (SQLite WAL mode handles concurrent reads/writes). Thread count capped by `orch_config.max_parallel_agents` (default: 5).

### Plan-Then-Swarm Architecture

```
User triggers workflow
         |
         v
[Planning Phase]
  - Single planning agent runs
  - Produces task list with dependencies
  - Output parsed into PlanSteps with depends_on fields
         |
         v
[Approval Gate] (optional, reuses GateNode HumanApproval)
  - Human reviews the plan
  - Approves, rejects, or modifies
         |
         v
[Swarm Phase]
  - orchestrate_parallel() processes the plan
  - Dependency-aware level-by-level execution
  - Each level: spawn all tasks, barrier sync
         |
         v
[Aggregation Phase]
  - Collect results from all child runs
  - Aggregate blockers
  - Produce summary
```

### Generic FSM Trait

```rust
pub trait StateMachine {
    type State: Clone + PartialEq;
    fn current_state(&self) -> &Self::State;
    fn valid_transitions(&self, from: &Self::State) -> Vec<Self::State>;
    fn transition(&mut self, to: Self::State) -> Result<()>;
    fn is_terminal(&self) -> bool;
    fn resume_state(&self) -> Option<Self::State>;  // Resumability hook
}
```

Existing `AgentRunStatus`, `StepStatus`, `WorkflowRunStatus`, and `WorkflowStepStatus` enums implement this trait. Composes with the workflow engine's execution loop by providing transition validation and terminal detection as reusable infrastructure.

**Composite pattern caveat**: FSM depends on P-LG-04b (state specification template) not in Wave 2. Skeleton functions without it but loses standardized state interface. Feasibility score reduced by -15.

### Per-Pattern Details

**behavioral-trigger-dispatch@1.2.0** (Gap: Partial)
- `DispatchTable { entries: Vec<DispatchEntry> }` where each entry has `trigger`, `agent`, `classification`
- Location: `.conductor/dispatch.toml` or inline in workflow definitions
- No-fallthrough enforced at validation time: `validate_dispatch_table()` checks all trigger types have entries
- Resolution: `first_match` (table ordered by specificity)
- Extends workflow DSL `trigger: manual|pr|scheduled` with fine-grained intra-workflow dispatch

**cross-agent-delegation-protocol@1.0.0** (Gap: Full)
- `delegation_table` field in `AgentDef` frontmatter: which roles each agent can delegate to
- `DelegationRequest` struct: context envelope (subtask, evidence, constraints, expected return format)
- Runtime: orchestrator creates `agent_delegations` record, spawns delegate, polls for completion
- Roleplay prohibition: agents with delegation tables get injected instruction to invoke specialists rather than simulate
- Three archetypes: orchestrator-to-specialist, agent-to-utility, sequential pipeline

**dependency-aware-parallel-agent-spawning@1.0.0** (Gap: Partial)
- `depends_on` attribute on `CallNode` within `parallel` blocks
- `dag.rs`: parse dependencies into adjacency list, topological sort, compute levels, validate no cycles
- See orchestrate_parallel() design above

**plan-then-swarm-execution@1.0.0** (Gap: Partial)
- Upgrades existing plan-then-execute to swarm-style parallel
- `swarm_mode: bool` on `OrchestratorConfig`
- After planning agent completes, orchestrator parses plan into dependency graph, executes via `orchestrate_parallel()`
- **Composite pattern caveat**: Multi-level pattern composing spawning + delegation + FSM. Apply -20 feasibility penalty.

### Ordering Constraints

1. `dag.rs` must exist before `orchestrator.rs` parallel enhancement
2. `delegation.rs` must exist before council (Sub-wave C)
3. `fsm.rs` can proceed independently
4. `dispatch.rs` can proceed independently

### Test Cases

| Test Type | Scope | Description |
|-----------|-------|-------------|
| Unit | `dag.rs` | Topological sort: linear chain, diamond, forest, cycle detection |
| Unit | `dag.rs` | Level computation: all-independent (single level), deep chain (one per level), mixed |
| Unit | `dispatch.rs` | Dispatch table validation: coverage check, no-fallthrough, resolution strategies |
| Unit | `delegation.rs` | Context envelope construction, tier enforcement, delegation table validation |
| Unit | `fsm.rs` | State machine trait: valid transitions, invalid rejection, terminal detection, resume |
| Integration | `orchestrator.rs` | `orchestrate_parallel()` with mock agents: 2-level DAG, barrier sync, fail_fast |
| Integration | `orchestrator.rs` | Plan-then-swarm: planning output parsed into DAG, swarm execution, result aggregation |

---

## Sub-wave C: Quality (4 patterns)

**Patterns**: builder-validator-quality-gate@1.0.0, few-shot-example-dispatch-blocks@1.0.0, fail-forward-with-blocker-aggregation@1.0.0, council-decision-architecture@1.0.0

**Goal**: Add structured quality gates, contextual examples in prompts, resilient execution with blocker tracking, and multi-agent consensus.

### Files to Create

| File | Purpose | Key Types |
|------|---------|-----------|
| `conductor-core/src/agent/quality_gate.rs` | Builder-validator cycle: validation pipeline, pass/fail with feedback | `QualityGateResult`, `build_then_validate()` |
| `conductor-core/src/agent/examples.rs` | Few-shot example dispatch: example registry, task-type matching, prompt injection | `load_examples()`, `inject_examples()` |
| `conductor-core/src/agent/council.rs` | Council: session management, vote collection, quorum, tie-breaking | `CouncilSession`, `CouncilVote`, `CouncilManager` |

### Files to Modify

| File | Change |
|------|--------|
| `conductor-core/src/agent/context.rs` | Inject few-shot examples into `build_startup_context()` based on task type |
| `conductor-core/src/workflow/engine.rs` | Add fail-forward mode: on step failure, record blocker and continue to next independent step |
| `conductor-core/src/workflow/executors.rs` | Integrate blocker aggregation into step execution; add `execute_council()` for council decision nodes |
| `conductor-core/src/workflow_dsl/types.rs` | Add `CouncilNode` variant to `WorkflowNode` enum |
| `conductor-core/src/workflow_dsl/parser.rs` | Parse `council` block syntax |
| `conductor-core/src/agent/mod.rs` | Add module declarations |

### Council Decision Flow

```
Council question posed (workflow or human)
         |
         v
[Spawn Council Agents]
  - Create council_sessions record
  - Spawn N agents in parallel (via dag.rs), each receives question + context + persona
         |
         v
[Collect Votes]
  - Each agent produces vote (position + confidence + rationale)
  - Stored in council_votes table
         |
         v
[Evaluate Decision]
  - Apply decision method (majority | unanimous | weighted)
  - If quorum met and clear winner: DECIDED
  - If deadlocked: reconciliation agent or escalate to human via FeedbackRequest
         |
         v
[Reconciliation] (optional)
  - Reconciliation agent receives all votes
  - Produces: agreements, disagreements, synthesis, dissents
  - Stored in council_sessions.reconciled_decision
         |
         v
[Decision Record]
  - Final decision stored in agent_decisions table as DEC-NNN
```

### Council DSL Syntax

```
council "architecture-review" {
    question = "Should we use microservices or monolith?"
    agents = [architect, security-reviewer, devops-engineer]
    quorum = 3
    method = "majority"  // majority | unanimous | weighted
}
```

**Composite pattern caveat**: Council + roundtable reconciliation is a multi-level composite pattern. Apply -25 feasibility penalty. Both carry high integration complexity. Implement council as simplest viable version first (majority vote, no reconciliation), then add reconciliation as a separate deliverable.

### Per-Pattern Details

**builder-validator-quality-gate@1.0.0** (Gap: Low/Partial)
- Existing `AgentRole::Actor` vs `Reviewer` and `QualityGateConfig` provide structural foundation
- Adds `QualityGateResult` struct: pass/fail, structured feedback, confidence score
- `build_then_validate()` executor: Actor call then Reviewer call, auto-retry on failure (up to `max_retries`)
- Integrates with output schema system (`schema_config.rs`) for structural validation

**few-shot-example-dispatch-blocks@1.0.0** (Gap: Full)
- Convention: `.conductor/agents/<name>/examples/` directory
- Each example: `.md` file with frontmatter declaring `task_type`
- `load_examples(agent_name, task_type)`: scan, filter, sort by relevance
- `inject_examples(prompt, examples)`: insert at `{{examples}}` marker
- Additive: agents without examples directories function identically to today

**fail-forward-with-blocker-aggregation@1.0.0** (Gap: Partial)
- Existing `fail_fast: false` and `always { }` block provide foundation
- On step failure: create `agent_blockers` record, skip dependent steps, continue to independent
- At workflow completion: aggregate all open blockers into summary
- TUI gains "Blockers" tab in workflow detail view
- Extends existing `workflow_runs.blocked_on` JSON with richer `agent_blockers` table

**council-decision-architecture@1.0.0** (Gap: Full)
- See Council Decision Flow and DSL syntax above

### Ordering Constraints

1. Council depends on Sub-wave B delegation infrastructure
2. Fail-forward depends on Sub-wave D blocker table
3. Builder-validator and few-shot are independent

### Test Cases

| Test Type | Scope | Description |
|-----------|-------|-------------|
| Unit | `quality_gate.rs` | Build-validate cycle: pass first try, fail-then-pass, fail all retries |
| Unit | `examples.rs` | Example loading: correct task_type matching, empty dir, missing dir |
| Unit | `council.rs` | Vote collection, majority/unanimous/weighted, quorum check, deadlock detection |
| Integration | `engine.rs` | Fail-forward: 3-step workflow, step 2 fails, step 3 (independent) still runs |
| Integration | `engine.rs` | Fail-forward with deps: step 2 fails, step 3 (depends on 2) skipped, step 4 runs |

---

## Sub-wave E: Checkpoint (1 pattern)

**Pattern**: human-checkpoint-protocol@1.0.0

**Goal**: This pattern already has **full existing support** via `GateNode` with `HumanApproval`/`HumanReview` types, `FeedbackRequest`, and the TUI approval flow. Integration work is limited to characterization tests and documentation.

**Files to create/modify**: None.

**Action**: Write characterization tests confirming existing behavior matches the pattern specification. Document the mapping in the integration report.

### Test Cases

| Test Type | Scope | Description |
|-----------|-------|-------------|
| Characterization | existing | Verify `GateNode` HumanApproval matches pattern spec |
| Characterization | existing | Verify `FeedbackRequest` lifecycle matches pattern spec |

---

## Tasks

### W2-T01: Agent Identity Schema Extension
- **Sub-wave**: A
- **Patterns**: persona-based-agent-specialization@1.1.0, agent-template-standardization@1.2.0, role-based-agent-hierarchy@1.0.0, model-tier-selection@1.0.0, two-layer-agent-namespace-separation@1.0.0
- **Files to create**: `persona.rs`, `namespace.rs`, `template.rs`
- **Files to modify**: `agent_config.rs`, `agent/mod.rs`, `agent/context.rs`, `db/migrations.rs`
- **Action**: Add 8 new optional fields to AgentDef; create new types; run v051 migration for `agent_templates` and `agent_artifacts` tables
- **Test**: Unit tests for new fields, backward compat, migration test

### W2-T02: Behavioral Trigger Dispatch
- **Sub-wave**: B
- **Pattern**: behavioral-trigger-dispatch@1.2.0
- **Files to create**: `agent/dispatch.rs`
- **Files to modify**: `agent_config.rs`
- **Action**: Event-driven agent dispatch: dispatch table type, classification-driven selection, no-fallthrough validation
- **Test**: Dispatch routing tests with mock events

### W2-T03: Dependency-Aware Parallel Spawning
- **Sub-wave**: B
- **Pattern**: dependency-aware-parallel-agent-spawning@1.0.0
- **Depends**: W2-T01
- **Files to create**: `agent/dag.rs`
- **Files to modify**: `orchestrator.rs`, `workflow/executors.rs`, `workflow_dsl/types.rs`, `workflow_dsl/parser.rs`
- **Action**: Build dependency graph; topological sort into levels; `orchestrate_parallel()` with barrier sync
- **Test**: DAG resolution, cycle detection, correct spawn ordering, barrier sync

### W2-T04: Plan-Then-Swarm Execution
- **Sub-wave**: B
- **Pattern**: plan-then-swarm-execution@1.0.0
- **Depends**: W2-T03
- **Files to modify**: `orchestrator.rs`
- **Action**: Two-phase execution: planning agent produces task graph, swarm executes via `orchestrate_parallel()`
- **Test**: End-to-end test with mock agents
- **Composite pattern caveat**: -20 feasibility penalty

### W2-T05: Cross-Agent Delegation
- **Sub-wave**: B
- **Pattern**: cross-agent-delegation-protocol@1.0.0
- **Depends**: W2-T01
- **Files to create**: `agent/delegation.rs`
- **Files to modify**: `agent_config.rs`, `orchestrator.rs`
- **Action**: Delegation table, context envelope, runtime delegation execution via `agent_delegations` table
- **Test**: Delegation routing, capability match, tier enforcement tests

### W2-T06: Generic FSM for Agent Lifecycle
- **Sub-wave**: B
- **Pattern**: generic-fsm-skeleton@1.1.0
- **Depends**: Wave 1 W1-T06 (FSM spec)
- **Files to create**: `agent/fsm.rs`
- **Files to modify**: `agent/status.rs`
- **Action**: `StateMachine` trait, transition validation, state history, resumability hook
- **Test**: State transition tests, invalid transition rejection
- **Composite pattern caveat**: -15 feasibility penalty (missing P-LG-04b)

### W2-T07: Builder-Validator Quality Gate
- **Sub-wave**: C
- **Pattern**: builder-validator-quality-gate@1.0.0
- **Depends**: W2-T06
- **Files to create**: `agent/quality_gate.rs`
- **Files to modify**: `workflow/executors.rs`
- **Action**: `build_then_validate()` executor, `QualityGateResult`, auto-retry on validation failure
- **Test**: Build-validate cycle: pass, fail-then-pass, fail-all-retries

### W2-T08: Few-Shot Example Dispatch
- **Sub-wave**: C
- **Pattern**: few-shot-example-dispatch-blocks@1.0.0
- **Files to create**: `agent/examples.rs`
- **Files to modify**: `agent/context.rs`, `agent_config.rs`
- **Action**: Example loading from `.conductor/agents/<name>/examples/`, task-type matching, prompt injection at `{{examples}}` marker
- **Test**: Prompt construction tests, correct example selection, missing directory graceful handling

### W2-T09: Fail-Forward with Blocker Aggregation
- **Sub-wave**: C
- **Pattern**: fail-forward-with-blocker-aggregation@1.0.0
- **Files to create**: `agent/blockers.rs` (shared with W2-T11)
- **Files to modify**: `workflow/engine.rs`, `workflow/executors.rs`
- **Action**: On failure, record `agent_blockers`, skip dependents, continue independents, aggregate at completion
- **Test**: Multi-step workflow with partial failures, dependency-aware skip logic

### W2-T10: Council Decision Architecture
- **Sub-wave**: C
- **Pattern**: council-decision-architecture@1.0.0
- **Depends**: W2-T01, W2-T05
- **Files to create**: `agent/council.rs`
- **Files to modify**: `workflow/executors.rs`, `workflow_dsl/types.rs`, `workflow_dsl/parser.rs`
- **Action**: `CouncilNode` DSL syntax, session management, parallel vote collection, quorum evaluation, deadlock escalation
- **Test**: Voting with majority/unanimous/weighted methods, tie-breaking, quorum scenarios
- **Composite pattern caveat**: -25 feasibility penalty

### W2-T11: Communication Infrastructure (DB Schema)
- **Sub-wave**: D
- **Patterns**: artifact-mediated-agent-communication@1.0.0, decision-log-as-shared-memory@1.0.0, threaded-blocker-comments@1.1.0
- **Files to modify**: `db/migrations.rs`
- **Action**: Create v050 migration with `agent_decisions`, `agent_handoffs`, `agent_blockers`, `agent_delegations`, `council_sessions`, `council_votes` tables
- **Test**: Migration test; CRUD tests for new tables

### W2-T12: Structured Handoff Protocol
- **Sub-wave**: D
- **Pattern**: structured-handoff-protocol@1.1.0
- **Depends**: W2-T11
- **Files to create**: `agent/handoff.rs`
- **Files to modify**: `workflow/engine.rs`, `agent/context.rs`
- **Action**: Structured handoff records with payload schema, validation, context injection into next agent
- **Test**: Handoff creation, retrieval, continuation, payload validation tests

### W2-T13: Human Escalation Artifacts
- **Sub-wave**: D
- **Pattern**: human-escalation-artifact@1.0.0
- **Depends**: W2-T11
- **Action**: Already implemented via `FeedbackRequest`. Write characterization tests only.
- **Test**: Escalation generation and TUI rendering characterization tests

### W2-T14: Output Behavior Contract
- **Sub-wave**: D
- **Pattern**: output-behavior-contract@1.0.0
- **Files to create**: `agent/output_contract.rs`
- **Files to modify**: `agent_config.rs`, `workflow/executors.rs`
- **Action**: `output_contract` field in AgentDef, contract parsing, tier profiles, soft validation via reviewer
- **Test**: Contract validation tests with conforming and non-conforming output

### W2-T15: Roundtable Structured Reconciliation
- **Sub-wave**: D
- **Pattern**: roundtable-structured-reconciliation@1.0.0
- **Depends**: W2-T10
- **Files to create**: `agent/reconciliation.rs`
- **Files to modify**: `agent/council.rs`, `workflow/executors.rs`
- **Action**: Merge divergent agent outputs, conflict detection, synthesis into `council_sessions.reconciled_decision`
- **Test**: Reconciliation tests with conflicting inputs
- **Composite pattern caveat**: -25 feasibility penalty (shares council composite penalty)

### W2-T16: Human Checkpoint Protocol
- **Sub-wave**: E
- **Pattern**: human-checkpoint-protocol@1.0.0
- **Depends**: W2-T11, W1-T06
- **Action**: Already implemented. Write characterization tests confirming `GateNode`/`FeedbackRequest` match pattern spec.
- **Test**: Checkpoint creation, TUI approval flow, workflow resume after approval

## Task Dependency Graph

```
W2-T11 (comms DB, D) ──> W2-T12 (handoffs, D)
                     ├──> W2-T13 (escalation, D)
                     └──> W2-T16 (checkpoint, E)

W2-T01 (identity, A) ──> W2-T03 (parallel spawn, B) ──> W2-T04 (plan-swarm, B)
                     ├──> W2-T05 (delegation, B) ──> W2-T10 (council, C) ──> W2-T15 (reconciliation, D)
                     └──> W2-T06 (FSM, B) ──> W2-T07 (builder-validator, C)

W2-T02 (triggers, B)          -- independent
W2-T08 (few-shot, C)          -- independent
W2-T09 (fail-forward, C)      -- depends on W2-T11 (blockers table)
W2-T14 (output contract, D)   -- independent
```

---

## TUI Integration Points

| Feature | TUI Component | Data Source |
|---------|--------------|-------------|
| Blocker panel | New tab in WorkflowRunDetail | `agent_blockers` table |
| Decision log viewer | New tab in WorkflowRunDetail | `agent_decisions` table |
| Council vote display | New section in workflow step detail | `council_votes` table |
| Handoff viewer | Expandable section between steps | `agent_handoffs` table |
| Human escalation (existing) | Modal with feedback prompt | `feedback_requests` table |
| Artifact registry | New tab in worktree detail | `agent_artifacts` table |

All TUI data access follows the existing pattern: query on the main thread if fast (indexed lookups), or spawn background thread for aggregation queries per the `CLAUDE.md` threading rule.

---

## Risk Assessment

### Risk 1: Concurrent SQLite Access in Parallel Spawning (HIGH)

`orchestrate_parallel()` spawns multiple threads, each creating and polling `AgentRun` records. SQLite WAL mode supports concurrent readers + single writer, but heavy concurrent writes can trigger `SQLITE_BUSY`. Existing `open_database()` sets 5-second busy timeout.

**Mitigation**: (1) Cap concurrency at 5 via `max_parallel_agents`. (2) Wrap DB writes in Wave 1 retry infrastructure (`bounded-retry-with-escalation`). (3) Each thread opens its own connection.

### Risk 2: AgentDef Extension Breaks Existing Parsers (MEDIUM)

Adding 8 new optional fields to `AgentFrontmatter` relies on `serde(default)`. Field name collision with existing YAML content is unlikely but possible.

**Mitigation**: (1) Field names chosen to avoid collision. (2) `parse_agent_file()` already handles missing frontmatter. (3) Add `#[serde(deny_unknown_fields)]` guard on old struct, then remove when adding new fields to catch breakage in tests.

### Risk 3: Council/Roundtable Composite Pattern Complexity (HIGH)

Multi-level composite pattern (parallel spawning + vote collection + decision method + deadlock + reconciliation). CMU/SEI research shows F1=0.56 for composite patterns.

**Mitigation**: (1) Implement council as simplest viable version first (majority vote, no reconciliation). (2) Add reconciliation as separate later deliverable. (3) Manual review before merge.

### Risk 4: Workflow DSL Extension (MEDIUM)

Adding `depends_on` attribute and `council` block to hand-written recursive descent parser (958 lines).

**Mitigation**: (1) Comprehensive parser tests before implementation. (2) Additive syntax only. (3) Consider implementing `depends_on` as `with` attribute reusing existing attribute parsing.

### Risk 5: TUI Thread Safety for New Tables (LOW)

New TUI panels follow established background-thread pattern.

**Mitigation**: Follow exact pattern in `CLAUDE.md` and reference implementations in `crud_operations.rs` and `workflow_management.rs`.

---

## Summary Statistics

| Metric | Value |
|--------|-------|
| Total patterns | 22 |
| Full existing support (tests only) | 2 (human-checkpoint-protocol, human-escalation-artifact) |
| Partial support (extend) | 11 |
| No existing support (new) | 9 |
| New files to create | ~14 |
| Files to modify | ~15 |
| New DB tables | 8 (agent_decisions, agent_handoffs, agent_blockers, agent_delegations, council_sessions, council_votes, agent_templates, agent_artifacts) |
| New DB migrations | 2 (v050_agent_communication, v051_agent_identity) |
| Sub-wave order | D -> A -> B -> C -> E |
| Estimated total effort | 6-10 weeks (1 engineer) |
| Highest risk | Concurrent SQLite access in parallel spawning |
| Composite pattern penalties | FSM (-15), plan-then-swarm (-20), council/roundtable (-25) |

---

## Cross-References

- **Task 001** (Architecture Map): Module map, dependency graph, and seam identification used throughout
- **Task 004** (Agent Infrastructure Report): Gap analysis driving integration complexity estimates
- **Task 006** (Wave 1 Plan): `SubprocessFailure` and `RetryConfig` from Wave 1 are prerequisites for parallel spawning retry logic
- **Task 007** (Wave 2 Integration Plan): Full per-pattern integration specs, data flow diagrams, test strategy
- **Pattern Library**: All 22 pattern specs consulted for adaptation notes
