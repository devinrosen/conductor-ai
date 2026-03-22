---
wave: 4
title: "Advanced Orchestration: Cross-Domain Composition + Multi-Agent Deliberation + Autonomous Execution"
status: pending
pattern_count: 12
os_count: 0
depends_on: [1, 2, 3]
migrations: [v053_decision_log, v054_context_tracking]
sub_waves: [4A, 4B, 4C]
estimated_effort: "8-12 weeks"
average_feasibility: 58.8
---

# Wave 4: Advanced Orchestration

## Executive Summary

Wave 4 is the composition wave. Every pattern builds on primitives from Waves 1-3 rather than introducing standalone capabilities. The 12 patterns decompose into four design concerns: (1) cross-domain template and context plumbing, (2) verification pipeline composition, (3) agent architecture triad and recovery, and (4) multi-agent deliberation with supervised autonomy.

**Composite pattern warning**: Three patterns (W4-T04 gated-verification-pipeline, W4-T05 agent-architecture-triad, W4-T07 autonomous-recovery-cycle) are composite multi-level patterns. Per CMU/SEI research on design pattern detection (Composite F1=0.56), automated coupling analysis has significantly lower reliability for these. All three receive a -20 point confidence penalty on feasibility scores and are flagged for manual verification. This is the primary risk in Wave 4.

**Pre-extraction recommendation**: 8 of 12 patterns modify `conductor-core/src/workflow/executors.rs` (3,204 lines). Before Wave 4 begins, extract `executors.rs` into sub-modules: `executors/call.rs`, `executors/gate.rs`, `executors/parallel.rs`, `executors/script.rs`. This is a prerequisite refactoring, not a Wave 4 task.

**Implementation structure**: Three sub-waves with explicit integration test gates between them. Sub-Wave 4A (foundation, 2-3 weeks), Sub-Wave 4B (composition, 3-4 weeks), Sub-Wave 4C (complex compositions, 3-4 weeks).

---

## Composition Strategy: How Wave 4 Consumes Waves 1-3

Wave 4 patterns are pure compositions. Every module and type they consume comes from prior waves.

### Wave 1 Dependencies (Error Handling + Lifecycle Gating)

| Wave 4 Pattern | Wave 1 Artifact Consumed | Purpose |
|----------------|--------------------------|---------|
| W4-T07 (recovery cycle) | `SubprocessFailure` struct (error.rs) | Classify errors for recovery tier selection |
| W4-T07 (recovery cycle) | `retry.rs` module (bounded-retry) | Inner retry loop within each escalation tier |
| W4-T07 (recovery cycle) | `is_transient()` classifier | Determine retry eligibility |
| W4-T07 (recovery cycle) | Emergency recovery protocol (P-EH-04) | 4-tier escalation ladder structure |
| W4-T04 (gated pipeline) | `ConductorError::exit_code()` | Semantic exit codes for pipeline stage failures |
| W4-T12 (supervised autonomy) | `RetryConfig` + `retry_with_backoff()` | Per-item autonomy bounds enforcement |
| W4-T11 (context guard) | Checkpoint persistence protocol | State checkpoint on guard trigger |

### Wave 2 Dependencies (Agent Coordination + Communication)

| Wave 4 Pattern | Wave 2 Artifact Consumed | Purpose |
|----------------|--------------------------|---------|
| W4-T05 (triad) | `AgentDef` extended with persona traits | Identity layer (Layer 1) |
| W4-T05 (triad) | Builder-validator quality gate | Validator role in the triad |
| W4-T05 (triad) | Cross-agent delegation protocol | Inter-layer communication |
| W4-T08 (facilitator) | Council decision architecture | Multi-agent deliberation substrate |
| W4-T08 (facilitator) | `ParallelNode` execution | Parallel delegate invocation |
| W4-T10 (parallel indep.) | `ParallelNode` with synchronization barrier | Concurrent agent spawning |
| W4-T09 (namespaced IDs) | Decision log shared memory (W2-T11) | Namespaced decision storage |
| W4-T02 (state comms) | Agent communication DB tables (W2-T11) | SQLite as state bus |
| W4-T12 (supervised autonomy) | Human checkpoint protocol (W2-T16) | Intrinsic checkpoint implementation |
| W4-T01 (templates) | Agent template standardization | Template inheritance system |

### Wave 3 Dependencies (Quality Infrastructure)

| Wave 4 Pattern | Wave 3 Artifact Consumed | Purpose |
|----------------|--------------------------|---------|
| W4-T04 (gated pipeline) | Evidence directory system | Evidence collection per pipeline stage |
| W4-T04 (gated pipeline) | Acceptance criteria DSL extension | Per-stage verification criteria |
| W4-T04 (gated pipeline) | `ConsistencyChecker` (desync detection) | DESYNC detection at stage boundaries |
| W4-T04 (gated pipeline) | `ErrorCategory` vocabulary | Structured error reporting in pipeline |
| W4-T07 (recovery cycle) | `ConsistencyChecker` | State drift detection as cycle trigger |
| W4-T07 (recovery cycle) | `DesyncReport` + `DesyncFlag` | Drift classification for tier selection |
| W4-T03 (context mgmt) | Error vocabulary propagation | Context-injected error classification |

