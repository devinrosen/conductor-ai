---
wave: 1
title: "Foundation: Error Handling + Core Lifecycle-Gating"
status: pending
pattern_count: 7
os_count: 0
depends_on: []
migration: v049
minimum_viable_subset: ["W1-T00", "W1-T01", "W1-T04", "W1-T06"]
estimated_effort: "4-6 weeks (with parallelization)"
critical_path: "W1-T00 (3-5 days) -> parallel error-handling + lifecycle-gating tracks"
---

# Wave 1: Foundation

## Executive Summary

Wave 1 lays the structural groundwork for all subsequent pattern integration by hardening error handling and formalizing the workflow engine's implicit state machine. It contains 7 patterns across two parallel tracks -- error-handling (4 patterns) and lifecycle-gating (3 patterns) -- plus one critical prerequisite refactor (W1-T00) that gates the entire error-handling track.

The **minimum viable subset** is W1-T00 + W1-T01 + W1-T04 + W1-T06 (SubprocessFailure refactor, semantic exit codes, process escape hatch, FSM state specification). These four changes address the most pressing user pain points with the smallest implementation surface and can be completed in 1.5-2 weeks.

## Patterns

| # | Pattern | Version | Domain | Track | Strategy |
|---|---------|---------|--------|-------|----------|
| 0 | (prerequisite: SubprocessFailure refactor) | -- | error-handling | Error-Handling | -- |
| 1 | semantic-exit-code-convention | 1.0.0 | error-handling | Error-Handling | Anchored CoT |
| 2 | bounded-retry-with-escalation | 1.0.0 | error-handling | Error-Handling | Direct Prompting |
| 3 | process-escape-hatch | 1.0.0 | error-handling | Error-Handling | Anchored CoT |
| 4 | emergency-recovery-protocol | 1.0.0 | error-handling | Error-Handling | Anchored CoT |
| 5 | fsm-state-specification-template | 1.0.0 | lifecycle-gating | Lifecycle-Gating | Anchored CoT |
| 6 | checkpoint-persistence-protocol | 1.2.0 | lifecycle-gating | Lifecycle-Gating | Anchored CoT |
| 7 | verification-gated-commit-protocol | 1.1.0 | lifecycle-gating | Lifecycle-Gating | Anchored CoT |

## Parallel Tracks

Wave 1 decomposes into two independent tracks that can proceed in parallel after W1-T00 completes.

### Track A: Error-Handling (W1-T00 through W1-T05)

```
W1-T00 (SubprocessFailure refactor) [PREREQUISITE - GATES TRACK A]
    |
    +--- W1-T01 (semantic exit codes)
    |
    +--- W1-T02 (bounded retry module) ---> W1-T03 (apply retry to call sites)
    |
    +--- W1-T04 (process escape hatch)
    |
    +--- W1-T05 (emergency recovery) [benefits from T01+T02+T04 but no hard dep]
```

### Track B: Lifecycle-Gating (W1-T06 through W1-T08)

```
W1-T06 (FSM state specification)
    |
    +--- W1-T07 (checkpoint persistence)
    |
    +--- W1-T08 (verification-gated commits)
```

Track B has **no dependency** on Track A or on W1-T00. W1-T06 can begin immediately.

## DB Migration

**Migration v049**: Combined migration for Wave 1.

```sql
-- v049: Wave 1 foundation (SubprocessFailure struct + FSM transition metadata)
-- Part 1: checkpoint version tracking for checkpoint-persistence-protocol
ALTER TABLE workflow_runs ADD COLUMN checkpoint_version INTEGER DEFAULT NULL;
```

This is the only schema change in Wave 1. It is optional for initial integration (the checkpoint module works without it) but should be included to avoid a separate migration later.

---

## Tasks

### W1-T00: SubprocessFailure Refactor (Prerequisite)

**Purpose**: Every error-handling pattern depends on the ability to programmatically inspect subprocess outcomes. The current `run_command` in `git.rs` discards exit codes and flattens stderr into an opaque `String`. This must be fixed before any error-handling pattern can be fully integrated.

**Status**: prerequisite (must be completed and merged before W1-T01, W1-T02, W1-T04, or W1-T05 can start)

**Estimated effort**: Medium (3-5 days)

#### Files to create

None.

#### Files to modify

