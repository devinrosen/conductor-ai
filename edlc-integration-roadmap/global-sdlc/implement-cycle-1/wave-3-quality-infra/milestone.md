---
wave: 3
title: "Quality Infrastructure: Verification + State Consistency + Diagnostics"
status: pending
pattern_count: 7
os_count: 0
depends_on: [2]
migration: v052
estimated_effort: "4-6 weeks"
highest_risk: "evidence-based-task-verification (composite, adjusted feasibility 52)"
---

# Wave 3: Quality Infrastructure

## Patterns

| # | Pattern | Version | Domain | GRS | Strategy | Adjusted Feasibility |
|---|---------|---------|--------|-----|----------|---------------------|
| 1 | structured-evidence-directory | 1.1.0 | verification | medium | Anchored CoT | 75 |
| 2 | acceptance-criteria-driven-verification | 1.0.0 | verification | medium | Anchored CoT | 70 |
| 3 | evidence-based-task-verification | 1.0.0 | verification | medium | Anchored CoT | 52 (composite penalty -20) |
| 4 | prerequisite-verification-protocol | 1.0.0 | verification | medium | Anchored CoT | 80 |
| 5 | critical-task-escalation | 1.0.0 | verification | medium | Anchored CoT | 68 |
| 6 | desync-detection-protocol | 1.0.0 | state-consistency | medium | Anchored CoT | 78 |
| 7 | cross-agent-error-vocabulary | 1.0.0 | diagnostics | medium | Anchored CoT | 82 |

---

## Design Decisions

### Hybrid Storage: Evidence Metadata in SQLite, Bulk Artifacts on Filesystem

Verification evidence is ephemeral runtime data tied to a specific workflow run. It fits neither conductor-ai's SQLite runtime state model nor its filesystem declarative definition model cleanly. The solution is a hybrid:

| Data Category | Storage | Rationale |
|---------------|---------|-----------|
| Verification criteria (parsed from step definitions) | SQLite JSON column on `workflow_run_steps` | Queryable, co-located with step lifecycle |
| Per-criterion verdicts (pass/fail, expected/actual) | SQLite JSON column on `workflow_run_steps` | Queryable for dashboards and reports |
| Overall verification status | SQLite column on `workflow_run_steps` | Enables SQL filtering by verification state |
| Bulk evidence artifacts (test output, logs, command transcripts) | Filesystem: `~/.conductor/evidence/<run_id>/<step_name>/` | Avoids SQLite blob overhead, supports large artifacts |
| Verification report (summary) | Filesystem: `~/.conductor/evidence/<run_id>/report.md` | Human-readable, linked from SQLite |

### Evidence Directory Structure

```
~/.conductor/evidence/
  <workflow_run_id>/
    report.md
    <step_name>/
      checklist.json
      evidence/
        test_output/
        command_demos/
        error_handling/
        coverage/
      capture.sh                  # Reproducible evidence collection script (optional)
```

### ConsistencyChecker Design: Manager Pattern, Report-Only

`ConsistencyChecker` follows conductor-ai's manager pattern, taking `&Connection` + `&Config`. It detects 6 desync scenarios across 4 entity types. Initial integration is **report-only** -- no auto-fix except for confirmed-orphaned agent runs (tmux window absent), which transition to `failed` with `result_text = "Process terminated externally (desync recovery)"`.

### Error Vocabulary: `C-{XX}-{NNN}` Code Scheme

Six semantic categories, each with a two-letter prefix and three-digit sequence:

| Category | Code Prefix | Range | Examples |
|----------|-------------|-------|---------|
| Environment | `C-EN` | 001-099 | `C-EN-001` git not found, `C-EN-002` gh not found |
| Configuration | `C-CF` | 001-099 | `C-CF-001` config parse error, `C-CF-002` missing required field |
| State | `C-ST` | 001-099 | `C-ST-001` desync detected, `C-ST-002` invalid state transition |
| Execution | `C-EX` | 001-099 | `C-EX-001` agent timeout, `C-EX-002` subprocess failed |
| Permission | `C-PM` | 001-099 | `C-PM-001` git auth failed, `C-PM-002` API token expired |
| Validation | `C-VL` | 001-099 | `C-VL-001` invalid workflow DSL, `C-VL-002` schema validation failed |

