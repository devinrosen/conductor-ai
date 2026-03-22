---
wave: 5
title: "Remaining Lifecycle-Gating + Operational Structures"
status: pending
pattern_count: 9
os_count: 111
depends_on: [4]
estimated_effort: "40-55 days (25-35 patterns, 15-20 OS adaptation)"
new_rust_loc: "2,500-3,500"
new_files: ~65
modified_files: ~5
adapt_count: 38
reference_count: 49
defer_count: 24
composite_patterns_requiring_manual_review: 3
---

# Wave 5: Remaining Lifecycle-Gating + Operational Structures

## Executive Summary

Wave 5 is the largest and most heterogeneous wave. It contains two structurally distinct workstreams: (1) nine lifecycle-gating patterns that formalize scoring, thresholding, and gated progression within conductor's workflow engine, and (2) 111 operational structures (agents, commands, schemas, decisions, skills, protocols) that must be triaged, adapted, and mapped to conductor-ai's existing filesystem conventions.

Seven of the nine lifecycle-gating patterns share a common ancestor (`threshold-based-decision-branching`) and compose hierarchically. The integration strategy implements the base threshold evaluation as a reusable Rust module in `conductor-core/src/scoring/` (~10 files, 2,500-3,500 LOC), then expresses specializations as configuration-driven gate definitions. This creates a deep module with a simple interface -- a single `evaluate_threshold()` function that accepts a gate definition and a score.

The 111 operational structures are NOT integrated as Rust code. They are file-based artifacts mapped to conductor's existing `.conductor/` directory conventions. OS triage results: **38 adapt (34%)**, **49 reference (44%)**, **24 defer (22%)**. No new database migrations are needed -- scoring is in-memory, OS are filesystem-based.

**Critical DSL gap**: Three roundtable commands (roundtable-strategic, roundtable-command-debug, roundtable-ux) require interactive multi-agent conversation that exceeds the workflow DSL's execution model. These are deferred.

---

## Pattern Dependency Graph

```
threshold-based-decision-branching (P1: base)
  |
  +-- confidence-gated-task-progression (P2: binary specialization)
  +-- weighted-alignment-scoring (P3: multi-factor specialization)
  +-- multi-dimension-readiness-gate (P4: N-dimension specialization)
  +-- score-fix-rescore-convergence-loop (P5: iterative wrapper) [COMPOSITE: -20 penalty]
  +-- performance-gated-workflow-variant (P6: workflow injection)
  +-- quality-gate-layering (P7: tier orchestration) [COMPOSITE: -20 penalty]
  +-- verification-gate-template-instantiation (P9: meta-template) [COMPOSITE: -20 penalty]

command-tier-classification (P8: standalone classification, GRS=30/low)
```

## Patterns (9)

| # | Pattern | Version | Domain | GRS | Strategy | Feasibility |
|---|---------|---------|--------|-----|----------|-------------|
| P1 | threshold-based-decision-branching | 1.0.0 | lifecycle-gating | medium | Anchored CoT | 80 |
| P2 | confidence-gated-task-progression | 1.0.0 | lifecycle-gating | medium | Anchored CoT | 80 |
| P3 | weighted-alignment-scoring | 1.1.0 | lifecycle-gating | medium | Anchored CoT | 75 |
| P4 | multi-dimension-readiness-gate | 1.1.0 | lifecycle-gating | medium | Anchored CoT | 75 |
| P5 | score-fix-rescore-convergence-loop | 1.0.0 | lifecycle-gating | medium | Anchored CoT | 55* |
| P6 | performance-gated-workflow-variant | 1.0.0 | lifecycle-gating | medium | Anchored CoT | 75 |
| P7 | quality-gate-layering | 1.0.0 | lifecycle-gating | medium | Anchored CoT | 50* |
| P8 | command-tier-classification | 1.0.0 | lifecycle-gating | 30 (low) | Direct Prompting | 85 |
| P9 | verification-gate-template-instantiation | 1.0.0 | lifecycle-gating | medium | Anchored CoT | 50* |

*Composite pattern penalty applied (-20 points). Manual review required before implementation.

---

## Operational Structure Triage Summary (111 items)

| Category | Total | Adapt | Reference | Defer |
|----------|-------|-------|-----------|-------|
| Agents | 32 | 12 | 10 | 10 |
| Commands | 19 | 8 | 7 | 4 |
| Schemas | 22 | 6 | 12 | 4 |
| Decisions | 27 | 8 | 15 | 4 |
| Skills | 9 | 3 | 4 | 2 |
| Protocols | 2 | 1 | 1 | 0 |
| **TOTAL** | **111** | **38** | **49** | **24** |

Adapt rate: 34% -- below the 40% sub-phasing threshold. Wave 5 proceeds as a single phase for OS adaptation.

---

## Per-Item Triage Tables

### Agents (32) -- 12 Adapt, 10 Reference, 10 Defer