| File | Change | Location |
|------|--------|----------|
| `conductor-core/src/error.rs` | Add `SubprocessFailure` struct with `command`, `exit_code`, `stderr`, `stdout` fields. Add `Display` impl. Add `SubprocessFailure::from_message()` convenience constructor. | Before `ConductorError` enum definition |
| `conductor-core/src/error.rs` | Change `ConductorError::Git(String)` to `Git(SubprocessFailure)` and `GhCli(String)` to `GhCli(SubprocessFailure)` | Enum variant definitions |
| `conductor-core/src/git.rs` | Rewrite `run_command` to accept `fn(SubprocessFailure) -> ConductorError` and construct `SubprocessFailure` with exit code, stderr, stdout | Lines 23-43 |
| `conductor-core/src/git.rs` | Update `local_branch_exists` inline error construction | Lines 53-70 |
| `conductor-core/src/git.rs` (tests) | Update ~5 pattern matches on `ConductorError::Git(msg)` | Test module |
| `conductor-core/src/worktree.rs` | Update ~8 inline `ConductorError::Git(format!(...))` constructions to use `SubprocessFailure::from_message()` | Various |
| `conductor-core/src/feature.rs` | Update ~12 inline Command calls with Git/GhCli error construction | Various |
| `conductor-core/src/github.rs` | Update `run_gh_with_token` error construction (~3 sites) | Line 25 area |
| `conductor-core/src/workflow_ephemeral.rs` | Update ~3 inline error constructions | Various |
| `conductor-core/src/workflow/executors.rs` | Update ~5 inline git/gh error constructions | Various |

#### New types

```rust
#[derive(Debug, Clone)]
pub struct SubprocessFailure {
    pub command: String,
    pub exit_code: Option<i32>,
    pub stderr: String,
    pub stdout: String,
}

impl SubprocessFailure {
    pub fn from_message(command: &str, message: String) -> Self { /* ... */ }
}
```

#### Config changes

None.

#### Test cases

- Verify `SubprocessFailure::from_message` creates a well-formed instance with `exit_code: None`.
- Verify `Display` impl shows stderr when present, falls back to exit code when empty.
- Verify all existing tests still pass after mechanical `ConductorError::Git(String)` -> `Git(SubprocessFailure)` migration.

#### Backward compatibility

Internal API change only. Binary crates use `ConductorError` via `Display` and `anyhow` wrapping. Since `SubprocessFailure` implements `Display`, no binary crate changes are needed (confirmed: no binary crate pattern-matches on `Git(msg)` directly).

---

### W1-T01: Semantic Exit Codes for ConductorError

**Pattern**: semantic-exit-code-convention@1.0.0
**Depends**: W1-T00 (uses SubprocessFailure for exit code extraction)
**Estimated effort**: Small (1-2 days)
**Minimum viable subset**: YES

#### Files to create

None.

#### Files to modify

| File | Change | Location |
|------|--------|----------|
| `conductor-core/src/error.rs` | Add exit code range constants and `exit_code(&self) -> i32` method on `ConductorError` | New `impl ConductorError` block after line 73 |
| `conductor-cli/src/main.rs` | Replace `std::process::exit(1)` with `std::process::exit(err.exit_code())` using `downcast_ref::<ConductorError>()` | 9 call sites (lines 634, 1538, 1595, 1650, 1705, 1763, 1976, 2015, 2020) |

#### New types

```rust
impl ConductorError {
    /// Semantic exit code. Ranges:
    ///   0      = success
    ///   1      = unspecified / anyhow fallthrough
    ///   10-19  = infrastructure (DB, I/O)
    ///   20-29  = user input errors
    ///   30-39  = subprocess / external tool failures
    ///   40-49  = configuration errors
    ///   50-59  = agent subsystem
    ///   60-69  = workflow subsystem
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Database(_) => 10,
            Self::Io(_) => 11,
            Self::RepoNotFound { .. } => 20,
            Self::RepoAlreadyExists { .. } => 21,
            Self::WorktreeNotFound { .. } => 22,
            Self::WorktreeAlreadyExists { .. } => 23,
            Self::IssueSourceAlreadyExists { .. } => 24,
            Self::TicketNotFound { .. } => 25,
            Self::TicketAlreadyLinked => 26,
            Self::InvalidInput(_) => 27,
            Self::FeatureNotFound { .. } => 28,
            Self::FeatureAlreadyExists { .. } => 29,
            Self::Git(_) => 30,
            Self::GhCli(_) => 31,
            Self::TicketSync(_) => 32,
            Self::Config(_) => 40,
            Self::AgentConfig(_) => 41,
            Self::Schema(_) => 42,
            Self::Agent(_) => 50,
            Self::FeedbackNotPending { .. } => 51,
            Self::Workflow(_) => 60,
            Self::WorkflowRunAlreadyActive { .. } => 61,
        }
    }
}
```

#### CLI dispatcher change