Error vocabulary builds on Wave 1's `SubprocessFailure` struct and `ConductorError` enum. It classifies existing error types into semantic categories -- it does not replace them.

### Critical Task Escalation: Reuses Existing Gate Infrastructure

No new gate mechanism required. Critical task escalation maps to the existing workflow engine `Waiting` state:

1. Verification pipeline produces `PASS` verdict on a critical step
2. Step status set to `Waiting` with `gate_type = 'critical_review'`
3. `gate_prompt` populated with review comment template and evidence path
4. Workflow enters `Waiting` status via existing `set_waiting_blocked_on()` mechanism
5. Human approves/rejects via existing TUI gate approval flow
6. Approval -> `Completed`; rejection -> `Failed`

---

## Schema Migration: v052

Single migration file covering all Wave 3 columns. Waves 1-2 consume migrations v049-v051; Wave 3 starts at v052.

### New columns on `workflow_run_steps`

```sql
ALTER TABLE workflow_run_steps ADD COLUMN acceptance_criteria TEXT;
  -- JSON array: [{text, evidence_type, verdict, expected, actual}]

ALTER TABLE workflow_run_steps ADD COLUMN verification_status TEXT;
  -- NULL (not verified), 'pending', 'passed', 'failed'

ALTER TABLE workflow_run_steps ADD COLUMN evidence_path TEXT;
  -- Filesystem path to evidence directory for this step

ALTER TABLE workflow_run_steps ADD COLUMN is_critical INTEGER DEFAULT 0;
  -- Boolean flag for critical task escalation
```

### New columns on `workflow_runs`

```sql
ALTER TABLE workflow_runs ADD COLUMN verification_report_path TEXT;
  -- Filesystem path to overall verification report

ALTER TABLE workflow_runs ADD COLUMN consistency_check_result TEXT;
  -- JSON summary from last desync check (optional, for diagnostic display)
```

---

## Tasks

### W3-T01: Evidence Directory Structure
- **Pattern**: structured-evidence-directory@1.1.0
- **Feasibility**: 75
- **Effort**: Low (3-5 days)
- **Phase**: A (no dependencies, parallelizable)
- **Files to create**: `conductor-core/src/workflow/evidence.rs`
- **Files to modify**: `conductor-core/src/config.rs` (add `evidence_dir()` path helper)
- **Schema**: Migration v052 -- `evidence_path` column on `workflow_run_steps`, `verification_report_path` on `workflow_runs`
- **Implementation**:
  1. Add `evidence_dir()` to `Config` returning `~/.conductor/evidence/`
  2. Create `evidence.rs` with `create_evidence_directory()`, `cleanup_evidence()`, `list_evidence()`
  3. Wire `create_evidence_directory()` into `execute_call()` when criteria are present
- **Pattern adaptation**: Source pattern uses `.verify/` co-located with work unit; conductor adaptation uses centralized `~/.conductor/evidence/<run_id>/` because work units are SQLite records, not filesystem entities. Checklist format changed from `md` to `json` for machine-parseability. Evidence type taxonomy extended with `coverage`.
- **Key risk**: Filesystem cleanup on workflow deletion must cascade evidence directory removal
- **Tests**:
  - Evidence directory creation (unit, `tempfile::TempDir`, verify structure)
  - Evidence directory cleanup (unit, create then delete, verify removal)
  - Evidence path stored in SQLite (integration, in-memory SQLite)
  - Evidence survives workflow resume (integration, simulate crash, verify persistence)

### W3-T02: Acceptance Criteria for Workflow Steps
- **Pattern**: acceptance-criteria-driven-verification@1.0.0
- **Feasibility**: 70
- **Effort**: Medium (5-8 days)
- **Phase**: B (depends on W3-T01)
- **Files to create**: `conductor-core/src/workflow/verification.rs`
- **Files to modify**:
  - `conductor-core/src/workflow_dsl/types.rs` -- add `AcceptanceCriterion` struct, `EvidenceType` enum, `acceptance_criteria` field on `CallNode`
  - `conductor-core/src/workflow_dsl/parser.rs` -- parse `acceptance_criteria { ... }` block within call nodes
  - `conductor-core/src/workflow/executors.rs` -- gate step completion on criteria evaluation
