# PLAN: Feature #520 — Workflow Targets (Run Workflows on Repos and Tickets)

## Summary

Extend the workflow engine to support running workflows on **repos** and **tickets** as targets, in addition to the existing worktree/PR targets. This enables use cases like dependency audits, ticket triage, and changelog generation without requiring a worktree.

**Scope:** Backend (conductor-core) + TUI (conductor-tui)
**CLI/Web:** Deferred to Phase 2

---

## Files to Create or Modify

### conductor-core

| File | Change |
|------|--------|
| `conductor-core/src/db/migrations/029_workflow_targets.sql` | **NEW** — Add `ticket_id` and `repo_id` nullable FK columns to `workflow_runs`, with indexes |
| `conductor-core/src/workflow.rs` | **MODIFY** — Update `WorkflowRun` struct, `WorkflowExecInput`, `WorkflowExecStandalone`, `ExecutionState`, `execute_workflow()`, `build_variable_map()`, `row_to_workflow_run()` |
| `conductor-core/src/workflow_dsl.rs` | **MODIFY** — Add validation for `targets` field values |

### conductor-tui

| File | Change |
|------|--------|
| `conductor-tui/src/state.rs` | **MODIFY** — Add `Ticket` and `Repo` variants to `WorkflowPickerTarget` enum |
| `conductor-tui/src/app.rs` | **MODIFY** — Add `filter_workflows_by_target()`, `spawn_ticket_workflow_thread()`, `spawn_repo_workflow_thread()`, update `handle_run_workflow()` dispatch |
| `conductor-tui/src/input.rs` | **MODIFY** — Update 'w' key handler for Repos pane (repo workflows) and Tickets pane (ticket workflows) |

---

## Design Decisions

1. **Target discriminator via nullable FKs** — `workflow_runs` stores which FK is set (ticket_id, repo_id, or worktree_id). Only one should be non-null per run. This is enforced by application logic, not DB constraints, for simplicity.

2. **Implicit inputs** — Ticket/repo context is injected into the `inputs` map before agent execution (in `build_variable_map()`), not as special variables. Keys: `ticket_id`, `ticket_title`, `ticket_url`, `repo_id`, `repo_path`, `repo_name`.

3. **Working directory** — Repo workflows use `repo.local_path`; ticket workflows use a temporary directory. `WorkflowExecInput::worktree_path` is renamed to `working_dir` to reflect this generalization.

4. **Temp directory cleanup** — Use `tempfile::TempDir` RAII guards for ticket workflow temp dirs. The guard is kept alive in the thread that runs the workflow and dropped on completion.

5. **Workflow filtering** — The DSL already **requires** `targets` to be set (it errors if missing), so all workflows declare at least one target. Filtering matches workflows whose `targets` array contains the string matching the picker context (`"worktree"`, `"ticket"`, `"repo"`, `"pr"`). Filtering happens in TUI at picker-open time, not at execution time.

6. **No `target: none` yet** — Deferred to a future RFC (scheduled/cron workflows).

---

## Detailed Task List

### Task 1: DB Migration
- Create `conductor-core/src/db/migrations/029_workflow_targets.sql`:
  ```sql
  ALTER TABLE workflow_runs ADD COLUMN ticket_id TEXT REFERENCES tickets(id);
  ALTER TABLE workflow_runs ADD COLUMN repo_id   TEXT REFERENCES repos(id);
  CREATE INDEX idx_workflow_runs_ticket ON workflow_runs(ticket_id);
  CREATE INDEX idx_workflow_runs_repo ON workflow_runs(repo_id);
  ```
- Verify migration numbering is correct (check existing migrations directory for next number)

### Task 2: Update WorkflowRun and DB helpers
- Add `ticket_id: Option<String>` and `repo_id: Option<String>` to `WorkflowRun` struct
- Update `row_to_workflow_run()` to read new columns from query result
- Update any SQL SELECT queries that enumerate columns to include new fields

