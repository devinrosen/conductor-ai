# RFC 010: `for_each_ticket` Workflow Step Type

**Status:** Draft
**Date:** 2026-04-04
**Author:** Devin
**Closes:** RFC 009 (remaining workflow engine primitives)
**Tracks:** [#1743](https://github.com/devinrosen/conductor-ai/issues/1743)

---

## Problem

RFC 009 built the data model for ticket dependencies: the `ticket_dependencies` table, `TicketInput.blocked_by`/`.children`, `get_ready_tickets()`, and the `conductor_get_ready_tickets` MCP tool. What it deliberately deferred was the workflow engine primitive that acts on that graph.

Today a workflow can operate on a single ticket. There is no mechanism to drive work across a set of tickets — fanning out in dependency order, parallelizing where safe, sequencing where required, and recovering when a child fails. That gap is what this RFC closes.

---

## Proposed Design

### 1. DSL syntax

A new top-level step type, `for_each_ticket`, fits the existing grammar as a new production under `node`:

```
for_each_ticket IDENT "{" for_each_kv* "}"
for_each_kv := "scope"       "=" scope_block
             | "max_parallel" "=" NUMBER
             | "workflow"     "=" STRING
             | "inputs"       "=" map
             | "on_child_fail" "=" ("halt" | "continue" | "skip_dependents")
             | "on_cycle"     "=" ("fail" | "warn")
scope_block  := "{" ("ticket_id" | "label" | "query") "=" STRING "}"
```

**Example — process all children of a parent ticket in dependency order:**

```
for_each_ticket sprint-work {
  scope       = { ticket_id = "{{inputs.root_ticket_id}}" }
  max_parallel = 3
  workflow    = "ticket-to-pr"
  inputs      = { ticket_id = "{{item.id}}" }
  on_child_fail = skip_dependents
}
```

**Example — process all open tickets with a label:**

```
for_each_ticket backend-tickets {
  scope        = { label = "backend" }
  max_parallel = 2
  workflow     = "ticket-to-pr"
  inputs       = { ticket_id = "{{item.id}}" }
  on_child_fail = continue
}
```

#### Scope variants

| Variant | Semantics |
|---|---|
| `ticket_id = "..."` | Fan out over all direct children (`parent_of` edges) of the given ticket. Respects `blocks` ordering among children. |
| `label = "..."` | Fan out over all open tickets with the given label in the repo. Ordering derived from any `blocks` edges between them. |
| `query = "..."` | Reserved for a future filter expression. Not implemented in v1 — the parser accepts and stores it but execution emits an error. |

#### `max_parallel` is required

No default is provided. Forcing the author to state a concurrency cap prevents runaway fan-out on large projects. The validator rejects a `for_each_ticket` block without `max_parallel`.

#### `on_child_fail` (default: `skip_dependents`)

| Value | Semantics |
|---|---|
| `halt` | Cancel all in-flight child runs and fail the step immediately. No new tickets are dispatched. |
| `continue` | Log the failure, mark the ticket, and keep dispatching remaining ready tickets. The step succeeds if at least one child succeeded. |
| `skip_dependents` | Mark the failed ticket's direct and transitive dependents as `skipped`. Other unrelated tickets continue normally. *(default)* |

#### `on_cycle` (default: `fail`)

| Value | Semantics |
|---|---|
| `fail` | Abort the step with a clear error listing the cycle. *(default)* |
| `warn` | Log the cycle, break it by ignoring the back-edge, and continue. |

#### `{{item}}` template variable

Each child workflow run receives `{{item.id}}`, `{{item.title}}`, `{{item.url}}`, and `{{item.source_id}}` for the ticket being processed, in addition to any explicitly listed `inputs`. This mirrors how `parallel` call-level inputs work.

---

### 2. Engine execution model

#### Phase 1 — cycle detection (at step start)

Before dispatching any child run, the engine performs a DFS over the dependency subgraph for the resolved ticket set:

1. Call `get_ready_tickets()` with the step's scope to build the full candidate set.
2. Load all `ticket_dependencies` edges between tickets in that set.
3. Run DFS; if a back-edge is found, either fail or warn based on `on_cycle`.

This is static with respect to the current DB state. It runs once at step start, not repeatedly. Cycles introduced by a re-sync mid-run are not detected until the next step start or workflow run.

**Why at step start rather than at `workflow validate` time?**

The ticket set is runtime data — the validator cannot resolve `{{inputs.root_ticket_id}}` at parse time. Static analysis at validate-time is not possible for `ticket_id` and `label` scopes. The step start check is the earliest safe point.

#### Phase 2 — dispatch loop

The engine maintains a per-step in-memory queue. On each DB poll tick while the step is active:

1. Call `get_ready_tickets()` with the step's scope and exclude tickets already dispatched, in-flight, completed, or skipped.
2. Compute `available_slots = max_parallel - in_flight_count`.
3. Dispatch up to `available_slots` tickets by creating child `workflow_runs` linked to the parent step via `parent_run_id`.
4. If the queue is empty and `in_flight_count == 0`: the step is done — succeed or fail based on child outcomes.
5. If the queue is non-empty but `in_flight_count == 0` and no tickets are ready: all remaining tickets are blocked by unresolved dependencies or failed dependents — surface as a warning and end the step.

The dispatch loop reuses the existing DB poll tick already used for orphan reaping and background sync. No new timer or goroutine is needed.

#### Phase 3 — completion handling

When a child run transitions to a terminal state (`completed`, `failed`, `cancelled`):

1. Record the outcome in the step's tracking table (see §3).
2. Apply `on_child_fail` semantics if the run failed.
3. Re-evaluate the dispatch loop on the next tick.

The parent `for_each_ticket` step does not bubble child markers up — its own output is a summary context describing how many tickets succeeded/failed/were skipped.

---

### 3. DB schema

Two additions support tracking fan-out state and enabling resumability.

#### `workflow_run_step_fan_out_tickets`

Tracks per-ticket dispatch state for each active `for_each_ticket` step:

```sql
CREATE TABLE workflow_run_step_fan_out_tickets (
    id              TEXT PRIMARY KEY,           -- ULID
    step_run_id     TEXT NOT NULL REFERENCES workflow_run_steps(id) ON DELETE CASCADE,
    ticket_id       TEXT NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    child_run_id    TEXT REFERENCES workflow_runs(id),
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'running', 'completed', 'failed', 'skipped')),
    dispatched_at   TEXT,
    completed_at    TEXT,
    UNIQUE (step_run_id, ticket_id)
);
```

`status` transitions:
- `pending` → `running` when a child run is created
- `running` → `completed` | `failed` when the child run reaches a terminal state
- `pending` → `skipped` when `on_child_fail = skip_dependents` propagates through the dep graph

#### `workflow_run_steps` additions

Two new columns on the existing table:

```sql
ALTER TABLE workflow_run_steps ADD COLUMN fan_out_total    INTEGER;
ALTER TABLE workflow_run_steps ADD COLUMN fan_out_completed INTEGER DEFAULT 0;
ALTER TABLE workflow_run_steps ADD COLUMN fan_out_failed    INTEGER DEFAULT 0;
ALTER TABLE workflow_run_steps ADD COLUMN fan_out_skipped   INTEGER DEFAULT 0;
```

These are updated atomically as tickets complete and drive progress display in TUI/web.

---

### 4. Resumability

On restart, the engine finds any `workflow_run_steps` with `status = 'running'` whose workflow node is `for_each_ticket`. It reconstructs the in-memory queue by querying `workflow_run_step_fan_out_tickets`:

- `status = 'pending'`: not yet dispatched — add to queue
- `status = 'running'`: child run exists — check child run status; if still running, continue monitoring; if terminal (run was orphaned), apply `on_child_fail` semantics
- `status = 'completed'` | `'failed'` | `'skipped'`: already terminal — skip

This makes fan-out resumable across process restarts without re-dispatching completed work, consistent with the existing engine's snapshot-based resume model.

---

### 5. Cycle detection algorithm

```
fn detect_ticket_cycles(tickets: &[TicketId], deps: &[(TicketId, TicketId)]) -> Option<Vec<TicketId>>
```

Standard iterative DFS with a `visited` set and a `stack` set (current path):

1. Build an adjacency list from `deps` filtered to tickets in scope.
2. For each unvisited ticket, run DFS.
3. On entering a node, add to `stack`. On leaving, remove from `stack`.
4. If a neighbor is already in `stack`, a cycle exists — return the cycle path.
5. If no cycle is found, return `None`.

The cycle path returned is used in the error/warning message:
```
Ticket cycle detected: TICKET-42 → TICKET-17 → TICKET-8 → TICKET-42
```

`on_cycle = warn` breaks the cycle by ignoring the back-edge (not inserting the back-edge into the adjacency list for dispatch ordering). The tickets remain in scope; they just lose their dependency constraint on each other.

---

### 6. Global/repo-level concurrency cap

`max_parallel` is step-scoped. RFC 009 identified the risk of multiple simultaneous `for_each_ticket` workflows across repos saturating the machine.

**V1 approach:** No global cap is enforced. `max_parallel` is the author's responsibility. Document the risk clearly in the DSL reference.

**V2 path:** A future `[defaults] max_agent_runs` in `config.toml` can impose a machine-wide cap. The dispatch loop already has the hook: before claiming an available slot, check global in-flight count against the cap. No schema change is needed — `workflow_runs` already records status.

This is filed as a known gap, not a blocker for this RFC.

---

### 7. AST representation

New variant added to `WorkflowNode` in `conductor-core/src/workflow_dsl/types.rs`:

```rust
enum WorkflowNode {
    // ... existing variants ...
    ForEachTicket(ForEachTicketNode),
}

struct ForEachTicketNode {
    pub name:          String,
    pub scope:         TicketScope,
    pub max_parallel:  u32,
    pub workflow:      String,
    pub inputs:        HashMap<String, String>,
    pub on_child_fail: OnChildFail,
    pub on_cycle:      OnCycle,
}

enum TicketScope {
    TicketId(String),   // value may contain {{variable}} references
    Label(String),
    Query(String),      // reserved; errors at execution time
}

enum OnChildFail { Halt, Continue, SkipDependents }
enum OnCycle     { Fail, Warn }
```

---

### 8. TUI and web surface

#### TUI — `for_each_ticket` step row

In the workflow run detail view, a `for_each_ticket` step shows a progress bar:

```
► sprint-work   [████████░░░░░░░]  8/15  (2 running, 5 pending, 0 failed)
```

Expanding the step (e.g., pressing `→` or `Enter`) shows per-ticket rows:

```
  ✓ TICKET-12   Build auth module          completed
  ✓ TICKET-8    Add login endpoint         completed
  ◐ TICKET-17   Implement token refresh    running
  ◐ TICKET-23   Add logout flow            running
  ⏳ TICKET-42   Write auth tests           pending  (blocked by TICKET-17)
  ⏳ TICKET-5    Update OpenAPI spec        pending
  …
```

A `⛔` icon indicates a ticket skipped due to a failed dependency (when `on_child_fail = skip_dependents`).

The worktree/run list gains a lock icon (`🔒`) next to any worktree whose associated ticket is `pending` in an active fan-out.

#### Web — fan-out progress panel

The `/workflows/<run-id>` detail page renders the `for_each_ticket` step as a collapsible panel with the same counters and per-ticket status table. Clicking a ticket row navigates to the child workflow run detail.

---

### 9. `conductor workflow validate` additions

The validator gains two new checks for `for_each_ticket` nodes:

1. **`max_parallel` present** — error if missing.
2. **`workflow` resolves** — error if the named workflow file is not found in `.conductor/workflows/`.
3. **Input compatibility** — warn if the referenced workflow declares a `required` input that is not satisfied by the `inputs` map and is not `{{item.*}}`.

Cycle detection is **not** run at validate time (ticket data is runtime state).

---

## Decisions Made

1. **`max_parallel` required, no default** — follows RFC 009's intent. Forces explicit concurrency decisions.

2. **`skip_dependents` as the default `on_child_fail`** — more useful than `halt` for real projects where one ticket failing should not block unrelated work. `continue` is too permissive as a default; `skip_dependents` expresses the natural intent.

3. **Cycle detection at step start, not validate time** — ticket data is runtime state; static analysis cannot resolve ticket IDs. Step start is the earliest safe detection point.

4. **No shared context between parent and child workflows** — consistent with the shallow composition model from RFC 008. Child workflows start fresh; only `{{item.*}}` and explicit `inputs` cross the boundary.

5. **`query` scope reserved, not implemented in v1** — the scope of a filter expression language is large enough to warrant its own RFC. The parser slot is reserved so existing `.wf` files using `query` do not break when the feature ships.

6. **No global concurrency cap in v1** — deferred to a `config.toml` default in v2. Documents the known gap rather than shipping a half-implemented cap.

7. **Fan-out state persisted in a dedicated table** — `workflow_run_step_fan_out_tickets` instead of inlining state into `workflow_run_steps`. This keeps the core step table clean and makes per-ticket queries cheap.

---

## Open Questions

**1. Should failed child runs block the parent step from succeeding?**

`on_child_fail = continue` allows the step to succeed with partial failures. Is there a minimum success threshold analogous to `parallel`'s `min_success`? V1 leaves this implicit: `continue` means the step succeeds regardless; `halt` and `skip_dependents` fail the step if any child fails. A `min_success = N` option could be added later without a schema change.

**2. Should `{{item}}` include the full ticket object or just IDs?**

Exposing `{{item.raw_json}}` would give child workflows access to the full upstream payload without a separate MCP call. The downside is coupling child workflow prompts to source-specific JSON structure. V1 exposes only typed fields (`id`, `title`, `url`, `source_id`, `state`). Raw JSON access can be added in a follow-up if agent prompts need it.

**3. Cross-repo fan-out**

`for_each_ticket` operates on tickets in the workflow's registered repo. A fan-out over tickets from multiple repos (e.g., a Vantage project spanning two GitHub repos) is not supported in v1. This requires resolving which worktree to create for each ticket, which touches the feature branch coordination work from RFC 008.

---

## Prerequisites

- RFC 009 implementation complete: `ticket_dependencies` table, `get_ready_tickets()`, `conductor_get_ready_tickets` MCP tool — **all done as of 2026-04-04**
- No other blocking dependencies

---

## Implementation Order

1. DB migration: `workflow_run_step_fan_out_tickets` table + `fan_out_*` columns on `workflow_run_steps`
2. AST: `ForEachTicketNode` + supporting enums in `types.rs`
3. Parser: extend lexer and recursive descent parser for `for_each_ticket` syntax
4. Validator: `max_parallel` required, workflow resolves, input compatibility checks
5. Engine: cycle detection, dispatch loop, completion handler, resumability
6. CLI: `workflow run-show` renders fan-out progress (ticket count, per-ticket status)
7. TUI: fan-out progress bar and per-ticket expansion in workflow run detail view
8. Web: fan-out progress panel in run detail page
9. Update `docs/workflow/engine.md` with `for_each_ticket` construct documentation

Steps 1–6 are the core and should land together. Steps 7–9 are independent follow-ups.

---

## What This Enables

Once implemented, a single workflow file can drive a full sprint autonomously:

```
workflow process-sprint {
  meta {
    description = "Process all tickets in a sprint deliverable"
    trigger     = "manual"
    targets     = ["repo"]
  }

  inputs {
    root_ticket_id  required
  }

  for_each_ticket sprint-work {
    scope        = { ticket_id = "{{inputs.root_ticket_id}}" }
    max_parallel = 3
    workflow     = "ticket-to-pr"
    inputs       = { ticket_id = "{{item.id}}" }
    on_child_fail = skip_dependents
  }
}
```

Run against a Vantage project deliverable or GitHub epic, conductor:
- Resolves all child tickets in dependency order
- Spawns up to 3 concurrent `ticket-to-pr` runs
- Respects blocking relationships — holds TICKET-42 until TICKET-17 completes
- Detects and surfaces cycles at step start rather than deadlocking
- Skips TICKET-42's dependents if TICKET-42 fails
- Resumes cleanly after a process restart, without re-dispatching completed tickets