- **Schema**: Migration v052 -- `acceptance_criteria` JSON column, `verification_status` column on `workflow_run_steps`
- **New types**:
  ```rust
  pub struct AcceptanceCriterion {
      pub text: String,
      pub evidence_type: EvidenceType,
  }

  pub enum EvidenceType {
      TestOutput, CommandDemo, ErrorHandling, Coverage, ManualInspection,
  }
  ```
- **DSL syntax**:
  ```
  call agent_name {
    prompt = "Implement the feature"
    acceptance_criteria {
      - [ ] All unit tests pass (cargo test)
      - [ ] No clippy warnings (cargo clippy -- -D warnings)
    }
  }
  ```
- **Evidence type classification**: Keyword matching on criterion text (`test pass` -> `TestOutput`, `command output` -> `CommandDemo`, `error` -> `ErrorHandling`, `coverage` -> `Coverage`, default -> `ManualInspection`)
- **Verification gate**: In `execute_call()`, after agent completion and before `update_step_status(Completed)`, evaluate criteria. If any fail, mark step `Failed` with verification failure reason.
- **Key risk**: DSL parser complexity -- adding a new block type requires careful lexer/parser extension
- **Tests**:
  - Criteria parsing from DSL (unit, parse workflow string, verify AST)
  - Evidence type classification (unit, keyword matching against known inputs)
  - Criteria evaluation all pass (unit, mock evidence directory)
  - Criteria evaluation some fail (unit, mock with missing files)
  - Step blocked on failed criteria (integration, in-memory SQLite)

### W3-T03: Evidence-Based Task Verification
- **Pattern**: evidence-based-task-verification@1.0.0
- **Feasibility**: 52 (composite penalty -20; flagged for incremental implementation)
- **Effort**: Medium (5-8 days)
- **Phase**: C (depends on W3-T01, W3-T02)
- **Files to create**: `conductor-core/src/workflow/verification_pipeline.rs` (7-phase orchestrator)
- **Files to modify**:
  - `conductor-core/src/workflow/executors.rs` -- invoke pipeline at step completion
  - `conductor-core/src/workflow/evidence.rs` -- capture script execution
- **7-phase pipeline**:
  1. Phase 0: Prerequisite check (delegates to W3-T04)
  2. Phase 1: Parse criteria from step definition
  3. Phase 2: Read project standards (from `.conductor/config.toml`)
  4. Phase 3: Create evidence directory (delegates to W3-T01)
  5. Phase 4: Generate capture script
  6. Phase 5: Execute evidence collection
  7. Phase 6: Evaluate evidence against criteria
  8. Phase 7: Generate report
- **Fast path**: When step has no `acceptance_criteria`, bypass pipeline entirely
- **IMPORTANT -- Incremental implementation required**: This is a composite pattern (adjusted feasibility 52). Do NOT implement as a single deliverable. Deliver phases 1-3 first (criteria parsing + evidence directory creation), then phases 4-6 (capture + collection), then phase 7 (reporting). Each increment must be independently testable.
- **Key risk**: Pipeline phases add latency to step completion; must have fast path for steps without criteria
- **Tests**:
  - Full pipeline pass path (integration, in-memory SQLite + tempdir)
  - Full pipeline fail path (integration, failing criteria, verify rework recommendation)
  - Pipeline bypass when no criteria (unit, verify fast path)
  - Pipeline phase failures (unit, fail each phase individually, verify error propagation)

### W3-T04: Prerequisite Verification
- **Pattern**: prerequisite-verification-protocol@1.0.0
- **Feasibility**: 80
- **Effort**: Low (2-3 days)
- **Phase**: A (no dependencies, parallelizable)
- **Files to create**: `conductor-core/src/workflow/prerequisites.rs`
- **Files to modify**:
  - `conductor-core/src/workflow_dsl/types.rs` -- add `PrerequisiteCheck` enum: `FileExists(path)`, `CommandAvailable(command)`, `StepCompleted(step_name)`
  - `conductor-core/src/workflow_dsl/parser.rs` -- parse `requires { ... }` block
  - `conductor-core/src/workflow/engine.rs` -- add Phase 0 before step execution