### Composition Diagram

```
Wave 1 (Foundation)                Wave 2 (Agent Layer)           Wave 3 (Quality)
  SubprocessFailure ──────────────┐
  retry.rs ───────────────────────┼─── W4-T07 (Recovery Cycle)
  emergency-recovery (P-EH-04) ───┘        |
  ConductorError::exit_code() ────────── W4-T04 (Gated Pipeline) ──── evidence directory
  checkpoint-persistence ─────────────── W4-T11 (Context Guard)        acceptance criteria
  RetryConfig ────────────────────────── W4-T12 (Supervised Auto.)     desync detection
                                    |                                  error vocabulary
  AgentDef (extended) ──────────────┼─── W4-T05 (Triad)
  builder-validator ────────────────┘        |
  delegation protocol ──────────────────── W4-T08 (Facilitator) ──── W4-T10 (Parallel)
  council architecture ─────────────────── W4-T09 (Namespaced IDs)
  ParallelNode ─────────────────────────── W4-T10 (Parallel Independence)
  decision-log DB ──────────────────────── W4-T09 (Namespaced IDs)
  human-checkpoint ─────────────────────── W4-T12 (Supervised Auto.)
  agent-template-std ───────────────────── W4-T01 (Template Propagation)
  comms DB tables ──────────────────────── W4-T02 (State-Mediated Comms)
```

---

## DB Migrations

Wave 4 introduces two new migrations. These follow Wave 3's migrations (v049-v052 range).

### v053: Decision Log and Namespaces

**File**: `conductor-core/src/db/migrations/v053_decision_log.sql`

```sql
CREATE TABLE IF NOT EXISTS decision_log (
    id TEXT PRIMARY KEY,
    namespace TEXT NOT NULL,
    sequence_num INTEGER NOT NULL,
    workflow_run_id TEXT REFERENCES workflow_runs(id),
    step_name TEXT,
    agent_name TEXT,
    content TEXT NOT NULL,
    rationale TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(namespace, sequence_num)
);

CREATE TABLE IF NOT EXISTS decision_namespaces (
    prefix TEXT PRIMARY KEY,
    description TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
```

**Consumed by**: W4-T09 (namespace-separated-decision-ids)

### v054: Context Tracking

**File**: `conductor-core/src/db/migrations/v054_context_tracking.sql`

```sql
ALTER TABLE workflow_runs ADD COLUMN estimated_tokens_used INTEGER DEFAULT 0;
```

**Consumed by**: W4-T11 (context-window-exhaustion-guard)

---

## Sub-Wave Implementation Order

### Sub-Wave 4A: Foundation + Standalone Patterns (2-3 weeks)

Patterns with minimal inter-pattern dependencies within Wave 4. Can begin as soon as their Wave 1-3 dependencies are complete.

| Order | Task | Pattern | Feasibility | Effort | Wave Deps |
|-------|------|---------|-------------|--------|-----------|
| 1 | W4-T06 | selective-domain-applicability-filter | 72 (clean) | Low (3-5 days) | None |
| 2 | W4-T11 | context-window-exhaustion-guard | 58 (moderate) | Medium (1-2 weeks) | Wave 1 only |
| 3 | W4-T09 | namespace-separated-decision-ids | 70 (clean) | Low-Medium (3-5 days) | Wave 2: W2-T11 |
| 4 | W4-T08 | facilitator-delegate-separation | 75 (clean) | Low (3-5 days) | Wave 2: council |

**Gate**: All sub-wave 4A patterns have unit tests passing.

### Sub-Wave 4B: Composition Patterns (3-4 weeks)

Patterns that depend on 4A results and on each other.

| Order | Task | Pattern | Feasibility | Effort | Depends On |
|-------|------|---------|-------------|--------|------------|
| 5 | W4-T01 | template-with-adaptation-propagation | 55 (-15 penalty, moderate) | Medium (1 week) | Wave 2: template std |
| 6 | W4-T02 | state-mediated-agent-communication | 65 (moderate) | Medium (1 week) | Wave 2: W2-T11 |
| 7 | W4-T03 | cross-cutting-context-management | 60 (-15 penalty, moderate) | Medium (1 week) | Wave 3: error vocab |
| 8 | W4-T10 | parallel-first-round-independence | 60 (moderate) | Medium (1 week) | W4-T08 |
| 9 | W4-T12 | supervised-autonomy-model | 55 (moderate) | Medium (1-2 weeks) | W4-T11, Wave 2: W2-T16 |
| 10 | W4-T05 | agent-architecture-triad | 50 (-20 penalty, moderate) | Medium (1-2 weeks) | Wave 2: persona, builder-validator, delegation |

**Gate**: Integration tests pass for triad workflow and parallel deliberation.

### Sub-Wave 4C: Complex Compositions (3-4 weeks)

The highest-risk composite patterns. Implemented last with full Wave 1-3 and 4A-4B primitives available.