### Task 3: Extend WorkflowExecInput / WorkflowExecStandalone / ExecutionState
- Add `ticket_id: Option<&str>` and `repo_id: Option<&str>` to `WorkflowExecInput`
- Rename `worktree_path` → `working_dir` in `WorkflowExecInput` (update all call sites)
- Mirror changes in `WorkflowExecStandalone`
- Add `ticket_id` and `repo_id` fields to `ExecutionState`

### Task 4: Update execute_workflow() to write new FKs
- When inserting a new `workflow_runs` record, write `ticket_id` and `repo_id` from input
- Populate `ExecutionState` with ticket_id/repo_id from input

### Task 5: Inject implicit inputs in build_variable_map()
- For ticket target: query ticket by id, inject `ticket_id`, `ticket_title`, `ticket_url`
- For repo target: query repo by id (or use cached path), inject `repo_id`, `repo_path`, `repo_name`
- Cache ticket/repo records in `ExecutionState` to avoid repeated DB queries per step

### Task 6: Add target validation to workflow_dsl.rs
- In `validate_workflow_semantics()`, check that each value in `workflow.targets` is one of: `worktree`, `ticket`, `repo`, `pr`
- Emit validation error for unknown target types

### Task 7: Extend WorkflowPickerTarget in TUI state
- Add `Ticket { ticket_id, ticket_title, ticket_url, repo_id }` variant
- Add `Repo { repo_id, repo_path, repo_name }` variant
- Update any exhaustive match statements on this enum

### Task 8: Add filter_workflows_by_target() in TUI
- New function: filters `Vec<WorkflowDef>` by checking `wf.targets` against picker target type
- Match: `wf.targets.contains("ticket")`, `wf.targets.contains("repo")`, etc.
- No "empty targets = all contexts" logic needed — DSL enforces the field
- Used when opening the WorkflowPicker modal

### Task 8a: Workflow discovery for repo/ticket targets
- For **repo** target: call `WorkflowManager::list_defs("", repo.local_path)` — `resolve_conductor_subdir` will skip the empty worktree path and find `{repo_path}/.conductor/workflows/`
- For **ticket** target: look up ticket's `repo_id`, fetch repo, then call `WorkflowManager::list_defs("", repo.local_path)`
- No changes needed to `load_workflow_defs` itself

### Task 9: Update 'w' keybinding handlers in input.rs
- **Repos pane:** 'w' → build `WorkflowPickerTarget::Repo`, filter workflows, open picker
- **Tickets pane:** 'w' → build `WorkflowPickerTarget::Ticket`, filter workflows, open picker
- Existing worktree pane behavior unchanged

### Task 10: Add ticket/repo workflow spawn functions in app.rs
- `spawn_ticket_workflow_thread()`: creates temp dir, constructs `WorkflowExecStandalone` with ticket_id set, spawns thread, ensures cleanup
- `spawn_repo_workflow_thread()`: constructs `WorkflowExecStandalone` with repo_id set, uses repo_path as working_dir
- Update `handle_run_workflow()` dispatch to call these for new target variants

### Task 11: Update call sites for renamed worktree_path → working_dir
- Search all uses of `WorkflowExecInput { worktree_path: ... }` in TUI and CLI
- Update to use `working_dir` field name

### Task 12: Tests
- Unit test: implicit input injection for ticket/repo targets in `workflow.rs`
- Unit test: `filter_workflows_by_target()` — empty targets, ticket match, repo match, no match
- Unit test: `WorkflowRun` serialization with new nullable FK fields

---

## Risks & Unknowns

1. **Migration number conflict** — Need to verify next migration number. Check `conductor-core/src/db/migrations/` for highest current number.

2. **`worktree_path` rename scope** — Renaming to `working_dir` will affect call sites in CLI and web. Need to audit all callers carefully.

3. **`tempfile` crate availability** — Temp dir cleanup for ticket workflows requires `tempfile` crate. Verify it's already a dependency in `Cargo.toml` (it's commonly available but not guaranteed).

4. **WorkflowPickerTarget match exhaustiveness** — The enum is used in multiple match arms across TUI. Adding variants will require auditing all match sites.

5. **`targets` field parsing in DSL** — Confirmed: DSL already parses `targets` as a `Vec<String>` and enforces it is non-empty. No DSL parser changes needed.