| # | Agent | Lines | Triage | Rationale |
|---|-------|-------|--------|-----------|
| 1 | claude-code-guide | -- | **A** | Directly applicable: Claude Code workflow design for conductor |
| 2 | prompt-analyst | -- | **A** | Applicable: prompt quality analysis for conductor agents |
| 3 | prompt-engineer | -- | **A** | Applicable: prompt construction for conductor agents |
| 4 | tsdlc-autonomous-debugger | 397 | **A** | Adaptable: autonomous debugging methodology applies to Rust/conductor |
| 5 | tsdlc-debugger | 199 | **A** | Adaptable: general debugging methodology |
| 6 | tsdlc-doc-librarian | 290 | **A** | Applicable: doc reorganization applies to any codebase |
| 7 | tsdlc-doc-writer | 427 | **A** | Applicable: doc creation applies to any codebase |
| 8 | tsdlc-engineering-lead | 162 | **A** | Adaptable: process improvement, DX, tech debt governance |
| 9 | tsdlc-planner | 379 | **A** | Applicable: milestone planning methodology |
| 10 | tsdlc-preplanner | 579 | **A** | Applicable: research readiness assessment |
| 11 | tsdlc-verification-engineer | -- | **A** | Applicable: evidence-based verification |
| 12 | tsdlc-handoff-compliance | 293 | **A** | Applicable: milestone handoff process |
| 13 | tsdlc-command-remediator | 78 | **R** | Reference: slash command remediation patterns |
| 14 | tsdlc-milestone-aligner | 701 | **R** | Reference: alignment scoring methodology |
| 15 | tsdlc-platform-architect | 260 | **R** | Reference: architecture patterns (Go-specific internals) |
| 16 | tsdlc-product-manager | 127 | **R** | Reference: product strategy methodology |
| 17 | tsdlc-program-manager | 106 | **R** | Reference: program coordination patterns |
| 18 | tsdlc-progress-analyst | 704 | **R** | Reference: progress analysis methodology |
| 19 | tsdlc-qa-verification-engineer | -- | **R** | Reference: QA methodology |
| 20 | tsdlc-research-readiness-assessor | -- | **R** | Reference: research assessment rubric |
| 21 | tsdlc-sdlc-guide | -- | **R** | Reference: SDLC guidance methodology |
| 22 | tsdlc-skill-tooling-developer | -- | **R** | Reference: skill/tool development patterns |
| 23 | tsdlc-go-cli-architect | 754 | **D** | Go-specific: irrelevant to Rust/conductor |
| 24 | tsdlc-go-services-architect | 1220 | **D** | Go-specific: irrelevant to Rust/conductor |
| 25 | tsdlc-typescript-services-architect | -- | **D** | TypeScript-specific: irrelevant to Rust/conductor |
| 26 | tsdlc-web-frontend-architect | -- | **D** | Frontend-specific: conductor-web is minimal |
| 27 | tsdlc-playwright-executor | 244 | **D** | Playwright-specific: conductor uses cargo test |
| 28 | tsdlc-ux-specialist | -- | **D** | UX-specific: conductor TUI has different patterns |
| 29 | tsdlc-visual-designer | -- | **D** | Visual design: global-sdlc specific |
| 30 | lively-people-ops-manager | -- | **D** | Company-specific: Lively HR agent |
| 31 | product-intel-analyst | -- | **D** | Company-specific: competitive intelligence |
| 32 | sidebar-command-executor | -- | **D** | Platform-specific: sidebar UI executor |

### Commands (19) -- 8 Adapt, 7 Reference, 4 Defer

| # | Command | Triage | Rationale |
|---|---------|--------|-----------|
| 1 | verify | **A** | Directly applicable: evidence-based verification for conductor workflows |
| 2 | project-status | **A** | Adaptable: progress analysis maps to conductor workflow/agent run status |
| 3 | preplan | **A** | Adaptable: research readiness assessment before implementation |
| 4 | plan-milestone | **A** | Adaptable: milestone planning maps to conductor feature planning |
| 5 | pr | **A** | Adaptable: PR workflow already exists in conductor (iterate-pr.wf) |
| 6 | align-milestone | **A** | Adaptable: alignment scoring uses lifecycle-gating patterns |
| 7 | handoff-milestone | **A** | Adaptable: milestone handoff process |
| 8 | ship-milestone | **A** | Adaptable: milestone completion workflow |
| 9 | agent-generator | **R** | Reference: meta-agent generation methodology |
| 10 | explore-backlog | **R** | Reference: backlog exploration methodology |
| 11 | qa-test | **R** | Reference: QA testing methodology |
| 12 | update-project-state | **R** | Reference: state update patterns |
| 13 | information-topology | **R** | Reference: information architecture analysis |
| 14 | create-theme | **R** | Reference: theme creation patterns |
| 15 | remediate-all-commands | **R** | Reference: bulk command remediation |
| 16 | roundtable-strategic | **D** | CRITICAL DSL GAP: multi-agent roundtable exceeds workflow DSL |
| 17 | roundtable-command-debug | **D** | CRITICAL DSL GAP: multi-agent roundtable exceeds workflow DSL |
| 18 | roundtable-ux | **D** | CRITICAL DSL GAP: multi-agent roundtable exceeds workflow DSL |
| 19 | roundtable-people-ops | **D** | Company-specific and complex |

### Schemas (22) -- 6 Adapt, 12 Reference, 4 Defer