```rust
// Current pattern (all 9 sites):
if let Err(e) = result {
    eprintln!("Error: {e}");
    std::process::exit(1);
}

// Updated:
if let Err(e) = result {
    eprintln!("Error: {e}");
    let code = e.downcast_ref::<ConductorError>()
        .map(|ce| ce.exit_code())
        .unwrap_or(1);
    std::process::exit(code);
}
```

#### Config changes

None.

#### Test cases

- Unit test: each `ConductorError` variant maps to a unique exit code within its documented range.
- Property test: no two variants share the same code.
- CLI integration test (optional, via `assert_cmd`): a known error condition produces the expected exit code.

---

### W1-T02: Bounded Retry Module

**Pattern**: bounded-retry-with-escalation@1.0.0
**Depends**: W1-T00 (SubprocessFailure needed for transient error classification)
**Estimated effort**: Medium (3-4 days for module, T03 adds the application)

#### Files to create

| File | Purpose |
|------|---------|
| `conductor-core/src/retry.rs` | Generic retry executor with configurable bounds, backoff, escalation, and transient error classifier |

#### Files to modify

| File | Change | Location |
|------|--------|----------|
| `conductor-core/src/lib.rs` | Add `pub mod retry;` | Module declaration block (lines 29-55) |

#### New types

```rust
pub struct RetryConfig {
    pub max_attempts: u32,           // default: 3
    pub initial_backoff: Duration,   // default: 1s
    pub backoff_multiplier: f64,     // default: 2.0
    pub max_backoff: Duration,       // default: 30s
}

pub enum RetryOutcome<T, E> {
    Success { value: T, attempts: u32 },
    Exhausted { last_error: E, attempts: u32 },
}

pub fn retry_with_backoff<T, E, F, R>(config, operation, is_retriable) -> RetryOutcome<T, E>
pub fn is_transient(failure: &SubprocessFailure) -> bool
```

The `is_transient` classifier checks for network patterns: "could not resolve host", "connection refused", "timed out", "ssl", "rate limit", "429", "503", "SQLITE_BUSY". Unknown failures are treated as permanent (safe default).

#### Config changes

Optional for Wave 1 (defaults are sufficient):

```toml
[retry]
max_attempts = 3
initial_backoff_secs = 1
backoff_multiplier = 2.0
max_backoff_secs = 30
```

#### Test cases

- `retry_with_backoff` with always-succeed: returns on first attempt.
- `retry_with_backoff` with always-fail (transient): exhausts `max_attempts`, returns `Exhausted`.
- `retry_with_backoff` with fail-then-succeed: retries correctly, returns `Success` with correct attempt count.
- `retry_with_backoff` with permanent error: returns immediately without retrying (attempt count = 1).
- Backoff timing: use `std::time::Instant` to verify intervals are approximately correct.
- `is_transient` unit tests: known transient messages return true, unknown messages return false.

---

### W1-T03: Apply Retry to Command Call Sites

**Pattern**: bounded-retry-with-escalation@1.0.0
**Depends**: W1-T02
**Estimated effort**: Medium (3-4 days)

#### Files to create

None.

#### Files to modify

| File | Change | Location |
|------|--------|----------|
| `conductor-core/src/git.rs` | Add `check_output_with_retry` and `check_gh_output_with_retry` wrappers | After existing functions |
| `conductor-core/src/worktree.rs` | Wrap 5 retry-eligible call sites (`git push`, `git clone`, `gh pr view`, `gh api`, `git fetch`) | Lines 595, 958, 996, 1019, 1034 |
| `conductor-core/src/git.rs` | Wrap `is_branch_merged_remote` fetch call | Lines 92-96 |
| `conductor-core/src/workflow_ephemeral.rs` | Wrap `gh repo clone` and `gh pr checkout` | Lines 124, 139 |
| `conductor-core/src/github.rs` | Wrap `run_gh_with_token` calls | Line 25 area |
| `conductor-core/src/jira_acli.rs` | Wrap ACLI calls | Lines 9, 49 |

#### Call sites: 10 "Yes" candidates

| # | File | Operation | Line |
|---|------|-----------|------|
| 1 | `worktree.rs` | `git push` | 595 |
| 2 | `worktree.rs` | `git clone` | 958 |
| 3 | `worktree.rs` | `gh pr view` (JSON) | 996 |
| 4 | `worktree.rs` | `gh api repos/...` | 1019 |
| 5 | `worktree.rs` | `git fetch` | 1034 |
| 6 | `git.rs` | `is_branch_merged_remote` (fetch) | 92-96 |
| 7 | `workflow_ephemeral.rs` | `gh repo clone` | 124 |
| 8 | `workflow_ephemeral.rs` | `gh pr checkout` | 139 |
| 9 | `github.rs` | Various `gh` API calls | 25+ |
| 10 | `jira_acli.rs` | `acli jira` (search) | 9, 49 |