| Order | Task | Pattern | Feasibility | Effort | Depends On |
|-------|------|---------|-------------|--------|------------|
| 11 | W4-T07 | autonomous-recovery-cycle | 40 (-20 penalty, difficult) | High (2-3 weeks) | Wave 1: retry, emergency-recovery; Wave 3: ConsistencyChecker; W4-T05 |
| 12 | W4-T04 | gated-verification-pipeline | 45 (-20 penalty, difficult) | High (2-3 weeks) | Wave 3: evidence, acceptance, desync, error vocab; W4-T02, W4-T03 |

**Gate**: Full Wave 4 integration test suite passes. Recovery cycle tested with progressive failure scenarios. Verification pipeline tested end-to-end.

### Ordering Diagram

```
Sub-Wave 4A (foundation)          Sub-Wave 4B (composition)        Sub-Wave 4C (complex)
  W4-T06 (filter) ──────────────>
  W4-T11 (context guard) ──────> W4-T12 (supervised auto.) ─────>
  W4-T09 (namespaced IDs) ─────>
  W4-T08 (facilitator) ────────> W4-T10 (parallel indep.) ──────>
                                  W4-T01 (templates) ────────────>
                                  W4-T02 (state comms) ──────────> W4-T04 (gated pipeline)
                                  W4-T03 (context mgmt) ────────> W4-T04 (gated pipeline)
                                  W4-T05 (triad) ───────────────> W4-T07 (recovery cycle)
```

---

## Per-Task Specifications

### W4-T01: Template Adaptation Propagation

**Pattern**: template-with-adaptation-propagation@1.0.0
**Quality**: Moderate
**Feasibility Score**: 55 (-15 composite penalty)
**Sub-Wave**: 4B
**Effort**: Medium (1 week)
**Approach**: Extract-and-adapt
**Dependencies**: Wave 2 agent-template-standardization

**Files to create**:
- `conductor-core/src/workflow_template.rs` -- `WorkflowTemplate`, `VariantSlot`, `WorkflowTemplateEngine`

**Files to modify**:
- `conductor-core/src/workflow_config.rs` -- Recognize template parameters in workflow definitions
- `conductor-core/src/lib.rs` -- Add `pub mod workflow_template;`

**New types**:

```rust
pub struct WorkflowTemplate {
    pub name: String,
    pub invariant_skeleton: String,  // The .wf file with {{slot}} markers
    pub variant_slots: Vec<VariantSlot>,
}

pub struct VariantSlot {
    pub name: String,
    pub slot_type: SlotType,
    pub default_value: Option<String>,
    pub description: String,
}
```

**Test cases**:
- Unit: Template slot resolution preserves invariant skeleton, fills variant slots
- Unit: Missing required slot produces descriptive error
- Integration: Template instantiation produces valid workflow definition parseable by DSL parser

---

### W4-T02: State-Mediated Agent Communication

**Pattern**: state-mediated-agent-communication@1.0.0
**Quality**: Moderate
**Feasibility Score**: 65
**Sub-Wave**: 4B
**Effort**: Medium (1 week)
**Approach**: Extract-and-adapt
**Dependencies**: Wave 2 W2-T11 (comms DB)