| # | Schema | Triage | Rationale |
|---|--------|--------|-----------|
| 1 | bug.schema.yaml | **A** | Maps to conductor ticket type with priority/severity |
| 2 | decision.schema.yaml | **A** | Maps to `.conductor/decisions/` format |
| 3 | checkpoint.schema.yaml | **A** | Maps to workflow gate checkpoints |
| 4 | change-request.schema.yaml | **A** | Maps to conductor ticket lifecycle extension |
| 5 | bypass.schema.yaml | **A** | Maps to gate override/bypass mechanism (DEC-004 escape hatch) |
| 6 | escalation.schema.yaml | **A** | Maps to workflow blocked-on escalation |
| 7 | project.schema.yaml | **R** | Reference: conductor uses repos, not projects |
| 8 | story.schema.yaml | **R** | Reference: conductor uses tickets, not stories |
| 9 | deliverable.schema.yaml | **R** | Reference: no direct conductor equivalent |
| 10 | goal.schema.yaml | **R** | Reference: alignment scoring reference |
| 11 | objective.schema.yaml | **R** | Reference: no direct equivalent |
| 12 | tech-debt.schema.yaml | **R** | Reference: useful for future ticket type extension |
| 13 | tech-request.schema.yaml | **R** | Reference: useful for future ticket type extension |
| 14 | user.schema.yaml | **R** | Reference: conductor has no user entity (single-user tool) |
| 15 | charter.schema.yaml | **R** | Reference: organizational scope document |
| 16 | codebase.schema.yaml | **R** | Reference: conductor uses repos table |
| 17 | domain.schema.yaml | **R** | Reference: organizational grouping |
| 18 | action-item.schema.yaml | **R** | Reference: meeting-originated action items |
| 19 | meeting.schema.yaml | **D** | Domain-specific: meeting management |
| 20 | agenda-item.schema.yaml | **D** | Domain-specific: meeting agendas |
| 21 | workgroup.schema.yaml | **D** | Domain-specific: organizational workgroups |
| 22 | README.md | **D** | Documentation, not a schema |

### Decisions (27) -- 8 Adapt, 15 Reference, 4 Defer

| # | Decision | Triage | Rationale |
|---|----------|--------|-----------|
| 1 | DEC-001 (Two-Layer Architecture) | **A** | Pattern: separate agent populations for different concerns |
| 2 | DEC-002 (Hard Fork Model) | **A** | Pattern: independent evolutionary branches |
| 3 | DEC-004 (Escape Hatch) | **A** | Directly applicable: SDLC as tool not constraint |
| 4 | DEC-017 (Shared Domain) | **A** | Pattern: shared domain model |
| 5 | DEC-018 (Exit Codes) | **A** | Pattern: semantic exit codes for workflow scripts |
| 6 | DEC-122 | **A** | Assess for applicability |
| 7 | DEC-123 | **A** | Assess for applicability |
| 8 | DEC-124 | **A** | Assess for applicability |
| 9-23 | DEC-008 through DEC-309 | **R** | Reference: varying degrees of applicability |
| 24-27 | Various | **D** | Domain-specific: Lively/Vantage internal decisions |

### Skills (9) -- 3 Adapt, 4 Reference, 2 Defer

| # | Skill | Triage | Rationale |
|---|-------|--------|-----------|
| 1 | agent-generator | **A** | Applicable: generate conductor agent definitions |
| 2 | qa-verification | **A** | Applicable: QA verification methodology |
| 3 | bug-fix-templates | **A** | Applicable: bug fix workflow templates |
| 4 | debug-analysis-patterns | **R** | Reference: debugging methodology |
| 5 | research-readiness-assessment | **R** | Reference: assessment rubric |
| 6 | slash-command-remediation | **R** | Reference: command design patterns |
| 7 | test-structure-templates | **R** | Reference: test organization patterns |
| 8 | cli-tool-generator | **D** | Go-specific: CLI generation for Go |
| 9 | test-data-management | **D** | Domain-specific: test data patterns |

### Protocols (2) -- 1 Adapt, 1 Reference

| # | Protocol | Triage | Rationale |
|---|----------|--------|-----------|
| 1 | OUTPUT_CONTRACT.md | **A** | Directly applicable: output behavior rules for conductor agents |
| 2 | CLAUDE.md | **R** | Reference: conductor already has its own CLAUDE.md with different structure |

---

## Composable Scoring Module

New module: `conductor-core/src/scoring/` (~10 files, 2,500-3,500 LOC)

7/9 lifecycle patterns share `threshold-based-decision-branching` as base. The scoring module implements a deep module with simple interface pattern.

### Module Layout

```
conductor-core/src/scoring/
  mod.rs              -- Public API: evaluate_threshold(), evaluate_confidence(), evaluate_alignment()
  types.rs            -- GateOutcome, ActionType, ThresholdMutability, TierDefinition, ThresholdGate
  threshold.rs        -- Core threshold evaluation algorithm
  confidence.rs       -- Binary confidence gate (specialization of threshold)
  alignment.rs        -- Weighted multi-factor alignment scoring
  readiness.rs        -- N-dimension readiness assessment
  convergence.rs      -- Score-fix-rescore iteration loop
  convergence_types.rs -- ConvergenceState, IterationRecord, StuckDimensionAnalysis
  performance_gate.rs -- Before/after measurement protocol
  quality_layers.rs   -- Tiered quality gate orchestration
  gate_template.rs    -- Meta-template for gate instantiation
```

### Core Type Definitions

```rust
// conductor-core/src/scoring/types.rs
pub enum ActionType {
    Proceed,
    ProceedWithReview,
    Block,
    BlockAndRemediate,
}

pub enum ThresholdMutability {
    Immutable,       // Cannot be overridden
    TeamConfigurable, // Overridable via config
    Dynamic,         // Adjustable at runtime
}

pub struct TierDefinition {
    pub name: String,
    pub threshold: Option<f64>,  // None = catch-all
    pub action: ActionType,
    pub description: String,
    pub human_checkpoint: bool,
}

pub struct ThresholdGate {
    pub metric_name: String,
    pub score_range: (f64, f64),
    pub tiers: Vec<TierDefinition>,  // ordered highest-first
    pub mutability: ThresholdMutability,
}

pub struct GateOutcome {
    pub tier_name: String,
    pub action: ActionType,
    pub score: f64,
    pub human_checkpoint: bool,
}

// conductor-core/src/scoring/threshold.rs
pub fn evaluate_threshold(gate: &ThresholdGate, score: f64) -> GateOutcome {
    // Descending comparison, first match wins, catch-all guarantees match
}
```