- **DSL syntax**:
  ```
  call agent_name {
    prompt = "Implement feature"
    requires {
      file ".conductor/config.toml"
      file "Cargo.toml"
      command "cargo --version"
    }
  }
  ```
- **Error mapping**: Prerequisite failures map to error vocabulary codes (`C-EN-*` for missing files/commands)
- **Key risk**: False positives from overly strict prerequisite checks blocking legitimate workflow execution
- **Tests**:
  - File prerequisite exists (unit, create temp file, verify passes)
  - File prerequisite missing (unit, verify fails with actionable error)
  - Command prerequisite available (unit, check for `git`, verify passes)
  - Command prerequisite missing (unit, nonexistent command, verify fails)
  - All prerequisites batch (unit, mix passing and failing, verify all reported)

### W3-T05: Critical Task Escalation
- **Pattern**: critical-task-escalation@1.0.0
- **Feasibility**: 68
- **Effort**: Medium (4-6 days)
- **Phase**: D (depends on W3-T03, Wave 2 TUI infrastructure)
- **Files to create**: `conductor-core/src/workflow/escalation.rs`
- **Files to modify**:
  - `conductor-core/src/workflow/verification_pipeline.rs` -- compose escalation at verdict boundary
  - `conductor-tui/src/app/` -- escalation notification modal (reuses existing gate approval UI)
- **Schema**: Migration v052 -- `is_critical` column on `workflow_run_steps` (or use existing `markers_out` JSON)
- **Escalation flow** (reuses existing gate infrastructure):
  1. Verification pipeline produces PASS on a critical step
  2. Set step to `Waiting` with `gate_type = 'critical_review'`
  3. Populate `gate_prompt` with review template + evidence path
  4. Workflow enters `Waiting` via `set_waiting_blocked_on()`
  5. Human approves/rejects via existing TUI gate flow
  6. Approval -> `Completed`; rejection -> `Failed`
- **Key risk**: TUI integration complexity -- must add escalation-specific content to existing gate modal
- **Tests**:
  - Non-critical pass, no escalation (unit, step goes to Completed)
  - Critical pass, escalation fires (integration, step transitions to Waiting with `gate_type = 'critical_review'`)
  - Critical fail, no escalation (unit, step goes to Failed -- only passes escalate)
  - Escalation approval (integration, simulate gate approval, verify Completed)
  - Escalation rejection (integration, simulate gate rejection, verify Failed)

### W3-T06: Desync Detection Protocol
- **Pattern**: desync-detection-protocol@1.0.0
- **Feasibility**: 78
- **Effort**: Medium (5-7 days)
- **Phase**: A (no dependencies, parallelizable)
- **Files to create**: `conductor-core/src/consistency.rs` (~300 lines)
- **Files to modify**:
  - `conductor-core/src/lib.rs` -- add `pub mod consistency`
  - `conductor-cli/src/main.rs` -- add `conductor check` subcommand
  - `conductor-tui/src/app/` -- startup check + status bar indicator
- **ConsistencyChecker struct** (follows Manager pattern):
  ```rust
  pub struct ConsistencyChecker<'a> {
      conn: &'a Connection,
      config: &'a Config,
  }
  ```
  Methods: `check_all()`, `check_worktrees()`, `check_agent_runs()`, `check_workflow_runs()`, `check_repos()`, `check_evidence()`
- **6 desync scenarios**:

  | Entity | Declared (SQLite) | Observed (Reality) | Flag |
  |--------|------------------|-------------------|------|
  | Worktree | `status = 'active'` | Directory missing | `DESYNC_PHANTOM` |
  | Worktree | `status = 'active'` | Branch deleted externally | `DESYNC_PHANTOM` |
  | Agent Run | `status = 'running'` | Tmux window absent | `DESYNC_AHEAD` |
  | Agent Run | `status = 'running'` | Tmux window exists, process exited | `DESYNC_AHEAD` |
  | Workflow Run | `status = 'running'` | Parent agent not running | `DESYNC_AHEAD` |
  | Repo | `local_path` set | Directory missing or not a git repo | `DESYNC_PHANTOM` |

