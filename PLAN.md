# Plan: `conductor workflow resume` command (#445)

## Summary

Add a `conductor workflow resume <run-id>` command that resumes a failed or stalled workflow run from the point of failure, skipping already-completed steps. The infrastructure is already in place: `definition_snapshot` stores the immutable workflow definition, and `workflow_run_steps` tracks per-step status with iteration counters. The main work is a new `resume_workflow()` engine function that reconstructs `ExecutionState` from DB records and re-enters `execute_nodes()` with skip logic.

## Files to modify

### 1. `conductor-core/src/workflow.rs` — Engine resume logic

**New public function: `resume_workflow()`**

- Takes `conn`, `config`, `workflow_run_id`, and optional `from_step` override
- Loads the `WorkflowRun` record; validates status is `Failed` or `Running` (reject `Completed`/`Cancelled`)
- Deserializes `definition_snapshot` back into `WorkflowDef`
- Loads all `workflow_run_steps` for the run
- Reconstructs `ExecutionState` from completed steps:
  - Rebuilds `step_results` HashMap from completed step records (status, result_text, context_out, markers_out)
  - Rebuilds `contexts` Vec from completed steps' `context_out` fields (ordered by position)
  - Restores `total_cost`, `total_turns`, `total_duration_ms` from the completed steps' child agent runs
  - Restores `last_structured_output` from the last completed step that has one
  - Sets `position` to max position of completed steps + 1
- Resets the workflow run status to `Running`
- Resets any `failed`/`running` steps to `pending` (so they get re-executed)
- Calls a modified execution path that skips completed steps

**New internal function: `execute_nodes_resuming()`** (or modify `execute_nodes` with a skip set)

Rather than a separate execution path, the simplest approach is:
- Pass a `HashSet<String>` of completed step keys into `ExecutionState` (new field: `completed_steps`)
- In `execute_call_with_schema()`: if the step key is in `completed_steps`, skip it (log "skipping completed step X") and return Ok
- In `execute_parallel()`: for each agent in the parallel block, check if its step is already completed; only spawn agents that aren't
- In `execute_while()`: start from the last completed iteration rather than 0
- In `execute_gate()`: skip if already approved

This approach minimizes changes to the execution flow — the engine runs the same node tree but skips steps that are already done.

**New `WorkflowManager` method: `reset_failed_steps()`**

- Updates all steps with status `failed`/`running`/`timed_out` back to `pending` for a given run
- Used before re-entering the execution loop

**New `WorkflowManager` method: `get_completed_step_keys()`**

- Returns a set of step keys (step_name + iteration) for all completed steps in a run
- Used to build the skip set

### 2. `conductor-core/src/workflow.rs` — `ExecutionState` changes

Add field:
```rust
skip_completed: HashSet<String>,  // step keys to skip (empty for fresh runs)
```

Modify `execute_call_with_schema()` (~line 1395):
- Before the retry loop, check `if state.skip_completed.contains(&step_key) { /* restore from DB and return */ }`
- When skipping: still advance `state.position`, rebuild `step_results` entry from DB, push to `contexts`

Modify `execute_parallel()` (~line 2018):
- Filter `node.calls` to only spawn agents whose step key is NOT in `skip_completed`
- For completed agents: load their results from DB and merge into state
- Still poll only the newly-spawned agents

Modify `execute_while()` (~line 1926):
- Determine the last completed iteration from `workflow_run_steps` and start from there
- The iteration counter should resume, not reset

### 3. `conductor-cli/src/main.rs` — CLI subcommand

Add new variant to `WorkflowCommands`:
```rust
/// Resume a failed or stalled workflow run
Resume {
    /// Workflow run ID (or prefix)
    id: String,
    /// Override: resume from a specific step name
    #[arg(long)]
    from_step: Option<String>,
    /// Model override for agent steps
    #[arg(long)]
    model: Option<String>,
    /// Restart from the beginning (reuse same run record)
    #[arg(long)]
    restart: bool,
},
```