### Composition with Existing Waves

- **Wave 3**: Gated verification pipeline (W3-T04) defined the concept of gates in workflow steps. P1 provides the numeric evaluation engine those gates call.
- **Wave 4**: Conditional branching (if/while nodes) provides the workflow-level routing that consumes gate outcomes. Gate types (`ci`, `pr`, `human`, `check`) provide building blocks for P7.

---

## Implementation Phases

### Phase 1: Foundation (5-8 days)

| Task | Pattern | Files | Effort |
|------|---------|-------|--------|
| P1 | threshold-based-decision-branching | `scoring/mod.rs`, `scoring/threshold.rs`, `scoring/types.rs`, `lib.rs` (modify), `.conductor/schemas/threshold-gate.yaml` | Medium (3-5 days) |
| P8 | command-tier-classification | `config.rs` (modify), `main.rs` (modify), `config.toml` (modify) | Low (2-3 days) |

P1 and P8 are independent and can proceed in parallel. P8 uses Direct Prompting (GRS=30/low) -- a config-driven lookup table, not a scoring engine.

### Phase 2: Parallel Specializations (5-7 days)

| Task | Pattern | Files | Effort |
|------|---------|-------|--------|
| P2 | confidence-gated-task-progression | `scoring/confidence.rs`, `.conductor/schemas/confidence-gate.yaml` | Low (1-2 days) |
| P3 | weighted-alignment-scoring | `scoring/alignment.rs`, `.conductor/schemas/alignment-scoring.yaml` | Medium (2-3 days) |
| P4 | multi-dimension-readiness-gate | `scoring/readiness.rs`, `.conductor/schemas/readiness-gate.yaml` | Medium (3-4 days) |

All three are direct specializations of P1. They can proceed in parallel once P1 is complete.

### Phase 3: Composite Patterns (9-11 days)

| Task | Pattern | Files | Effort | Penalty |
|------|---------|-------|--------|---------|
| P5 | score-fix-rescore-convergence-loop | `scoring/convergence.rs`, `scoring/convergence_types.rs` | Medium (3-4 days) | -20 (composite) |
| P6 | performance-gated-workflow-variant | `scoring/performance_gate.rs`, `.conductor/schemas/performance-gate.yaml` | Medium (2-3 days) | -- |
| P7 | quality-gate-layering | `scoring/quality_layers.rs`, `.conductor/schemas/quality-layers.yaml` | Medium (3-4 days) | -20 (composite) |

P5, P6, P7 depend on Phase 1 (P1 base). P7 additionally depends on W4-T04 (gated verification pipeline from Wave 4).

### Phase 4: Meta-Template (3-4 days)

| Task | Pattern | Files | Effort | Penalty |
|------|---------|-------|--------|---------|
| P9 | verification-gate-template-instantiation | `scoring/gate_template.rs`, `.conductor/schemas/gate-template.yaml`, `.conductor/schemas/gates/` (directory) | Medium (3-4 days) | -20 (composite) |

P9 depends on W5-T07 (P7: quality gate layering). It is the factory pattern for P7's quality layers.

### Phase 5: OS Adaptation (15-20 days, parallelizable)

OS adaptation proceeds in parallel with pattern phases where possible.

---

## Tasks

### W5-T01: Threshold-Based Decision Branching
- **Pattern**: threshold-based-decision-branching@1.0.0
- **Target**: `conductor-core/src/scoring/` (NEW module)
- **Files to create**: `scoring/mod.rs`, `scoring/threshold.rs`, `scoring/types.rs`, `.conductor/schemas/threshold-gate.yaml`, `conductor-core/tests/scoring_threshold_tests.rs`
- **Files to modify**: `conductor-core/src/lib.rs` (add `pub mod scoring;`)
- **Action**: Implement `TierDefinition`, `ThresholdGate`, `evaluate_threshold()` function. Descending tier comparison with catch-all guarantee.
- **Test cases**: Exact boundary values (score == threshold), catch-all tier, empty tier list, descending order validation
- **Effort**: Medium (3-5 days)

### W5-T02: Confidence-Gated Progression
- **Pattern**: confidence-gated-task-progression@1.0.0
- **Depends**: W5-T01 (uses evaluate_threshold from P1)
- **Target**: `conductor-core/src/scoring/confidence.rs`
- **Files to create**: `scoring/confidence.rs`, `.conductor/schemas/confidence-gate.yaml`, `conductor-core/tests/scoring_confidence_tests.rs`
- **Action**: Binary gate specialization. Default 70% threshold, READY/BLOCKED tiers, blocker taxonomy. Composes with Wave 4's `gate` node executor.
- **Test cases**: Score at 70 (pass), 69 (fail), 0, 100; blocker classification
- **Effort**: Low (1-2 days)

### W5-T03: Weighted Alignment Scoring
- **Pattern**: weighted-alignment-scoring@1.1.0
- **Depends**: W5-T01
- **Target**: `conductor-core/src/scoring/alignment.rs`
- **Files to create**: `scoring/alignment.rs`, `.conductor/schemas/alignment-scoring.yaml`
- **Action**: `AlignmentScorer` with weighted formula (coverage_weight * coverage_score + alignment_weight * alignment_score). Dual-mode thresholds (planning/delivery). Consumes `evaluate_threshold()` for final score-to-outcome mapping.
- **Test cases**: Weight sensitivity (coverage=0.6, alignment=0.4); dual-mode thresholds
- **Effort**: Medium (2-3 days)