- **DesyncFlag enum**: `Ok`, `DesyncAhead`, `DesyncBehind`, `DesyncPhantom`, `DesyncPartial`, `DesyncStale`
- **When to run**:
  - Application startup (CLI/TUI/web): `check_all()`
  - Before workflow resume: `check_workflow_runs()` + `check_agent_runs()`
  - After workflow failure: `check_workflow_runs()` for the failed run
  - Manual: `conductor check` CLI command
  - TUI status bar: `check_agent_runs()` periodically (lightweight)
- **TUI threading**: Must use background thread per CLAUDE.md TUI threading rule:
  ```rust
  std::thread::spawn(move || {
      let db = open_database(&db_path()).unwrap();
      let config = Config::load().unwrap();
      let checker = ConsistencyChecker::new(&db, &config);
      let reports = checker.check_all().unwrap_or_default();
      let _ = tx.send(Action::ConsistencyCheckComplete { reports });
  });
  ```
- **Remediation**: Report-only initially. Exception: orphaned agent runs (tmux absent) auto-transition to `failed`.
- **Key risk**: Tmux subprocess calls on TUI startup add latency; mitigated by background thread
- **Tests**:
  - Clean state, no desyncs (integration, consistent SQLite + filesystem, verify empty report)
  - Phantom worktree (integration, record pointing to nonexistent dir, verify `DESYNC_PHANTOM`)
  - Orphaned agent run (integration, running record with no tmux window, verify `DESYNC_AHEAD`)
  - Stale workflow run (integration, running workflow whose parent agent completed, verify `DESYNC_AHEAD`)
  - Multiple desyncs (integration, several desyncs, verify all detected in one pass)
  - **Note**: Tmux-dependent tests gated behind `#[cfg(not(ci))]` or use mock `TmuxChecker` trait

### W3-T07: Cross-Agent Error Vocabulary
- **Pattern**: cross-agent-error-vocabulary@1.0.0
- **Feasibility**: 82
- **Effort**: Low (2-4 days)
- **Phase**: A (no dependencies, parallelizable)
- **Files to create**: `conductor-core/src/error_vocabulary.rs` (~150 lines)
- **Files to modify**:
  - `conductor-core/src/error.rs` -- add `classify()` method or `ErrorCategory` integration
  - `conductor-core/src/agent/context.rs` -- inject vocabulary into agent startup context
  - `conductor-core/src/workflow/executors.rs` -- classify step errors with category
- **ErrorCategory enum**: `Environment`, `Configuration`, `State`, `Execution`, `Permission`, `Validation`
- **Classification function**: `ErrorCategory::classify(&ConductorError) -> Self` with subprocess sub-classifier examining exit codes and stderr content
- **Wave 1 alignment**: Maps `ConductorError` variants to categories:

  | `ConductorError` Variant | Category | Code |
  |--------------------------|----------|------|
  | `Git(SubprocessFailure)` exit 128 + auth | `Permission` | `C-PM-001` |
  | `Git(SubprocessFailure)` exit 1 | `Execution` | `C-EX-003` |
  | `Database(_)` | `State` | `C-ST-003` |
  | `Config(_)` | `Configuration` | `C-CF-001` |
  | `Io(_)` | `Environment` | `C-EN-004` |
  | `Workflow(_)` | `Execution` | `C-EX-004` |
  | `InvalidInput(_)` | `Validation` | `C-VL-003` |
  | `Schema(_)` | `Validation` | `C-VL-002` |

- **Propagation layers**:
  1. Canonical definition: Rust enum in `error_vocabulary.rs`
  2. Agent context injection: JSON vocabulary in agent startup context
  3. Workflow step metadata: `error_category` in step `result_text` JSON
  4. CLI/TUI display: Human-readable messages with category prefix
- **Key risk**: Low -- primarily a classification layer on existing error types
- **Tests**:
  - Classification coverage (unit, every `ConductorError` variant maps to an `ErrorCategory`)
  - Subprocess classification (unit, exit code + stderr combinations)
  - Error code format (unit, verify all codes match `C-{XX}-{NNN}` regex)
  - Round-trip serialization (unit, serialize/deserialize `ErrorCategory`, verify identity)

---

## Implementation Phases