#### Deferred "Maybe" sites (non-idempotent, skip in Wave 1)

| File | Operation | Risk |
|------|-----------|------|
| `worktree.rs:624` | `gh pr create` | Non-idempotent: duplicate PR creation |
| `worktree.rs:1066` | Package manager install | May leave partial `node_modules` |
| `feature.rs:479` | `gh pr create --fill` | Non-idempotent |
| `workflow/executors.rs:2083` | Script execution | Script may be non-idempotent; already has own retry |

#### Retry wrapper pattern

The `cmd_builder` closure pattern is necessary because `Command` is consumed on `.output()` and cannot be reused:

```rust
pub(crate) fn check_output_with_retry(
    cmd_builder: impl Fn() -> Command,
    retry_config: &RetryConfig,
) -> Result<std::process::Output>
```

#### Test cases

- Integration test with counter-file mock subprocess: fail N times then succeed, verify retry recovers.
- Integration test verifying permanent errors are NOT retried.
- Do NOT test against real network calls.

#### Shared test infrastructure

New fixture: `conductor-core/tests/fixtures/mock_subprocess.sh` (counter-file mock that fails until attempt N).

---

### W1-T04: Process Escape Hatch Flags

**Pattern**: process-escape-hatch@1.0.0
**Depends**: W1-T00 (for SubprocessFailure in error identification)
**Estimated effort**: Medium (3-5 days)
**Minimum viable subset**: YES

#### Files to create

| File | Purpose |
|------|---------|
| `conductor-core/src/escape_hatch.rs` | `OverrideRecord`, `OverrideTier` types, `log_override()` tracing function |

#### Files to modify

| File | Change | Location |
|------|--------|----------|
| `conductor-core/src/lib.rs` | Add `pub mod escape_hatch;` | Module declarations |
| `conductor-cli/src/main.rs` | Add `--force` flag to `worktree create`, `workflow run` subcommands | Clap command definitions |
| `conductor-core/src/worktree.rs` | Accept `force: bool` parameter in `create()` | Lines 116-263 (WorktreeAlreadyExists guard at 146-150) |
| `conductor-core/src/worktree.rs` | Accept `force: bool` parameter in `push()` | Lines 594-604 |
| `conductor-core/src/workflow/engine.rs` | Accept `force: bool` parameter in `execute_workflow()` | Line 176 (WorkflowRunAlreadyActive guard at 268-278) |

#### New types

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct OverrideRecord {
    pub timestamp: String,
    pub operation: String,
    pub constraint_bypassed: String,
    pub justification: String,
    pub tier: OverrideTier,
}

#[derive(Debug, Clone, serde::Serialize)]
pub enum OverrideTier {
    Low,   // Self-service: --force flag
    High,  // Requires explicit confirmation or audit
}

pub fn log_override(record: &OverrideRecord) { /* tracing::warn!(...) */ }
```

#### Highest-value escape hatches (implement these 3)

| # | Guard | CLI Flag | Core API Change |
|---|-------|----------|----------------|
| 1 | `WorkflowRunAlreadyActive` (engine.rs:270-278) | `conductor workflow run --force <name>` | Cancel existing run, then start new |
| 2 | `WorktreeAlreadyExists` (worktree.rs:146-150) | `conductor worktree create --force <repo> <name>` | Delete existing + recreate |
| 3 | `ensure_base_up_to_date` (worktree.rs:183) | `conductor worktree create --offline <repo> <name>` | Skip fetch, use local state |

#### Config changes

None initially. Future: `[escape_hatch]` section for tier thresholds.

#### Test cases

- `--force` on `worktree create` with existing slug: succeeds (deletes + recreates).
- `--force` on `workflow run` with active run: cancels existing, starts new.
- `OverrideRecord` is emitted via tracing (capture tracing events in tests).
- Without `--force`: guards still reject as before (regression test).

#### Shared test infrastructure

New: `conductor-core/tests/helpers/tracing_capture.rs` (tracing test subscriber for audit log verification).

---

### W1-T05: Emergency Recovery Protocol

**Pattern**: emergency-recovery-protocol@1.0.0
**Depends**: No hard dependencies, but benefits from W1-T00, W1-T01, W1-T02, W1-T04
**Estimated effort**: Large (1-2 weeks)

#### Files to create

| File | Purpose |
|------|---------|
| `conductor-core/src/recovery.rs` | `RecoveryTier` enum, `RecoveryResult` struct, `recover_worktree()`, `recover_workflows()` |

#### Files to modify

| File | Change | Location |
|------|--------|----------|
| `conductor-core/src/lib.rs` | Add `pub mod recovery;` | Module declarations |
| `conductor-core/src/worktree.rs` | Add `find_orphaned_worktrees()` and `repair_worktree()` methods to `WorktreeManager` | After existing methods |
| `conductor-core/src/workflow/manager.rs` | Add `cancel_stale_runs(stale_threshold: Duration)` method | After `cancel_run()` |
| `conductor-cli/src/main.rs` | Add `conductor worktree repair <repo>` subcommand | Clap command definitions |
| `conductor-cli/src/main.rs` | Add `conductor workflow recover` subcommand | Clap command definitions |

#### New types

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RecoveryTier {
    TargetedFix,       // Tier 1: auto-detect + repair specific issue
    ComponentRepair,   // Tier 2: reconcile DB and filesystem state
    StateCleanup,      // Tier 3: remove stale locks, orphaned records
    NuclearOption,     // Tier 4: backup + rebuild from scratch
}

#[derive(Debug)]
pub struct RecoveryResult {
    pub tier_used: RecoveryTier,
    pub description: String,
    pub success: bool,
}
```