### W5-T04: Multi-Dimension Readiness Gate
- **Pattern**: multi-dimension-readiness-gate@1.1.0
- **Depends**: W5-T01
- **Target**: `conductor-core/src/scoring/readiness.rs`
- **Files to create**: `scoring/readiness.rs`, `.conductor/schemas/readiness-gate.yaml`
- **Action**: N-dimension scoring with per-dimension minimums, composite scoring, bounded remediation. Pre-workflow gate: check readiness across dimensions (code ready, tests passing, deps installed, env configured). Bounded remediation loop maps to workflow DSL `do {} while` with `max_iterations`.
- **Test cases**: Per-dimension minimum enforcement; high composite with one zero dimension
- **Effort**: Medium (3-4 days)

### W5-T05: Score-Fix-Rescore Convergence
- **Pattern**: score-fix-rescore-convergence-loop@1.0.0
- **Depends**: W5-T01
- **Composite penalty**: -20 points (feasibility: 55)
- **Target**: `conductor-core/src/scoring/convergence.rs`
- **Files to create**: `scoring/convergence.rs`, `scoring/convergence_types.rs`
- **Action**: Convergence loop orchestrator with min/max iterations, delta tracking, role separation enforcement. Maps to `do { call scorer; if below_threshold { call fixer } } while !converged`. Workflow engine's existing `workflow_runs.iteration` provides persistence slot.
- **Test cases**: Min iterations enforced; max iterations terminate; delta tracking; role separation
- **Effort**: Medium (3-4 days)
- **Manual review required**: Yes (composite pattern)

### W5-T06: Performance-Gated Workflow Variants
- **Pattern**: performance-gated-workflow-variant@1.0.0
- **Depends**: W5-T01
- **Target**: `conductor-core/src/scoring/performance_gate.rs`
- **Files to create**: `scoring/performance_gate.rs`, `.conductor/schemas/performance-gate.yaml`
- **Action**: Before/after measurement protocol, threshold comparison, semantic exit codes. Maps to wrapper workflow with pre/post measurement steps. `call workflow` syntax already supports sub-workflow invocation.
- **Test cases**: Before/after measurement comparison; regression threshold; remediation routing
- **Effort**: Medium (2-3 days)

### W5-T07: Quality Gate Layering
- **Pattern**: quality-gate-layering@1.0.0
- **Depends**: W4-T04 (gated verification pipeline from Wave 4)
- **Composite penalty**: -20 points (feasibility: 50)
- **Target**: `conductor-core/src/scoring/quality_layers.rs`
- **Files to create**: `scoring/quality_layers.rs`, `.conductor/schemas/quality-layers.yaml`
- **Action**: Tier orchestrator: Tier 1 = script steps (lint, test), Tier 2 = call steps (agent review), Tier 3 = gate steps (human approval). Sequential gating maps to workflow DSL sequential execution. Wave 4's gate types (`ci`, `pr`, `human`, `check`) provide building blocks.
- **Test cases**: Sequential gating (tier 1 fail blocks tier 2); layer composition
- **Effort**: Medium (3-4 days)
- **Manual review required**: Yes (composite pattern)
- **NOTE**: Dependency is on W4-T04 (not W3-T04). This was corrected from an earlier version.

### W5-T08: Command Tier Classification
- **Pattern**: command-tier-classification@1.0.0 (GRS=30, Direct Prompting)
- **Target**: `conductor-cli/src/main.rs`, `conductor-core/src/config.rs`
- **Files to create**: `conductor-core/tests/command_tier_tests.rs`
- **Files to modify**: `.conductor/config.toml` (add `[command_tiers]` section), `conductor-core/src/config.rs` (add `CommandTier` enum, `CommandTierConfig`), `conductor-cli/src/main.rs` (read tier, apply behavioral profile)
- **Action**: Classify CLI commands as P0 (critical), P1 (important), P2 (utility); gate test requirements accordingly. Config-driven lookup table. Conservative default: unknown commands get highest tier.
- **Test cases**: Classification lookup; conservative default (unknown -> highest tier)
- **Effort**: Low (2-3 days)

### W5-T09: Verification Gate Template Instantiation
- **Pattern**: verification-gate-template-instantiation@1.0.0
- **Depends**: W5-T07 (quality gate layering)
- **Composite penalty**: -20 points (feasibility: 50)
- **Target**: `conductor-core/src/scoring/gate_template.rs`
- **Files to create**: `scoring/gate_template.rs`, `.conductor/schemas/gate-template.yaml`, `.conductor/schemas/gates/` (new directory with instantiations)
- **Action**: Meta-template with 5-section skeleton: checks, scoring formula, thresholds, output schema, human protocol. Factory pattern for P7's quality layers. Each layer in the quality pyramid is a gate instantiated from this template.
- **Test cases**: Template instantiation; schema validation of output; missing section detection
- **Effort**: Medium (3-4 days)
- **Manual review required**: Yes (composite pattern)

### W5-T10: Agent Definitions Adaptation
- **OS Category**: agents (32 total; 12 adapt, 10 reference, 10 defer)
- **Target**: `.conductor/agents/` directory

**Agent Mapping Table (12 Adapt items)**:

