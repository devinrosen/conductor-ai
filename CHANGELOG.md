# Changelog

## [Unreleased]

## [0.10.0] — 2026-04-30

The headline of 0.10.0 is **the workflow engine extraction is done.** `runkon-flow` is a standalone crate that conductor-core consumes through six trait implementations. A second runtime crate (`runkon-runtimes`) carries the portable agent-runtime layer. Conductor's own engine and DSL have been deleted; everything goes through `FlowEngine::run()` / `resume()` now.

### Added

- **`runkon-flow` crate** — standalone, harness-agnostic workflow engine carrying the DSL parser, execution loop, and six host-implementable traits (`ActionExecutor`, `ItemProvider`, `GateResolver`, `RunContext`, `WorkflowPersistence`, `TriggerSource`). Conductor is now the first harness. (#2700, #2685, #2688, #2699 — Phases 1–3)
- **`runkon-runtimes` crate** — portable agent-runtime layer extracted from `conductor-core` per RFC 007. Carries `RunHandle`, `RunStatus`, `AgentRuntime`, `RunTracker`, headless subprocess plumbing, and `PermissionMode`. (#2700, #2720, #2721, #2723)
- **`SqliteWorkflowPersistence`** moved into `runkon-flow` as an optional `sqlite` feature. The comm-harness (Phase 5) gets a production-ready persistence backend by enabling the feature. (#2719)
- **`{{base_branch}}` workflow variable** — pre-resolved once at the start of `review-pr.wf` by `resolve-pr-base.sh` and exposed via the engine's variable substitution layer. Reviewer agents no longer compute the PR base themselves (which broke when an agent `cd`'d into the wrong worktree). (#2737)
- **Generic FlowOutput extras → variable map plumbing** — any string-valued top-level field in a script step's FLOW_OUTPUT is now exposed as `{{name}}` to subsequent steps, with `ENGINE_INJECTED_KEYS` shadowing prevention. (#2737)
- **Deterministic off-diff finding filter** in `submit-review.sh` — drops blocking findings whose cited file is not in the PR diff before building the review body. Catches reviewer hallucinations that prompt-tightening alone misses. (#2735)

### Changed

- **Engine wiring:** `execute_workflow_standalone` and `resume_workflow_standalone` now delegate to `FlowEngine::run()` / `FlowEngine::resume()`. Conductor-core's parallel engine and DSL implementations have been removed. (#2575, #2598, #2618 — Phase 3)
- **`ChildWorkflowRunner` trait** narrowed: `execute_child` and `resume_child` now take `&ChildWorkflowContext` (an 8-field projection) instead of the full `&ExecutionState`. Future `ExecutionState` field renames no longer break harness implementors. (#2729)
- **`PermissionMode` is now opaque** in `runkon-runtimes`. Vendor-specific values (Claude's `plan` / `repo-safe`) live in conductor-core's `AgentPermissionMode` and convert at the boundary. (#2720)
- **`AgentRun` split** into a portable `RunHandle` (in `runkon-runtimes`) and a richer conductor-domain `AgentRun` (in `conductor-core`). The portable runtime no longer carries `worktree_id` / `repo_id` / `parent_run_id` / `WaitingForFeedback`. (#2721)
- **`ApprovalMode`** now has a conductor-core-owned enum behind the gate bridge; internal executor types no longer depend on `runkon_flow::dsl::ApprovalMode` directly. (#2727)
- **`<<<CONDUCTOR_OUTPUT>>>` → `<<<FLOW_OUTPUT>>>`** rename across .rs / .md / .wf / .sh files. The runkon-flow public-API surface no longer carries conductor-branded protocol identifiers. (#2726, #2730)
- **`prompt_builder::build_variable_map`** signature: `HashMap<&str, String>` → `HashMap<String, String>`. The borrow model was a small premature optimization; String keys allow dynamic insertion from prior contexts without lifetime gymnastics. (#2737)
- **`HumanApprovalGateResolver` and PR gate resolvers** moved off direct `gh` CLI invocations and onto the bridge layer. (#2727)
- **Workflow review pipeline:** review-aggregator's `overall_approved` decision is now derived from the post-filter blocking-findings count in `submit-review.sh`, so a clean filtered review correctly approves instead of mis-reporting "Changes Requested." (#2735)

### Fixed

- **`ticket_url`, `ticket_title`, `ticket_body`, `ticket_source_id`, `ticket_source_type`, `ticket_raw_json`, `repo_name`** template variables resolved to empty strings in script `env` blocks. The `prompt_builder` filter discarded all 11 `ENGINE_INJECTED_KEYS` from `state.inputs`, but the engine only re-injected 4 of them explicitly. (#2738)
- **`foreach` child workflow runs** now inherit `ticket_id` and `repo_id` lineage from the parent run. Previously the lineage was lost because `ForeachParentCtx::make_child_state` projected from a forked `child_state` whose `inputs` had been cleared. (#2733)
- **Heartbeat watchdog races** during long parallel/foreach waits — `last_heartbeat` is now refreshed inside both wait loops via `ExecutionState::tick_heartbeat_throttled()`, preventing the watchdog reaper from claiming a still-running workflow after >60 s. Concrete repro on review-pr previously double-ran review-aggregator + submit-review (PR review was posted twice). (#2734)
- **`as = "<bot>"` on script steps** now correctly threads the bot identity through script execution and injects `GH_TOKEN` for that bot. (#2716, #2717)
- **`retries` on parallel-block calls** previously silently ignored — `ActionParams::retries_remaining` was passed but the parallel executor never retried failed branches. (#2740, closes #2578)
- **Cancellation token isolation** in foreach: each fan-out item now gets its own `current_execution_id` Arc so a `cancel_run` targeting one in-flight executor cannot clobber another sibling's slot. (#2729)
- **Shell scripts emit `<<<FLOW_OUTPUT>>>` markers** — earlier rename pass missed `*.sh` and conditional reviewers in `review-pr.wf` silently skipped because their detect-* steps had empty `markers_out`. (#2730)

### Security

- **CORS hardening** in `conductor-web`: explicit method/header allowlist instead of wildcard. Hooks documentation calls out the threat model for command execution. (#2714)
- **Path traversal** rejected in workflow DSL: absolute paths and `..` traversal are blocked at parse time. (#2714)
- **JSON injection guard** in `resolve-pr-base.sh`: branch names go through `jq -nc --arg` instead of string interpolation. (#2737)
- **Sub-workflow event sinks** are now propagated on resume so resumed child workflows emit step events to TUI/web consumers. Previously `resume_child` hardcoded `event_sinks: vec![]`. (#2729)

### Deprecated

- `[notifications.workflows]` — Use `[[notify.hooks]]` with `on` patterns instead.
  A deprecation warning is now emitted at startup when this section is present in `config.toml`.
  The struct will be removed in the next release.

#### Migration

**Before:**
```toml
[notifications.workflows]
on_failure = true
on_success = false
on_gate_human = true
on_gate_ci = false
on_gate_pr_review = true
```

**After:**
```toml
[[notify.hooks]]
on = "workflow_run.failed"
run = "notify-send 'Conductor' 'Workflow failed'"

[[notify.hooks]]
on = "gate.waiting"
url = "https://hooks.slack.com/services/..."
```

| Old flag | Equivalent hook `on` value |
|---|---|
| `on_failure = true` | `"workflow_run.failed"` |
| `on_success = true` | `"workflow_run.completed"` |
| `on_gate_human = true` | `"gate.waiting"` |
| `on_gate_ci = true` | `"gate.waiting"` |
| `on_gate_pr_review = true` | `"gate.waiting"` |
| Multiple flags | `"workflow_run.failed,gate.waiting"` (comma-separated OR) |

> **Note:** `on_gate_ci`, `on_gate_human`, and `on_gate_pr_review` all map to the same
> `gate.waiting` event — per-gate-type discrimination is not yet supported at the hook level.