#### Implementation priority (Wave 1 scope)

Focus on **Stuck State #1** (partially created worktree) and **Stuck State #3** (stuck workflow run):

| Stuck State | Detection | Recovery |
|-------------|-----------|----------|
| #1: Partially created worktree | Scan `~/.conductor/workspaces/<repo>/` for dirs with no DB record | `repair_worktree(adopt=true)` or `repair_worktree(adopt=false)` (cleanup) |
| #3: Stuck workflow run | Query `workflow_runs` where status=Running/Waiting and `updated_at` > N hours ago, cross-check tmux windows | `cancel_stale_runs()` marks as cancelled |

Other stuck states (#2 orphaned agent, #4 partial delete, #5 DB lock, #6 gate waiting) are deferred to future iterations.

#### Config changes

Optional:

```toml
[recovery]
stale_workflow_threshold_hours = 4
auto_reap_on_startup = false
```

#### Test cases

- Create a worktree dir on disk without DB record (simulate crash after `git worktree add` but before INSERT). Verify `find_orphaned_worktrees` detects it. Verify `repair_worktree(adopt=false)` cleans it up.
- Insert a `workflow_runs` record with `status=running` and old `updated_at`. Verify `cancel_stale_runs` marks it cancelled.
- Verify graduated ladder: Tier 1 attempted before Tier 2.

---

### W1-T06: FSM State Specification

**Pattern**: fsm-state-specification-template@1.0.0
**Depends**: None (independent track, can start immediately)
**Estimated effort**: Medium (1 week)
**Minimum viable subset**: YES

#### Files to create

| File | Purpose |
|------|---------|
| `conductor-core/src/workflow/transitions.rs` | Central transition tables: `is_valid_run_transition()`, `is_valid_step_transition()`, `RunTransition` struct |

#### Files to modify

| File | Change | Location |
|------|--------|----------|
| `conductor-core/src/workflow/mod.rs` | Add `pub(crate) mod transitions;` | Module declarations |
| `conductor-core/src/workflow/status.rs` | Add `is_terminal()`, `is_active()` methods to `WorkflowRunStatus` and `WorkflowStepStatus` | After existing impls |
| `conductor-core/src/workflow/manager.rs` | Add transition validation (warn-only guard) in `update_workflow_status` and `update_step_status_full` | Lines 200-204 area |
| `conductor-core/src/workflow/engine.rs` | Route status updates through transition dispatcher (~5 call sites) | Various |
| `conductor-core/src/workflow/executors.rs` | (Wave 1: warn-only guard, NOT full migration of ~50 sites) | Various |

#### New types

```rust
#[derive(Debug, Clone)]
pub struct RunTransition {
    pub from: WorkflowRunStatus,
    pub to: WorkflowRunStatus,
    pub reason: String,
}

pub fn is_valid_run_transition(from: &WorkflowRunStatus, to: &WorkflowRunStatus) -> bool
pub fn is_valid_step_transition(from: &WorkflowStepStatus, to: &WorkflowStepStatus) -> bool
```

#### Transition tables

**WorkflowRunStatus transitions** (6 states: Pending, Running, Completed, Failed, Waiting, Cancelled):

| From | To | Reason |
|------|----|--------|
| Pending | Running | Normal start |
| Running | Completed | All steps finished |
| Running | Failed | Step failure |
| Running | Waiting | Gate encountered |
| Waiting | Running | Gate resolved |
| Failed | Running | Resume |
| Pending | Cancelled | User cancel |
| Running | Cancelled | User cancel |
| Waiting | Cancelled | User cancel |
| Completed | Running | Restart |

**WorkflowStepStatus transitions** (7 states: Pending, Running, Completed, Failed, Waiting, Skipped, TimedOut):

| From | To | Reason |
|------|----|--------|
| Pending | Running | Step starts |
| Pending | Skipped | Condition skip |
| Pending | Completed | Quality gate direct eval |
| Pending | Failed | Quality gate direct fail |
| Running | Completed | Step succeeds |
| Running | Failed | Step fails |
| Running | Waiting | External gate |
| Running | TimedOut | Timeout |
| Running | Skipped | Dry-run skip |
| Waiting | Completed | Gate resolved (pass) |
| Waiting | Failed | Gate resolved (fail) |
| Failed | Pending | Resume reset |
| TimedOut | Pending | Resume reset |
| Waiting | Pending | Resume reset |
| Completed | Pending | Restart reset |

#### Gradual migration strategy (IMPORTANT)

The ~50 existing transition sites in `executors.rs` will NOT be refactored in Wave 1. Instead:

1. Add transition validation to `WorkflowManager::update_workflow_status()` and `update_step_status_full()` as a **warn-only guard** (log invalid transitions via `tracing::warn!` but do NOT reject them).
2. Run full test suite with warn-only mode to identify unexpected transition paths.
3. Once all unexpected paths are categorized (legitimate vs. bugs), promote the guard to **reject mode** in a follow-up PR.

This staged approach prevents regressions while building confidence in the transition table.

#### Config changes

None.

#### Test cases

- Exhaustive unit tests for `is_valid_run_transition`: all 36 state pairs (6x6), assert each as valid or invalid.
- Exhaustive unit tests for `is_valid_step_transition`: all 49 state pairs (7x7).
- Integration test: run full workflow test suite with warn-only guards. Verify zero warnings (or categorize any that appear).
- Regression test: Completed -> Pending without restart flag is rejected.

---

### W1-T07: Checkpoint Persistence

**Pattern**: checkpoint-persistence-protocol@1.2.0
**Depends**: W1-T06 (writes checkpoints at FSM transition points)
**Estimated effort**: Medium (3-5 days)

#### Files to create

| File | Purpose |
|------|---------|
| `conductor-core/src/workflow/checkpoint.rs` | Checkpoint file write/read/validate logic, checkpoint JSON schema |

#### Files to modify

| File | Change | Location |
|------|--------|----------|
| `conductor-core/src/workflow/mod.rs` | Add `pub(crate) mod checkpoint;` | Module declarations |
| `conductor-core/src/workflow/engine.rs` | Wrap creation sequence in SQLite transaction (`BEGIN IMMEDIATE` / `COMMIT`) | Lines 297-369 |
| `conductor-core/src/workflow/engine.rs` | Wrap resume sequence in SQLite transaction | Lines 803-868 |
| `conductor-core/src/workflow/engine.rs` | Add checkpoint write after terminal state transitions | Lines 466-480 |
| `conductor-core/src/workflow/executors.rs` | Add checkpoint write when entering Waiting state | Lines 1424-1426 |
| `conductor-core/src/workflow/manager.rs` | Add `get_checkpoint_data()` query method | After existing queries |

#### Checkpoint file location

`~/.conductor/checkpoints/<workflow_run_id>.json`

Predictable from run ID alone, satisfying the protocol's requirement that resumption can find the checkpoint without runtime context.

#### Checkpoint JSON schema

```json
{
  "schema_version": 1,
  "workflow_run_id": "01HXYZ...",
  "workflow_name": "my-workflow",
  "captured_at": "2026-03-21T15:30:00Z",
  "process_state": { "status": "running", "position": 3, "iteration": 0 },
  "progress": { "total_steps": 5, "completed": 2, "failed": 0, "pending": 3, "running": 0, "skipped": 0 },
  "completed_step_keys": [["lint", 0], ["test", 0]],
  "inputs_snapshot": { "repo_path": "/path/to/repo" },
  "last_action": "Completed step 'test' (ok)",
  "next_action": "Execute step 'deploy'"
}
```

#### Checkpoint write triggers

| Trigger | Classification | Location |
|---------|---------------|----------|
| Run reaches terminal state (Completed/Failed) | Mandatory | engine.rs:466-480 |
| Run enters Waiting state (gate) | Mandatory | executors.rs:1424-1426 |
| Run is cancelled | Optional | manager.rs:250-303 |

#### Checkpoint validation on resume

1. `schema_version` matches expected version.
2. `workflow_run_id` matches the run being resumed.
3. `completed_step_keys` are a subset of actual step records in DB.

If validation fails, fall back to existing DB-only resume path.

#### Transaction boundaries (highest-impact, lowest-effort improvement)

```rust
// execute_workflow(), wrap lines 297-369:
conn.execute_batch("BEGIN IMMEDIATE")?;
// ... create_workflow_run_with_targets, set_iteration, set_bot_name, set_inputs ...
conn.execute_batch("COMMIT")?;

// resume_workflow(), wrap reset + restart:
conn.execute_batch("BEGIN IMMEDIATE")?;
// ... reset_failed_steps / reset_completed_steps / reset_steps_from_position ...
// ... update_workflow_status(Running) ...
conn.execute_batch("COMMIT")?;
```

#### Config changes

None.

#### Test cases

- Write a checkpoint, read it back, verify all fields match.
- Write with `schema_version=1`, read with expected version=2, verify fallback to DB-only resume.
- Integration: start a workflow, kill mid-execution, resume. Verify checkpoint is used, workflow continues from correct step.
- Transaction: verify that if `set_workflow_run_inputs` fails, entire creation sequence is rolled back (no orphaned `workflow_runs` record).

---

### W1-T08: Verification-Gated Commits

**Pattern**: verification-gated-commit-protocol@1.1.0
**Depends**: W1-T06 (gate decision triggers FSM transition)
**Estimated effort**: Large (1-2 weeks)

#### Files to create

| File | Purpose |
|------|---------|
| `conductor-core/src/workflow/commit_gate.rs` | `CommitGateConfig`, `GateDecision` enum, `evaluate_commit_gate()`, `detect_agent_commits()` |

#### Files to modify

| File | Change | Location |
|------|--------|----------|
| `conductor-core/src/workflow/mod.rs` | Add `pub(crate) mod commit_gate;` | Module declarations |
| `conductor-core/src/workflow/executors.rs` | Add post-step verification check after `can_commit` agent steps | Lines 200-250 area (agent completion path) |
| `conductor-core/src/workflow_dsl/types.rs` | Add `verify_before_commit: bool` and `commit_checks: Vec<String>` fields to `CallNode` | CallNode struct definition |
| `conductor-core/src/workflow_dsl/parser.rs` | Parse `verify_before_commit` and `commit_checks` attributes | CallNode parsing section |

#### New types

```rust
#[derive(Debug, Clone)]
pub struct CommitGateConfig {
    pub checks: Vec<String>,  // shell commands; all must exit 0
    pub enabled: bool,
}

#[derive(Debug)]
pub enum GateDecision {
    Accept,
    Reject { failed_check: String, stderr: String, exit_code: Option<i32> },
}

pub fn evaluate_commit_gate(working_dir: &str, config: &CommitGateConfig) -> Result<GateDecision>
pub fn detect_agent_commits(working_dir: &str, before_sha: &str) -> Result<Vec<String>>
```

#### DSL extension

```
call my-agent {
    can_commit = true
    verify_before_commit = true
    commit_checks = ["cargo test", "cargo clippy -- -D warnings"]
}
```

#### Integration into executor flow

Before marking a step as Completed, if `can_commit && verify_before_commit`:

1. `detect_agent_commits()` compares HEAD before/after agent execution.
2. If new commits exist, `evaluate_commit_gate()` runs each check command.
3. `GateDecision::Accept` -> proceed to Completed.
4. `GateDecision::Reject` -> mark step as Failed with verification failure reason.

Pre-step SHA capture is required: record `git rev-parse HEAD` before agent execution begins.

#### Config changes

None.

#### Test cases

- `evaluate_commit_gate` with a check that succeeds: returns `Accept`.
- `evaluate_commit_gate` with a check that fails: returns `Reject` with stderr.
- `detect_agent_commits` in a test repo with known commit history.
- Integration: workflow with `verify_before_commit = true`, agent makes commits that fail check, step marked Failed.
- Negative: without `verify_before_commit`, gate is not evaluated (no performance overhead).

#### Shared test infrastructure

Extend `init_temp_repo()` from `git.rs` tests for temp git repos with known commit histories.

---

## Minimum Viable Wave 1

If time is constrained, implement these 4 items (including prerequisite):

| Priority | Task | Standalone Value | Effort |
|----------|------|-----------------|--------|
| 1 | **W1-T00: SubprocessFailure refactor** | Prerequisite. Unblocks all error-handling patterns. | 3-5 days |
| 2 | **W1-T01: Semantic exit codes** | Immediate value for CI/scripting. Scripts can distinguish "git failed" from "bad input" from "DB locked". | 1-2 days |
| 3 | **W1-T06: FSM state specification** | Immediate value for debugging. Invalid transitions are logged, revealing hidden bugs. | 1 week |
| 4 | **W1-T04: Process escape hatch** | Immediate value for stuck users. Unblocks the most common stuck states (`--force`). | 3-5 days |

Total minimum viable effort: **1.5-2 weeks**.

## Recommended Implementation Order

```
Week 1:  W1-T00 (SubprocessFailure) + W1-T06 (FSM spec, parallel)
Week 2:  W1-T01 (exit codes) + W1-T04 (escape hatch) + W1-T06 continues
Week 3:  W1-T02 (retry module) + W1-T07 (checkpoints)
Week 4:  W1-T03 (apply retry) + W1-T05 (recovery)
Week 5+: W1-T08 (verification-gated commits)
```

## Risk Assessment

### Highest-Risk Changes

| Risk | Severity | Mitigation |
|------|----------|-----------|
| SubprocessFailure refactoring breaks call sites | High (many files) | Mechanical change; compiler errors find all sites. Full test suite after each batch. Split into multiple PRs if needed. |
| Retry adds latency to user-facing operations | Medium | Short initial backoff (1s), max_attempts=3. Worst-case delay for permanent failure: ~7s. Log each retry. |
| FSM transition table rejects a legitimate transition | Medium | Warn-only mode first. Full test suite. Promote to reject mode only after zero unexpected warnings. |
| `--force` flag misuse causes data loss | Low | Audit logging via tracing. Confirmation prompt for high-risk overrides. |

### Complexity Hotspots

| File | Lines | Why Risky | Mitigation |
|------|-------|-----------|-----------|
| `workflow/executors.rs` | 3,204 | 50+ transition sites for eventual FSM migration | Gradual migration via warn-only guards; do NOT refactor all 50 sites in Wave 1 |
| `feature.rs` | 2,409 | ~12 inline Command calls need SubprocessFailure updates | Group changes by function; test independently |
| `worktree.rs` | 2,233 | Core lifecycle; escape hatch and retry both touch this file | Keep escape hatch and retry in separate PRs |

### Inter-Pattern Risks

| Risk | Patterns | Mitigation |
|------|----------|-----------|
| Retry + escape hatch interaction | bounded-retry + escape-hatch | Different layers: `--force` is user-initiated (CLI), retry is automatic (library). No conflict. |
| Checkpoint write during retry backoff | checkpoint + retry | Checkpoints at state transitions, retry within a single step. No overlap. |
| FSM guard rejects recovery transition | fsm-spec + recovery | Recovery uses `cancel_run()` which bypasses transition dispatcher. Recovery module uses manager directly. |

## Rollback Strategy

Each pattern is a separate PR. Rollback is per-PR revert.

| Pattern | Rollback Risk |
|---------|--------------|
| SubprocessFailure (W1-T00) | High churn but mechanical; all changes reversible |
| semantic-exit-code (W1-T01) | Low; no behavior change for non-zero exit |
| bounded-retry (W1-T02/T03) | Low; reverts to original non-retry behavior |
| escape-hatch (W1-T04) | Low; reverts to original guards |
| recovery (W1-T05) | Low; new commands only |
| fsm-spec (W1-T06) | Low if using warn-only mode |
| checkpoint (W1-T07) | Medium; removing transaction boundaries re-introduces inconsistency risk |
| gated-commit (W1-T08) | Low; opt-in only |

Migration v049 (checkpoint_version column) remains in schema if rolled back but is harmless when unused.

## Summary Statistics

| Metric | Value |
|--------|-------|
| Total patterns | 7 |
| Prerequisite refactors | 1 (SubprocessFailure, W1-T00) |
| New modules to create | 6 (`retry.rs`, `escape_hatch.rs`, `recovery.rs`, `transitions.rs`, `checkpoint.rs`, `commit_gate.rs`) |
| Existing files to modify | ~15 |
| New DB migrations | 1 (v049) |
| New CLI subcommands | 2 (`worktree repair`, `workflow recover`) |
| New CLI flags | 3 (`--force`, `--offline`, `verify_before_commit` DSL attribute) |
| Estimated total effort | 4-6 weeks (with parallelization of tracks) |
| Critical path | W1-T00 (3-5 days) then parallel tracks |
| Minimum viable integration | W1-T00 + W1-T01 + W1-T06 + W1-T04 (1.5-2 weeks) |