| Source Agent | Target Name | Role | Can Commit | Key Adaptations |
|-------------|-------------|------|-----------|----------------|
| claude-code-guide | `claude-code-guide` | actor | false | Remove global-sdlc references; add conductor workflow design guidance |
| prompt-analyst | `analyze-prompt` | reviewer | false | Add conductor agent file format knowledge |
| prompt-engineer | `engineer-prompt` | actor | true | Add conductor agent template knowledge |
| tsdlc-autonomous-debugger | `autonomous-debugger` | actor | true | Replace Go commands with Rust (cargo); replace file paths |
| tsdlc-debugger | `debugger` | actor | false | Replace Go commands with Rust; simplify |
| tsdlc-doc-librarian | `doc-librarian` | actor | true | Remove tsdlc prefix; adapt file structure references |
| tsdlc-doc-writer | `doc-writer` | actor | true | Remove tsdlc prefix; adapt file structure references |
| tsdlc-engineering-lead | `engineering-lead` | reviewer | false | Replace Go/global-sdlc references with Rust/conductor |
| tsdlc-planner | `planner` | actor | true | MERGE into existing `plan.md` -- do not overwrite |
| tsdlc-preplanner | `preplanner` | actor | false | New: research readiness assessment |
| tsdlc-verification-engineer | `verification-engineer` | reviewer | false | Adapt verification criteria to Rust/cargo test ecosystem |
| tsdlc-handoff-compliance | `handoff-compliance` | reviewer | false | Adapt handoff criteria to conductor feature lifecycle |

**Existing agent overlaps** (merge, do not overwrite):

| Existing Agent | Overlapping Source | Resolution |
|---------------|-------------------|------------|
| `plan.md` | tsdlc-planner | Merge planner methodology into existing plan.md |
| `diagnose-failure.md` | tsdlc-debugger | Merge debugging methodology into existing |

**Adaptation template**:
1. READ source agent from operational-library
2. EXTRACT persona, core principles, methodology, tool-use instructions
3. MAP frontmatter: `model` -> omit (use config.toml), `color` -> omit, add `role` and `can_commit`
4. ADAPT body: replace CLI references (tsdlc/sdlc -> conductor), build commands (Go -> cargo), file paths (.claude/ -> .conductor/)
5. ADD conductor template variables: `{{prior_context}}`, `{{ticket_id}}`
6. WRITE to `.conductor/agents/{adapted-name}.md`
7. VALIDATE: YAML frontmatter parses, required keys present

- **Test**: Agent definition schema validation (frontmatter parse, required keys)
- **Effort**: Medium (4-5 days for 12 adaptations)

### W5-T11: Command-to-Workflow Mapping
- **OS Category**: commands (19 total; 8 adapt, 7 reference, 4 defer)
- **Target**: `.conductor/workflows/` directory

**Command-to-Workflow Mapping (8 Adapt items)**:

| Command | Target Workflow | Mapping Strategy |
|---------|-----------------|-----------------|
| verify | `verify-task.wf` | Single agent call with structured output; gate on pass/fail |
| project-status | `project-status.wf` | Single agent call to analyze repo/workflow status |
| preplan | `preplan.wf` | Agent call for research assessment; gate on confidence score |
| plan-milestone | `plan-feature.wf` | Agent call to create implementation plan |
| pr | extends existing `iterate-pr.wf` | Merge PR workflow logic into existing workflow |
| align-milestone | `align-feature.wf` | Agent call for alignment scoring; conditional remediation |
| handoff-milestone | `handoff-feature.wf` | Sequential: create handoff notes, validate, archive |
| ship-milestone | `ship-feature.wf` | Sequential: verify all tasks, merge worktrees, create release |

**Adaptation template**:
1. READ source command from operational-library
2. EXTRACT goal statement, agent binding, step-by-step protocol, arguments/modes
3. DECOMPOSE protocol into workflow nodes:
   - "Use agent X to..." -> `call {agent-name} { as = "actor" }`
   - "Check condition..." -> `if {condition} { ... }`
   - "Repeat until..." -> `do { ... } while {condition}`
   - "Run CLI command..." -> `script {name} { run = ".conductor/scripts/{name}.sh" }`
   - "Gate on human approval..." -> `gate { type = human }`
4. MAP arguments to workflow inputs: `meta { inputs { arg_name = "description" } }`
5. WRITE to `.conductor/workflows/{adapted-name}.wf`
6. VALIDATE: workflow parses via conductor's `parse_workflow_str()`

**Critical DSL gap** -- 3 roundtable commands deferred:

| Command | Blocker |
|---------|---------|
| roundtable-strategic | Multi-agent real-time conversation with human-in-the-loop turn-taking |
| roundtable-command-debug | Multi-agent conversation with specialist subagent invocation mid-turn |
| roundtable-ux | Same architectural gap |

The workflow DSL does NOT support: interactive multi-agent conversation, dynamic agent selection based on conversation context, or human-in-the-loop turn-taking within a single workflow step. These require a future `roundtable` node type or separate execution engine.

- **Test**: Workflow definition validity via batch_validate module
- **Effort**: Medium (4-5 days for 8 adaptations)

### W5-T12: Schema Adaptation
- **OS Category**: schemas (22 total; 6 adapt, 12 reference, 4 defer)
- **Target**: `.conductor/schemas/` directory

**Schemas to adapt**:
1. `bug.schema.yaml` -> `.conductor/schemas/bug.yaml` (ticket type with priority/severity)
2. `decision.schema.yaml` -> `.conductor/schemas/decision.yaml` (decision record format)
3. `checkpoint.schema.yaml` -> `.conductor/schemas/checkpoint.yaml` (workflow gate checkpoints)
4. `change-request.schema.yaml` -> `.conductor/schemas/change-request.yaml` (ticket lifecycle extension)
5. `bypass.schema.yaml` -> `.conductor/schemas/bypass.yaml` (gate override/bypass, DEC-004 escape hatch)
6. `escalation.schema.yaml` -> `.conductor/schemas/escalation.yaml` (workflow blocked-on escalation)