Handler logic:
- Resolve run ID (support prefix matching like `show` does)
- Call `resume_workflow()` from conductor-core
- Print progress and final result (same as `run` handler)

### 4. `conductor-web/src/routes/workflows.rs` — Web API endpoint

Add endpoint:
```
POST /api/workflows/runs/{id}/resume
```

Request body (optional):
```json
{
  "from_step": null,
  "model": null,
  "restart": false
}
```

- Spawns resume in background thread (same pattern as `run_workflow`)
- Returns 200 with the run ID

### 5. `conductor-tui/src/` — TUI resume action

Add a keybinding on the workflow runs view to resume a failed run (e.g., `r` key when a failed run is selected). This mirrors the existing cancel action pattern.

### 6. Tests

Add to `conductor-core/src/workflow.rs` tests:
- `test_resume_workflow_skips_completed_steps` — mock a run with 3 steps, first 2 completed, verify only step 3 executes
- `test_resume_workflow_rejects_completed_run` — verify error on completed run
- `test_resume_workflow_rejects_cancelled_run` — verify error on cancelled run
- `test_resume_parallel_reruns_only_failed` — parallel block with 2/3 completed, verify only 1 re-spawns
- `test_resume_from_step_override` — `--from-step` causes earlier steps to be re-run
- `test_resume_restart_flag` — `--restart` clears all step results and re-runs from scratch

## Design decisions

### Skip-set approach vs. separate resume executor
**Decision:** Add a `skip_completed` set to `ExecutionState` and check it in each node executor, rather than writing a separate `execute_nodes_resuming()`.

**Why:** The workflow node tree is recursive (if/unless/while contain nested nodes). A separate executor would duplicate the entire dispatch logic. The skip-set approach adds a single `if` check at the start of each leaf executor (call, parallel, gate) with zero changes to control flow nodes (if/unless/while just re-evaluate their conditions naturally).

### Reuse same run record vs. create new run
**Decision:** Reuse the same `workflow_run` record. Reset its status to `Running` and re-enter execution.

**Why:** This preserves the full history — completed steps, their costs, and their outputs remain linked to the same run. A new run would lose the association with prior work. The `--restart` flag reuses the record but resets all steps.

### Parallel block partial resume
**Decision:** For parallel blocks, check each agent individually. Completed agents are skipped; only failed/pending ones are re-spawned.

**Why:** This is the most useful behavior — if 4/5 parallel reviewers completed and 1 failed due to rate limiting, you only want to re-run the 1 that failed. The `parallel_group_id` column already groups these steps together.

### While loop iteration resume
**Decision:** On resume, start the while loop from the iteration where it failed, not from 0.

**Why:** While loops may have completed expensive iterations. Re-running them wastes compute and may produce different results (e.g., a review iteration that already pushed fixes). The `iteration` column on `workflow_run_steps` tracks this.

## Risks and unknowns

1. **State reconstruction fidelity:** The `ExecutionState` is rebuilt from DB records. If the engine adds new in-memory state fields in the future, resume may produce subtly different behavior. Mitigation: keep the skip-set approach simple — for completed steps, we just need their outputs, not the full runtime state.

2. **While loop condition evaluation on resume:** The while loop checks `step_results` for a marker. On resume, we rebuild `step_results` from DB, so the condition should evaluate correctly. But if the marker came from a step that's being re-run (e.g., the step inside the while body), the condition needs to reflect the *last completed iteration's* markers, not the failed one's.

3. **`always` block behavior on resume:** If the main body failed and the `always` block ran successfully, then on resume, the `always` block will run again after the body completes. This is correct behavior (always blocks should always run) but worth noting.

4. **Concurrent resume:** No locking prevents two resume calls on the same run. The status check (`Failed`/`Running`) provides a soft guard, but a race is possible. For v1 this is acceptable; v2 could add a row-level lock.

5. **Definition snapshot compatibility:** The `definition_snapshot` is deserialized back into `WorkflowDef`. If the `WorkflowDef` struct changes between versions, old snapshots may fail to deserialize. Mitigation: serde's default field handling covers additive changes; breaking changes would need a migration.