The dependency graph within Wave 3 dictates strict ordering:

```
Phase A (parallel, no dependencies):
  W3-T01: structured-evidence-directory       [3-5 days]
  W3-T04: prerequisite-verification-protocol   [2-3 days]
  W3-T06: desync-detection-protocol            [5-7 days]
  W3-T07: cross-agent-error-vocabulary         [2-4 days]

Phase B (depends on Phase A):
  W3-T02: acceptance-criteria-driven-verification  [5-8 days]
    depends on: W3-T01

Phase C (depends on Phase B):
  W3-T03: evidence-based-task-verification  [5-8 days]
    depends on: W3-T01, W3-T02
    NOTE: Incremental delivery required (composite pattern, feasibility 52)

Phase D (depends on Phase C + Wave 2):
  W3-T05: critical-task-escalation  [4-6 days]
    depends on: W3-T03, Wave 2 TUI infrastructure
```

Phase A is fully parallelizable across developers. Total critical path: Phase A (5-7 days) -> Phase B (5-8 days) -> Phase C (5-8 days) -> Phase D (4-6 days) = **19-29 days**.

---

## Test Infrastructure Additions

1. **`test_helpers.rs` extension**: Add `setup_evidence_dir()` helper creating temporary evidence directory structure
2. **Mock tmux interface**: `TmuxChecker` trait with real implementation (subprocess) and mock implementation (configurable returns) for desync detection tests
3. **Verification test fixtures**: Workflow definitions with acceptance criteria in `conductor-core/tests/fixtures/`

---

## New CLI Commands

| Command | Purpose |
|---------|---------|
| `conductor check` | Run all consistency checks, display desync report |
| `conductor evidence prune` | Remove evidence directories for completed/old workflow runs per retention policy |

## New TUI Features

| Feature | Location | Threading |
|---------|----------|-----------|
| Startup consistency check | Status bar / modal | Background thread (required by CLAUDE.md TUI threading rule) |
| Critical task escalation | Gate approval modal (reuses existing) | Main thread (UI-only, no blocking) |

---

## Risk Matrix

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| DSL parser complexity (new `acceptance_criteria` + `requires` blocks) | Medium | High | Extensive parser tests; keep syntax similar to existing blocks |
| Evidence directory bloat | Medium | Medium | Retention policy from day 1; `conductor evidence prune` command |
| Desync detection false positives (timing windows) | Medium | Medium | 5s grace period; require two consecutive checks to confirm |
| TUI startup latency from consistency check | Low | Medium | Background thread; show result asynchronously |
| Wave 1 dependency (error vocabulary needs `SubprocessFailure`) | High | High | Fallback: temporary `classify_from_string()` for current `String` errors |
| Verification pipeline latency (7-phase) | Medium | Medium | Fast-path bypass for steps without criteria |
| Schema migration sequencing conflict | Low | High | Single migration file (v052) for all Wave 3 columns |

---

## Summary Statistics

| Metric | Value |
|--------|-------|
| Total patterns | 7 |
| New files to create | 7 (`evidence.rs`, `verification.rs`, `verification_pipeline.rs`, `prerequisites.rs`, `escalation.rs`, `consistency.rs`, `error_vocabulary.rs`) |
| Files to modify | ~10 (`engine.rs`, `executors.rs`, `types.rs`, `parser.rs`, `config.rs`, `error.rs`, `context.rs`, `lib.rs`, CLI, TUI) |
| Schema migrations | 1 (v052, multi-column) |
| New CLI commands | 2 (`conductor check`, `conductor evidence prune`) |
| New TUI features | 2 (startup consistency check, critical task escalation modal) |
| Phase A tasks (parallelizable) | 4 (W3-T01, W3-T04, W3-T06, W3-T07) |
| Critical path duration | 19-29 days |
| Estimated total effort | 4-6 weeks |
| Clean extraction points | 4 (T01, T04, T06, T07) |
| Moderate extraction points | 2 (T02, T05) |
| Difficult extraction points | 1 (T03 -- composite, incremental delivery required) |
| Highest feasibility | cross-agent-error-vocabulary (82) |
| Lowest feasibility | evidence-based-task-verification (52, composite penalty) |