**No new database migrations needed.** These schemas define structured output formats and validation rules, not SQLite table schemas. Scoring is in-memory; OS are filesystem-based.

**Adaptation template**:
1. READ source schema from operational-library
2. EXTRACT entity name, field definitions, status workflow, relationships
3. TRANSLATE fields: string -> String/TEXT, datetime -> TEXT (ISO 8601), integer -> INTEGER
4. REMOVE global-sdlc-specific references (meeting_id, workgroup_id)
5. ADD conductor-specific fields if needed (repo_id, worktree_id)
6. PRESERVE status_workflow as state machine documentation
7. WRITE to `.conductor/schemas/{adapted-name}.yaml`
8. VALIDATE: YAML parses, field types valid

- **Test**: Schema YAML parse; required fields (entity, fields, schema_version)
- **Effort**: Low (2-3 days for 6 adaptations)

### W5-T13: Decision Catalog
- **OS Category**: decisions (27 total; 8 adapt, 15 reference, 4 defer)
- **Target**: `.conductor/decisions/` directory (NEW)

**Decisions to adapt**: DEC-001 (Two-Layer Architecture), DEC-002 (Hard Fork Model), DEC-004 (Escape Hatch), DEC-017 (Shared Domain), DEC-018 (Exit Codes), DEC-122, DEC-123, DEC-124

**Adaptation template**:
1. READ source decision from operational-library
2. ASSESS applicability to conductor-ai
3. ADAPT: preserve Context/Decision/Rationale structure; replace global-sdlc references with conductor-ai equivalents
4. WRITE to `.conductor/decisions/{dec-id}.md`
5. VALIDATE: frontmatter parses, required sections present

- **Test**: Decision reference integrity (frontmatter parse, required sections)
- **Effort**: Low (2-3 days for 8 adaptations)

### W5-T14: Skills and Protocol Adaptation
- **OS Categories**: skills (9 total; 3 adapt, 4 reference, 2 defer), protocols (2 total; 1 adapt, 1 reference)
- **Target**: `.conductor/workflows/` + `.conductor/agents/` (skills decompose into workflows + agent prompts)

**Skills to adapt**: agent-generator, qa-verification, bug-fix-templates
**Protocol to adapt**: OUTPUT_CONTRACT.md (output behavior rules for conductor agents)

Skills conflate knowledge and procedure. Each adapted skill is decomposed:
- Procedural steps -> workflow definition (`.wf` file)
- Domain knowledge -> agent prompt augmentation (`.md` file)
- Input/output contracts -> workflow meta inputs + structured output expectations

- **Test**: Skill invocation and output contract validation
- **Effort**: Low (2-3 days for 4 adaptations)

---

## New Directory Structure

```
<repo>/.conductor/
  decisions/                    # NEW: Architectural decision records
    DEC-001.md
    DEC-002.md
    DEC-004.md
    DEC-017.md
    DEC-018.md
    DEC-122.md
    DEC-123.md
    DEC-124.md
  schemas/
    gates/                      # NEW: Gate definition instantiations
      confidence-gate.yaml
      readiness-gate.yaml
      quality-layers.yaml
    gate-template.yaml          # NEW: Meta-template for gate instantiation
    threshold-gate.yaml         # NEW: Threshold gate definition schema
    alignment-scoring.yaml      # NEW: Alignment scoring configuration
    performance-gate.yaml       # NEW: Performance measurement gate config
    bypass.yaml                 # NEW: Gate bypass/override mechanism
    escalation.yaml             # NEW: Workflow escalation schema
    checkpoint.yaml             # NEW: Verification checkpoint schema
    change-request.yaml         # NEW: Change request tracking schema
    decision.yaml               # NEW: Decision record schema
    bug.yaml                    # NEW: Bug tracking schema
  reference/                    # NEW: Reference documentation (not executable)
    agents/                     # 10 reference agent methodologies
    commands/                   # 7 reference command patterns
    schemas/                    # 12 reference entity schemas
    decisions/                  # 15 reference decisions

conductor-core/src/
  scoring/                      # NEW: Scoring infrastructure module
    mod.rs
    types.rs
    threshold.rs
    confidence.rs
    alignment.rs
    readiness.rs
    convergence.rs
    convergence_types.rs
    performance_gate.rs
    quality_layers.rs
    gate_template.rs
```

### Config Additions

```toml
# .conductor/config.toml additions

[command_tiers]
worktree_create = "P1"
worktree_delete = "P0"
agent_start = "P1"
workflow_run = "P0"
ticket_sync = "P2"
repo_add = "P1"

[scoring]
default_confidence_threshold = 70
default_convergence_max_iterations = 5
default_convergence_min_iterations = 2
```

---

## Composite Pattern Risk Assessment

Per CMU/SEI finding that composite pattern detection achieves F1=0.56, three patterns receive a -20 point confidence penalty:

| Pattern | Base Score | Composite Penalty | Adjusted Score | Manual Review Required |
|---------|-----------|-------------------|----------------|----------------------|
| P5 score-fix-rescore-convergence | 75 | -20 | 55 | Yes |
| P7 quality-gate-layering | 70 | -20 | 50 | Yes |
| P9 verification-gate-template-instantiation | 70 | -20 | 50 | Yes |

These patterns require manual verification by the pattern discoverer before implementation proceeds. Automated coupling analysis alone is insufficient to guarantee correct extraction of these multi-level compositions.

---

## DSL Expressiveness Gaps