**Files to create**: None (extends Wave 2's comms DB tables)

**Files to modify**:
- `conductor-core/src/agent/context.rs` -- Build agent startup context from shared state records
- `conductor-core/src/workflow/executors.rs` -- Write step outputs as communication artifacts to shared state

**Adaptation**: The source pattern uses filesystem artifacts. Conductor uses SQLite. The triple-role principle maps as:

| Triple Role | Source (Filesystem) | Conductor (SQLite) |
|-------------|--------------------|--------------------|
| State record | YAML frontmatter | `workflow_run_steps` status + metadata columns |
| Communication artifact | Markdown body | `result_text` + `context_out` + `structured_output` |
| Verification evidence | `.verify/` directory | Wave 3's evidence directory system |

**Test cases**:
- Unit: State record write produces correct SQLite columns
- Integration: State-mediated communication round-trip (agent A writes, agent B reads via context injection)
- Integration: Concurrent read/write under WAL mode does not corrupt state

---

### W4-T03: Cross-Cutting Context Management

**Pattern**: cross-cutting-context-management@1.0.0
**Quality**: Moderate
**Feasibility Score**: 60 (-15 composite penalty)
**Sub-Wave**: 4B
**Effort**: Medium (1 week)
**Approach**: Extract-and-adapt
**Dependencies**: Wave 3 error vocabulary

**Files to create**:
- `conductor-core/src/cross_cutting.rs` -- `CrossCuttingContext` struct

**Files to modify**:
- `conductor-core/src/agent/context.rs` -- Inject `CrossCuttingContext` into `build_startup_context()`
- `conductor-core/src/config.rs` -- Cross-cutting context configuration section

**New types**:

```rust
pub struct CrossCuttingContext {
    pub repo_config: RepoConfig,
    pub global_config: GlobalConfig,
    pub environment: EnvironmentInfo,
    pub error_vocabulary: &'static [ErrorCategory],
    pub context_guard: Option<ContextGuardConfig>,
}
```

**Test cases**:
- Unit: `CrossCuttingContext` populated correctly from config fixtures
- Integration: Context injected into agent startup context contains all expected fields
- Unit: Override mechanism allows per-workflow context customization

---

### W4-T04: Gated Verification Pipeline

**Pattern**: gated-verification-pipeline@1.0.0
**Quality**: Difficult
**Feasibility Score**: 45 (-20 composite penalty -- CMU/SEI Composite F1=0.56 applies; flagged for manual verification)
**Sub-Wave**: 4C (implement last)
**Effort**: High (2-3 weeks)
**Approach**: Refactor-then-extract
**Dependencies**: Wave 3 evidence directory, acceptance criteria, desync detection, error vocabulary; W4-T02, W4-T03

**Files to create**:
- `conductor-core/src/verification_pipeline.rs` -- Pipeline stage chain: gate -> verify -> record -> communicate

**Files to modify (not new -- these exist from earlier waves)**:
- `conductor-core/src/workflow/engine.rs` -- Insert pipeline as post-step hook when `acceptance_criteria` defined
- `conductor-core/src/workflow/executors.rs` -- Pipeline stage evaluation in gate execution path
- `conductor-core/src/consistency.rs` -- Wire `ConsistencyChecker` into pipeline stage boundaries

**Design**: At each workflow step boundary, the pipeline executes:
1. **Gate**: Evaluate threshold-based progression criteria
2. **Verify**: Collect evidence and evaluate against acceptance criteria
3. **Record**: Persist verification verdicts atomically to SQLite
4. **Communicate**: Generate verification report artifacts

**Risk factors**:
- Composite pattern composing 4 Wave 3 primitives; if any is incomplete, pipeline cannot be assembled
- `executors.rs` modification contention (mitigated by pre-extraction into sub-modules)
- 6+ coupling points with Wave 3 evidence system (highest coupling pair in Wave 4)

**Test cases**:
- Unit: Pipeline stage sequencing (gate -> verify -> record -> communicate) with mock evidence
- Integration: Pipeline rejects step when verification fails
- Integration: DESYNC detection at stage boundary triggers recovery
- Property: Pipeline stages are idempotent (re-running produces same result)

---

### W4-T05: Agent Architecture Triad

**Pattern**: agent-architecture-triad@1.0.0
**Quality**: Moderate
**Feasibility Score**: 50 (-20 composite penalty -- CMU/SEI Composite F1=0.56 applies; flagged for manual verification)
**Sub-Wave**: 4B
**Effort**: Medium (1-2 weeks)
**Approach**: Extract-and-adapt
**Dependencies**: Wave 2 persona-based-agent-specialization, builder-validator, cross-agent-delegation

**Concept**: The triad decomposes agent orchestration into three independently evolvable layers:
- **Layer 1 (Identity)**: Who acts. Resolved from `AgentDef` with persona, role, capability declarations.
- **Layer 2 (Capability)**: What they can do. Resolved from skill/procedure definitions.
- **Layer 3 (Deliberation)**: How they decide together. The coordination protocol.

**Implementation**: The triad is a **workflow-level orchestration pattern** (workflow template), not a new engine concept. It composes existing `call` + `gate` DSL nodes into a planner-executor-validator sequence.

**Files to create**: None (the triad is a workflow template: `.conductor/workflows/triad.wf`)

**Files to modify**:
- `conductor-core/src/workflow_dsl/types.rs` -- Add `TriadMetadata` struct as optional field on `WorkflowDef`
- `conductor-core/src/workflow/engine.rs` -- Triad lifecycle hooks: verify planner runs before executor, validator runs after executor
- `conductor-core/src/workflow_config.rs` -- Recognize `triad: true` frontmatter flag
- `conductor-core/src/workflow/executors.rs` -- Add `execute_triad_validation()` for ordering constraints

**New types** (`workflow_dsl/types.rs`):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriadMetadata {
    pub planner_step: String,
    pub executor_steps: Vec<String>,
    pub validator_step: String,
    pub quality_gate_step: Option<String>,
}
```

**Ordering enforcement**:
1. Validation (`workflow_dsl/validation.rs`): When `triad: true`, validate planner precedes executor precedes validator
2. Runtime guard (`engine.rs`): Before executing an executor step, assert planner completed successfully

**Test cases**:
- Unit: `TriadMetadata` validation rejects misordered steps
- Integration: Triad workflow executes planner -> executor -> validator in correct order
- Integration: Quality gate after validator triggers retry when confidence is low

---

### W4-T06: Selective Domain Applicability Filter

**Pattern**: selective-domain-applicability-filter@1.0.0
**Quality**: Clean
**Feasibility Score**: 72
**Sub-Wave**: 4A (implement first -- no Wave 2 dependency)
**Effort**: Low (3-5 days)
**Approach**: Direct extraction
**Dependencies**: None (standalone)

**Files to create**:
- `conductor-core/src/applicability.rs` -- `ApplicabilityFilter`, `FilterCriterion`, `ApplicabilityTier`

**Files to modify**:
- `conductor-core/src/workflow_config.rs` -- Wire filter into workflow discovery
- `conductor-core/src/lib.rs` -- Add `pub mod applicability;`

**New types**:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ApplicabilityTier {
    StrongCandidate,
    WeakCandidate,
    NotCandidate,
}

pub struct ApplicabilityFilter {
    pub criteria: Vec<FilterCriterion>,
}

pub struct FilterCriterion {
    pub name: String,
    pub predicate: FilterPredicate,
    pub tier_if_matched: ApplicabilityTier,
    pub rationale: String,
}

pub enum FilterPredicate {
    HasFile(String),
    HasDirectory(String),
    LanguageIs(String),
    FrameworkIs(String),
    FileCountAbove(usize),
}
```

**Test cases**:
- Unit: Filter predicates match/reject correctly with diverse repo configs
- Unit: Classification hierarchy resolves tier correctly with multiple criteria
- Integration: Filter wired into workflow discovery returns correct workflows for a given repo

---

### W4-T07: Autonomous Recovery Cycle

**Pattern**: autonomous-recovery-cycle@1.0.0
**Quality**: Difficult
**Feasibility Score**: 40 (-20 composite penalty -- CMU/SEI Composite F1=0.56 applies; flagged for manual verification)
**Sub-Wave**: 4C (implement last alongside W4-T04)
**Effort**: High (2-3 weeks)
**Approach**: Refactor-then-extract
**Dependencies**: Wave 1 SubprocessFailure, retry.rs, emergency-recovery; Wave 3 ConsistencyChecker, DesyncReport

**Composition**: The recovery cycle composes three Wave 1/Wave 3 primitives:
1. **Detection**: Wave 3's `ConsistencyChecker` triggers the cycle on state divergence
2. **Bounded retry**: Wave 1's `retry_with_backoff()` provides the inner retry loop
3. **Graduated escalation**: Wave 1's emergency recovery protocol provides the 4-tier ladder

**Files to create**:
- `conductor-core/src/recovery.rs` -- `RecoveryCycle`, `RecoveryConfig`, `EscalationTier`, `RecoveryOutcome`

**Files to modify (these are MODIFICATIONS of files from earlier waves, not new)**:
- `conductor-core/src/workflow/engine.rs` -- On step failure, invoke recovery cycle before marking Failed
- `conductor-core/src/workflow/types.rs` -- Add `BlockedOn::RecoveryEscalation` variant

**New types**:

```rust
pub struct RecoveryConfig {
    pub retry_limit_per_operation: u32,
    pub retry_limit_per_item: u32,
    pub escalation_depth: u32,
    pub detection: DetectionMechanism,
    pub human_escalation: HumanEscalationPolicy,
}

pub enum EscalationTier {
    TargetedFix,      // Retry the specific failed operation
    BroaderFix,       // Reset step state, retry from wider context
    BackupAndRebuild, // Checkpoint state, rebuild from known good
    Nuclear,          // Backup everything, clear state, rebuild from scratch
}

pub struct RecoveryCycle<'a> {
    config: RecoveryConfig,
    checker: ConsistencyChecker<'a>,
}
```

**Engine integration** (`engine.rs`):

```rust
if let Some(ref recovery_config) = workflow_def.recovery {
    let cycle = RecoveryCycle::new(recovery_config, &checker);
    match cycle.attempt_recovery(&step, &error) {
        RecoveryOutcome::Recovered { .. } => continue,        // Retry step
        RecoveryOutcome::HumanEscalation { .. } => {           // Transition to Waiting
            wf_mgr.set_waiting_blocked_on(run_id, &BlockedOn::RecoveryEscalation(diagnostic))?;
            return Ok(/* waiting */);
        }
        RecoveryOutcome::Exhausted { .. } => {                 // Normal failure path
            wf_mgr.update_step_status(step_id, Failed, ...)?;
        }
    }
}
```

**Risk factors**:
- Recovery actions at tier 3+ (backup+rebuild, nuclear) can cause data loss if implemented incorrectly
- Start with tier 1 (targeted fix) only; add higher tiers iteratively
- Never auto-execute tier 3+ without explicit opt-in configuration

**Test cases**:
- Unit: Tier escalation (targeted -> broader -> backup -> nuclear) with simulated failures
- Unit: Retry limit enforcement per operation and per item
- Integration: Recovery cycle recovers from transient failure at tier 1
- Integration: Recovery cycle escalates to human when all tiers exhausted
- Integration: Recovery cycle composes with desync detection (inject tmux window disappearance)

---

### W4-T08: Facilitator-Delegate Separation

**Pattern**: facilitator-delegate-separation@1.0.0
**Quality**: Clean
**Feasibility Score**: 75 (highest in Wave 4)
**Sub-Wave**: 4A
**Effort**: Low (3-5 days)
**Approach**: Direct extraction
**Dependencies**: Wave 2 council-decision-architecture

**Principle**: The facilitator (workflow engine) never simulates delegate responses. All agent contributions come from actual context-isolated invocations.

**Files to create**:
- `.conductor/agents/facilitator.md` -- Template with anti-roleplay prohibition in CRITICAL section

**Files to modify**:
- `conductor-core/src/workflow_dsl/types.rs` -- Add `role_constraint: Option<RoleConstraint>` to `CallNode`; new enum `RoleConstraint { Facilitator, Delegate, Unconstrained }`
- `conductor-core/src/workflow/executors.rs` -- When `role_constraint = Facilitator`, inject anti-roleplay instruction

**Anti-roleplay injection** (appended to facilitator agent prompts):

```
CRITICAL: You are the facilitator. You must NEVER generate content attributed to
delegate agents. All agent responses must come from actual Task tool call invocations.
Never simulate or summarize what an agent would say.
```

**Test cases**:
- Unit: `RoleConstraint::Facilitator` triggers anti-roleplay prompt injection
- Unit: `RoleConstraint::Delegate` and `Unconstrained` do not inject
- Integration: Facilitator routing and delegate response flow

---

### W4-T09: Namespace-Separated Decision IDs

**Pattern**: namespace-separated-decision-ids@1.0.0
**Quality**: Clean
**Feasibility Score**: 70
**Sub-Wave**: 4A
**Effort**: Low-Medium (3-5 days)
**Approach**: Direct extraction
**Dependencies**: Wave 2 W2-T11 (comms DB)
**Migration**: v053_decision_log

**Files to create**:
- `conductor-core/src/deliberation.rs` -- `DecisionId`, `DecisionNamespace`, registry
- `conductor-core/src/db/migrations/v053_decision_log.sql` -- `decision_log` + `decision_namespaces` tables

**Files to modify**:
- `conductor-core/src/lib.rs` -- Add `pub mod deliberation;`

**New types**:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionId {
    pub namespace: String,     // e.g., "ARCH", "SEC", "PERF"
    pub sequence_num: u32,     // Sequential within namespace
}

impl DecisionId {
    pub fn format(&self) -> String {
        format!("{}-{}", self.namespace, self.sequence_num)
    }
}

pub struct DecisionNamespace {
    pub prefix: String,
    pub description: String,
    pub workflow_run_id: Option<String>,
}
```

**Decision ID format**: `{PREFIX}-{NNN}` where PREFIX is 2-4 uppercase characters.

**Test cases**:
- Unit: DecisionId format and uniqueness within namespace (property test: generate N IDs, no collisions)
- Integration: Decision IDs persisted correctly with namespace isolation
- Integration: Multi-workflow concurrent decision logging does not collide

---

### W4-T10: Parallel First-Round Independence

**Pattern**: parallel-first-round-independence@1.0.0
**Quality**: Moderate
**Feasibility Score**: 60
**Sub-Wave**: 4B
**Effort**: Medium (1 week)
**Approach**: Extract-and-adapt
**Dependencies**: W4-T08 (facilitator-delegate separation)

**Principle**: All agents produce first-round assessments in parallel without seeing each other's responses. A synchronization barrier collects ALL responses before presenting ANY.

**Files to create**: None

**Files to modify**:
- `conductor-core/src/workflow_dsl/types.rs` -- Add `synchronization_mode: SyncMode` to `ParallelNode`
- `conductor-core/src/workflow_dsl/parser.rs` -- Parse `sync = "collect_all"` attribute on parallel blocks
- `conductor-core/src/workflow/executors.rs` -- In `execute_parallel()`, when `sync = CollectAll`, withhold results until all complete; add `parallel_marker` to step context

**New types**:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SyncMode {
    /// Collect results as they complete, present all at once after barrier
    CollectAll,
    /// Present results as they arrive (no independence guarantee)
    StreamAsComplete,
    /// Return after first success (existing fail_fast inverse)
    FirstSuccess,
}
```

**Workflow DSL example**:

```
parallel (sync = collect_all) {
  call security_reviewer { prompt = "Review for security issues" }
  call performance_reviewer { prompt = "Review for performance issues" }
  call architecture_reviewer { prompt = "Review for architectural concerns" }
}
call facilitator {
  prompt = "Synthesize the independent reviews: {{steps.parallel_1.outputs}}"
}
```

**Test cases**:
- Unit: `SyncMode::CollectAll` withholds individual results until all agents complete
- Integration: Two-agent parallel block produces independent outputs (no cross-contamination in round 1)
- Unit: Per-agent timeout within parallel blocks prevents hangs

---

### W4-T11: Context Window Exhaustion Guard

**Pattern**: context-window-exhaustion-guard@1.0.0
**Quality**: Moderate
**Feasibility Score**: 58
**Sub-Wave**: 4A
**Effort**: Medium (1-2 weeks)
**Approach**: Extract-and-adapt
**Dependencies**: Wave 1 checkpoint-persistence-protocol
**Migration**: v054_context_tracking

**Implementation**: The guard operates at two levels:
1. **Workflow-level guard** (cross-cutting): At every step boundary, estimate cumulative token usage and check against threshold
2. **Agent-level guard** (preventive discipline): Inject context monitoring instructions into agent prompts for long-running steps

**Files to create**:
- `conductor-core/src/context_guard.rs` -- `ContextGuard`, `ContextGuardConfig`, `CheckFrequency`
- `conductor-core/src/db/migrations/v054_context_tracking.sql` -- Add `estimated_tokens_used` to `workflow_runs`

**Files to modify**:
- `conductor-core/src/workflow/engine.rs` -- Insert guard check at step boundaries
- `conductor-core/src/workflow_dsl/types.rs` -- Add `context_guard: Option<ContextGuardConfig>` to `WorkflowDef`
- `conductor-core/src/agent/context.rs` -- Inject context monitoring instruction into agent startup
- `conductor-core/src/workflow/types.rs` -- Extend `BlockedOn` enum with `ContextExhaustion` variant

**New types**:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextGuardConfig {
    pub threshold_pct: u32,              // Default: 15
    pub max_context_tokens: Option<u64>, // Default: model-dependent
    pub check_frequency: CheckFrequency,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CheckFrequency {
    EveryPhaseTransition,
    EveryStepBoundary,
    EveryNSteps(u32),
}

pub struct ContextGuard {
    config: ContextGuardConfig,
    estimated_tokens_used: u64,
}
```

**BlockedOn extension**:

```rust
pub enum BlockedOn {
    HumanApproval { ... },
    PrApproval { ... },
    PrChecks { ... },
    ContextExhaustion {           // NEW
        tokens_used: u64,
        tokens_remaining_pct: u32,
        last_completed_step: String,
        checkpoint_summary: String,
    },
}
```

**Risk factors**:
- Token estimation is inherently imprecise; false positives interrupt workflows, false negatives allow degradation
- For single-step workflows with one long agent run, the guard cannot fire until the step completes
- Start with conservative threshold (20%), provide user override, log all guard decisions

**Test cases**:
- Unit: Guard triggers at threshold (set to 15%, simulate 86% usage)
- Unit: Guard does not trigger below threshold
- Integration: Guard transitions workflow to Waiting state with `BlockedOn::ContextExhaustion`
- Unit: Token estimation accumulates correctly across multiple steps

---

### W4-T12: Supervised Autonomy Model

**Pattern**: supervised-autonomy-model@1.0.0
**Quality**: Moderate
**Feasibility Score**: 55
**Sub-Wave**: 4B
**Effort**: Medium (1-2 weeks)
**Approach**: Extract-and-adapt
**Dependencies**: Wave 2 W2-T16 (human checkpoint); W4-T11 (context guard)

**Principle**: A single `autonomy_level` parameter controls governance posture from fully supervised to fully autonomous. Checkpoints are classified as intrinsic (always pause) or extrinsic (system-triggered, may auto-proceed).

**Files to create**:
- `conductor-core/src/autonomy.rs` -- `AutonomyLevel`, `AutonomyConfig`, `CheckpointClassification`, `IntrinsicTrigger`

**Files to modify**:
- `conductor-core/src/workflow_dsl/types.rs` -- Add `autonomy: Option<AutonomyConfig>` to `WorkflowDef`
- `conductor-core/src/workflow/engine.rs` -- Autonomy-aware gate evaluation
- `conductor-core/src/workflow/executors.rs` -- Per-item retry bound enforcement; gate classification logic
- `conductor-core/src/config.rs` -- Add `autonomy` section to global config
- `conductor-core/src/lib.rs` -- Add `pub mod autonomy;`

**New types**:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum AutonomyLevel {
    FullySupervised,    // Every gate pauses for human input
    Supervised,         // Intrinsic gates pause; extrinsic auto-proceed with logging
    SemiAutonomous,     // Only intrinsic gates pause; most quality gates auto-proceed
    FullyAutonomous,    // No gates pause; all decisions automated (highest risk)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutonomyConfig {
    pub level: AutonomyLevel,
    pub per_item_bound: u32,             // Max retries per step before escalation
    pub per_session_bound: u32,          // Max steps per workflow run before pause
    pub capacity_threshold_pct: u32,     // Delegates to ContextGuardConfig
    pub intrinsic_triggers: Vec<IntrinsicTrigger>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IntrinsicTrigger {
    BlockerResolution,
    VerificationVerdict,
    ThresholdFailure,
    SecurityReview,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CheckpointClassification {
    Intrinsic,  // Always pause, regardless of autonomy level
    Extrinsic,  // May auto-proceed at higher autonomy levels
}
```

**Gate evaluation modification** (`executors.rs`):

```rust
let classification = classify_checkpoint(&gate_node, &autonomy_config);
match (classification, autonomy_config.level) {
    (Intrinsic, _) => poll_for_gate_approval(...),
    (Extrinsic, SemiAutonomous | FullyAutonomous) => {
        // Auto-proceed with audit log
        tracing::info!("Auto-proceeding extrinsic gate at {:?}", autonomy_config.level);
        wf_mgr.update_step_status(step_id, Completed, Some("Auto-approved by autonomy policy"), ...)?;
    }
    _ => poll_for_gate_approval(...),
}
```

**Risk factors**:
- Checkpoint misclassification (labeling intrinsic as extrinsic) at higher autonomy levels can bypass required human approval
- Default all new gates to intrinsic; require explicit `extrinsic` annotation
- Conservative default: `Supervised` level

**Test cases**:
- Unit: Each gate type classified correctly at each autonomy level
- Unit: Per-item bound enforcement (N failures -> escalation)
- Integration: Extrinsic gate auto-approves at `SemiAutonomous` level
- Integration: Intrinsic gate always pauses regardless of autonomy level

---

## Prerequisite: executors.rs Pre-Extraction

**Rationale**: 8 of 12 Wave 4 patterns modify `conductor-core/src/workflow/executors.rs`, which is already 3,204 lines. Concurrent modifications to a single monolithic file create merge conflicts and increase cognitive load.

**Recommended extraction** (before Wave 4 begins):

| Sub-module | Contents |
|------------|----------|
| `executors/call.rs` | `execute_call()` and agent invocation logic |
| `executors/gate.rs` | `execute_gate()` and gate evaluation logic |
| `executors/parallel.rs` | `execute_parallel()` and synchronization logic |
| `executors/script.rs` | `execute_script()` and subprocess management |
| `executors/mod.rs` | Re-exports and shared types |

This is a behavior-preserving refactoring (no new functionality), and should be done as a separate PR before Wave 4 work begins.

---

## Risk Assessment

### Highest-Risk Patterns

| Risk | Pattern | Description | Mitigation |
|------|---------|-------------|------------|
| **Critical** | W4-T04 (gated pipeline) | Composite, 4 Wave 3 primitives, feasibility 45 | Implement last; gate each Wave 3 primitive before starting |
| **Critical** | W4-T07 (recovery cycle) | 4 escalation tiers, data loss risk at tier 3+ | Start with tier 1 only; never auto-execute tier 3+ without opt-in |
| **High** | W4-T11 (context guard) | Token estimation imprecise; false positives/negatives | Start at 20% threshold; provide user override; log all decisions |
| **High** | W4-T12 (supervised autonomy) | Checkpoint misclassification risk | Default all gates to intrinsic; require explicit extrinsic annotation |
| **Medium** | W4-T10 (parallel indep.) | Sync barrier adds latency; agent hangs block parallel block | Per-agent timeout; `fail_fast` escape hatch |
| **Medium** | W4-T05 (triad) | Invalid planner output wastes executor and validator | Plan validation step; manual plan override |

### Cross-Cutting Risks

1. **Wave dependency cascade**: If Wave 2 (largest wave) is delayed, most Wave 4 work blocks. Mitigation: W4-T06 and W4-T11 have no/minimal Wave 2 dependency and can start early.

2. **executors.rs modification overload**: 8/12 patterns touch this 3,204-line file. Mitigation: pre-extract into sub-modules before Wave 4 begins.

3. **BlockedOn enum explosion**: Wave 4 adds `ContextExhaustion` and `RecoveryEscalation` variants. The TUI must render and resume each. Mitigation: add variants early; implement TUI rendering as part of variant addition.

4. **Token estimation accuracy**: Lagging indicator (post-completion, not mid-execution). For long single-step workflows, guard fires too late. Mitigation: complementary agent-level prompt injection for preventive discipline.

5. **Multi-agent test non-determinism**: Agent outputs vary across runs. Mitigation: mock agents with deterministic outputs for integration tests; real-agent tests reserved for manual E2E validation.

---

## Test Strategy Summary

### Test Infrastructure Requirements

1. **Mock agent execution**: Simulate agent completion without tmux windows. Extend `test_helpers.rs` with mock agent results.
2. **Deterministic parallel execution**: Control execution ordering via test-mode synchronization primitives.
3. **Token estimation mocking**: Predictable `input_tokens`/`output_tokens` on `AgentRun` records.
4. **Desync injection**: Create desyncs for consistency checker tests (e.g., delete tmux window name while record shows `running`).

### Test Coverage by Sub-Wave

| Sub-Wave | Unit Tests | Integration Tests | Property Tests |
|----------|-----------|-------------------|---------------|
| 4A | Filter predicates, guard threshold, DecisionId format, RoleConstraint injection | Filter wired to discovery, guard -> Waiting transition, concurrent decision logging | DecisionId uniqueness |
| 4B | Template slot resolution, context struct population, SyncMode semantics, autonomy classification | Template -> valid workflow, state comms round-trip, triad ordering, extrinsic gate auto-approve | -- |
| 4C | Pipeline stage sequencing, tier escalation, retry limits | Pipeline reject on failure, desync -> recovery, transient failure recovery, human escalation | Pipeline idempotency |

---

## Summary Statistics

| Metric | Value |
|--------|-------|
| Total patterns | 12 |
| New modules to create | 7 (`workflow_template.rs`, `cross_cutting.rs`, `verification_pipeline.rs`, `context_guard.rs`, `autonomy.rs`, `recovery.rs`, `deliberation.rs`) |
| Patterns requiring only modifications | 5 |
| New DB migrations | 2 (v053_decision_log, v054_context_tracking) |
| Patterns touching executors.rs | 8 of 12 |
| Composite patterns (confidence penalty) | 3 (W4-T01 -15, W4-T04 -20, W4-T07 -20) |
| Average feasibility score | 58.8 |
| Highest feasibility | W4-T08 facilitator-delegate (75) |
| Lowest feasibility | W4-T07 autonomous recovery (40) |
| Clean extraction points | 3 (W4-T06, W4-T08, W4-T09) |
| Moderate extraction points | 6 (W4-T01, W4-T02, W4-T03, W4-T05, W4-T10, W4-T12) |
| Difficult extraction points | 2 (W4-T04, W4-T07) |
| Not extractable (current state) | 1 (W4-T11 -- token estimation accuracy risk) |
| Highest coupling pair | W4-T04 <-> Wave 3 evidence system (6+ coupling points) |
| Sub-waves | 3 (4A: 2-3w, 4B: 3-4w, 4C: 3-4w) |
| Estimated total effort | 8-12 weeks |