| Gap | Severity | Affected Items | Mitigation |
|-----|----------|---------------|------------|
| No multi-agent conversation mode | High | 3 roundtable commands | Defer to future cycle; document as DSL extension request |
| No dynamic agent selection at runtime | Medium | Commands with conditional agent routing | Use `if` nodes to statically route to different agents |
| No first-class scoring integration in DSL | Medium | All lifecycle-gating patterns | Scoring lives in Rust code; workflow accesses via structured output fields |
| No loop-with-delta-tracking in DSL | Low | P5 convergence loop | Implement delta tracking in Rust; workflow uses `do/while` with custom convergence check agent |

---

## Test Strategy

### Lifecycle-Gating Pattern Tests

| Pattern | Test Type | Key Test Cases |
|---------|-----------|---------------|
| P1 threshold | Unit | Exact boundary values (score == threshold), catch-all tier, empty tier list, descending order validation |
| P2 confidence | Unit | Score at 70 (pass), 69 (fail), 0, 100; blocker classification |
| P3 alignment | Unit | Weight sensitivity (coverage=0.6, alignment=0.4); dual-mode thresholds |
| P4 readiness | Unit | Per-dimension minimum enforcement; high composite with one zero dimension |
| P5 convergence | Unit | Min iterations enforced; max iterations terminate; delta tracking; role separation |
| P6 performance | Integration | Before/after measurement comparison; regression threshold; remediation routing |
| P7 quality layers | Integration | Sequential gating (tier 1 fail blocks tier 2); layer composition |
| P8 command tiers | Unit | Classification lookup; conservative default (unknown -> highest tier) |
| P9 gate template | Unit | Template instantiation; schema validation of output; missing section detection |

### OS Validation Tests

| Category | Validation Method | Automation |
|----------|------------------|-----------|
| Adapted agents | YAML frontmatter parse; required keys (role); no broken template variables | Script: parse all `.conductor/agents/*.md` |
| Adapted workflows | Workflow DSL parse via `parse_workflow_str()`; agent reference resolution | Script: batch-validate all `.wf` files |
| Adapted schemas | YAML parse; required fields (entity, fields, schema_version) | Script: parse all `.conductor/schemas/*.yaml` |
| Adapted decisions | Frontmatter parse; required sections (Context, Decision, Rationale) | Script: check markdown structure |
| Protocols | Manual review; integration into CLAUDE.md | Manual verification |

### Integration Tests

1. **Threshold gate in workflow**: Test workflow with gate step invoking `evaluate_threshold()`. Verify pass/block.
2. **Confidence gate in workflow**: Test workflow with confidence-gated step advancement. Verify READY/BLOCKED.
3. **Convergence loop in workflow**: Test workflow with score-fix-rescore loop. Verify bounded termination.
4. **Adapted agent in workflow**: Run adapted agent (e.g., `verification-engineer`) in workflow. Verify valid structured output.

---

## Task Ordering

```
Phase 1 (foundation):
  W5-T01 (P1 threshold)  ─────────────────────────────┐
  W5-T08 (P8 command tiers, independent) ──────────────┤
                                                        v
Phase 2 (specializations, parallel after T01):
  W5-T02 (P2 confidence)  ────────────────────────────┐
  W5-T03 (P3 alignment)   ────────────────────────────┤
  W5-T04 (P4 readiness)   ────────────────────────────┤
                                                        v
Phase 3 (composites, after T01 + W4-T04):
  W5-T05 (P5 convergence) [COMPOSITE] ────────────────┐
  W5-T06 (P6 performance gate) ────────────────────────┤
  W5-T07 (P7 quality layers) [COMPOSITE] ─────────────┤
                                                        v
Phase 4 (meta-template, after T07):
  W5-T09 (P9 gate template) [COMPOSITE] ──────────────┤
                                                        v
Phase 5 (OS adaptation, parallelizable with Phases 1-4):
  W5-T10 (agents)          ───────────────────────────┐
  W5-T11 (commands)        ───────────────────────────┤
  W5-T12 (schemas)         ───────────────────────────┤
  W5-T13 (decisions)       ───────────────────────────┤
  W5-T14 (skills/protocols) ──────────────────────────┘
```

**Cross-wave dependency**: W5-T07 depends on W4-T04 (gated verification pipeline from Wave 4). This was corrected from an earlier version that incorrectly referenced W3-T04.

---

## Summary Statistics

| Metric | Value |
|--------|-------|
| Lifecycle-gating patterns | 9 |
| Operational structures | 111 |
| Items to adapt | 38 (34%) |
| Items for reference | 49 (44%) |
| Items deferred | 24 (22%) |
| New Rust modules | 1 (`scoring/`) with 10 files |
| New Rust LOC estimate | 2,500-3,500 |
| New `.conductor/` directories | 3 (`decisions/`, `schemas/gates/`, `reference/`) |
| New workflow definitions | 7 (`.wf` files from command adaptation) |
| New agent definitions | 8 (new, non-overlapping) |
| Merged agent definitions | 4 (methodology merged into existing) |
| New schema definitions | 6 (adapted from global-sdlc) |
| New decision records | 8 (adapted from global-sdlc) |
| DSL gaps identified | 4 (multi-agent conversation is blocking for 3 items) |
| Estimated total effort | 40-55 days (25-35 patterns, 15-20 OS adaptation) |
| Composite patterns requiring manual review | 3 (P5, P7, P9) |
| Database migrations required | 0 |
| Existing files to modify | ~5 (`lib.rs`, `config.rs`, `main.rs`, `CLAUDE.md`, `config.toml`) |
