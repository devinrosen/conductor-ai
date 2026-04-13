# RFC 010: `foreach` Workflow Step Type

**Status:** Draft
**Date:** 2026-04-04
**Author:** Devin
**Closes:** RFC 009 (remaining workflow engine primitives)
**Supersedes:** initial `for_each_ticket` draft
**Tracks:** [#1743](https://github.com/devinrosen/conductor-ai/issues/1743)

---

## Problem

RFC 009 built the ticket dependency data model: the `ticket_dependencies` table,
`get_ready_tickets()`, and the `conductor_get_ready_tickets` MCP tool. What it
deferred was the workflow engine primitive that acts on that graph.

But the need is broader than tickets. The Autonomous SDLC vision
([docs/AUTONOMOUS-SDLC.md](../AUTONOMOUS-SDLC.md)) calls for workflows that
drive work across multiple object types:

- **Tickets** — fan out over a sprint's deliverables in dependency order
- **Repos** — assess test coverage across every registered repo and file issues
- **Workflow runs** — find failed runs and create GH issues to improve the workflow

The initial draft of this RFC proposed `for_each_ticket` as a ticket-specific
keyword. That would require separate `for_each_repo` and `for_each_workflow_run`
primitives later, and a separate `supervisor` workflow type from AUTONOMOUS-SDLC
stage 7b that is really just `foreach workflow_runs { filter = "failed" }`.

This RFC proposes a single general `foreach` step type with a typed `over` field.
Dep-aware dispatch — the unique value for tickets — is opt-in via `ordered = true`
and is only valid for `over = tickets`. All other types get simple parallel fan-out.
One primitive replaces three.

---

## Proposed Design

### 1. DSL syntax

`foreach` is a new production under `node` in the workflow grammar:

```
foreach IDENT "{" foreach_kv* "}"
foreach_kv := "over"          "=" ("tickets" | "repos" | "workflow_runs")
            | "scope"         "=" scope_block
            | "filter"        "=" map
            | "ordered"       "=" ("true" | "false")
            | "on_cycle"      "=" ("fail" | "warn")
            | "max_parallel"  "=" NUMBER
            | "workflow"      "=" STRING
            | "inputs"        "=" map
            | "on_child_fail" "=" ("halt" | "continue" | "skip_dependents")
scope_block := "{" ("ticket_id" | "label") "=" STRING "}"
```

#### Examples

**Fan out over a sprint's tickets in dependency order:**

```
foreach sprint-work {
  over         = tickets
  scope        = { ticket_id = "{{inputs.root_ticket_id}}" }
  ordered      = true
  max_parallel = 3
  workflow     = "ticket-to-pr"
  inputs       = { ticket_id = "{{item.id}}" }
  on_child_fail = skip_dependents
}
```

**Assess test coverage across all registered repos:**

```
foreach coverage-check {
  over         = repos
  max_parallel = 2
  workflow     = "assess-coverage"
  inputs       = { repo_slug = "{{item.slug}}" }
  on_child_fail = continue
}
```

**Find failed workflow runs and file improvement issues:**

```
foreach failed-runs {
  over         = workflow_runs
  filter       = { status = "failed" }
  max_parallel = 4
  workflow     = "diagnose-and-issue"
  inputs       = { run_id = "{{item.id}}" }
  on_child_fail = continue
}
```

---

### 2. `over` types

#### `tickets`

Fans out over tickets in the workflow's repo. Scope is required.

| Scope variant | Semantics |
|---|---|
| `ticket_id = "..."` | All direct children (`parent_of` edges) of the given ticket |
| `label = "..."` | All open tickets with the given label in the repo |

`ordered = true` enables dep-aware dispatch: the engine calls `get_ready_tickets()`
on each tick and holds tickets whose blockers are not yet resolved. Without
`ordered`, all in-scope tickets are dispatched immediately up to `max_parallel`.

`on_child_fail = skip_dependents` is only meaningful when `ordered = true`. The
validator warns if it is set on an unordered ticket fan-out.

`on_cycle` applies only when `ordered = true`. Ignored otherwise.

**`{{item}}` fields:** `id`, `title`, `url`, `source_id`, `state`, `labels`

#### `repos`

Fans out over all repos registered in conductor (`repos` table). No `scope` or
`ordered` options — repos are independent, unordered, and all included by default.
A `filter` map is accepted for future use but not evaluated in v1 (validator warns
if provided).

**`{{item}}` fields:** `slug`, `local_path`, `remote_url`

#### `workflow_runs`

Fans out over workflow runs. A `filter` map narrows the set:

| Filter key | Accepted values |
|---|---|
| `status` | `completed`, `failed`, `cancelled` |
| `workflow_name` | any string (exact match) |

Only terminal runs (`completed`, `failed`, `cancelled`) are eligible — filtering
on `running` or `paused` is rejected by the validator. No `scope` or `ordered`
options apply.

**`filter` is required for `over = workflow_runs`.** Without it, the set is every
terminal run in the DB — unbounded and almost never the right intent. The validator
rejects a `foreach workflow_runs` block without at least one `filter` key.

**`{{item}}` fields:** `id`, `workflow_name`, `status`, `started_at`, `ticket_id`

---

### 3. Options reference

#### `max_parallel` (required for all types)

No default. The validator rejects a `foreach` block without it. Forces the
workflow author to make an explicit concurrency decision.

#### `on_child_fail` (default: `continue` for repos/workflow_runs, `skip_dependents` for ordered tickets)

| Value | Semantics |
|---|---|
| `halt` | Cancel in-flight child runs and fail the step immediately |
| `continue` | Log the failure and keep dispatching remaining items. Step succeeds if at least one child succeeded. |
| `skip_dependents` | *(tickets + `ordered = true` only)* Mark the failed ticket's transitive dependents as `skipped`. Unrelated tickets continue normally. |

The default differs by type because `skip_dependents` is only meaningful with a
dep graph. For `repos` and `workflow_runs`, `continue` is almost always the right
behaviour — a coverage check failing on one repo should not halt the others.

#### `ordered` (tickets only, default: `false`)

When `true`, activates dep-aware dispatch via `get_ready_tickets()`. When `false`,
all in-scope tickets are queued immediately. The validator rejects `ordered = true`
on non-ticket `over` values.

#### `on_cycle` (tickets + `ordered = true` only, default: `fail`)

| Value | Semantics |
|---|---|
| `fail` | Abort at step start with a clear error naming the cycle |
| `warn` | Log the cycle, break it by dropping the back-edge, and continue |

---

### 4. Engine execution model

The engine behaviour is the same across all `over` types; the differences are in
how items are collected and whether ordering is applied.

#### Phase 1 — item collection and cycle detection (at step start)

1. Resolve the full item set from the DB based on `over`, `scope`, and `filter`.
2. For `over = tickets` with `ordered = true`: load `ticket_dependencies` edges
   within the set; run DFS cycle detection; fail or warn based on `on_cycle`.
3. Write one `workflow_run_step_fan_out_items` row per item with `status = 'pending'`.

Cycle detection runs at step start, not at `workflow validate` time. The item set
is runtime data — `{{inputs.root_ticket_id}}` cannot be resolved statically.

#### Phase 2 — dispatch loop (DB poll tick)

On each tick while the step is active:

1. Query `workflow_run_step_fan_out_items` for `pending` items in this step.
2. For `ordered = true` tickets: filter to items whose blockers are all `completed`
   (using `get_ready_tickets()`). For all other types: all `pending` items are eligible.
3. Compute `available_slots = max_parallel - in_flight_count`.
4. Dispatch up to `available_slots` items by creating child `workflow_runs` linked
   via `parent_run_id`; update row status to `running`.
5. **Done condition:** queue empty and `in_flight_count == 0` → succeed or fail
   based on child outcomes.
6. **Stall condition:** queue non-empty but no items are eligible and
   `in_flight_count == 0` → all remaining items are permanently blocked; surface
   as a warning and end the step.

#### Phase 3 — completion handling

When a child run reaches a terminal state:

1. Update the `workflow_run_step_fan_out_items` row and `fan_out_*` counters.
2. Apply `on_child_fail` semantics if failed.
3. For `skip_dependents`: walk the dep graph from the failed ticket and mark all
   transitively blocked items as `skipped`.
4. Re-evaluate the dispatch loop on the next tick.

The `foreach` step's own output is a summary context:
```json
{
  "markers": [],
  "context": "foreach sprint-work: 12 completed, 1 failed, 2 skipped of 15 tickets"
}
```

---

### 5. DB schema

#### `workflow_run_step_fan_out_items`

Replaces the ticket-specific `workflow_run_step_fan_out_tickets` table from the
initial draft. Generalised to handle any `over` type via a `item_type` column and
a nullable `item_id` (ULID into the relevant table) plus a `item_ref` freeform
field for display.

```sql
CREATE TABLE workflow_run_step_fan_out_items (
    id           TEXT PRIMARY KEY,
    step_run_id  TEXT NOT NULL REFERENCES workflow_run_steps(id) ON DELETE CASCADE,
    item_type    TEXT NOT NULL CHECK (item_type IN ('ticket', 'repo', 'workflow_run')),
    item_id      TEXT NOT NULL,   -- FK enforced at application level; type-dependent
    item_ref     TEXT NOT NULL,   -- human-readable label (ticket title, repo slug, run id)
    child_run_id TEXT REFERENCES workflow_runs(id),
    status       TEXT NOT NULL DEFAULT 'pending'
                 CHECK (status IN ('pending', 'running', 'completed', 'failed', 'skipped')),
    dispatched_at TEXT,
    completed_at  TEXT,
    UNIQUE (step_run_id, item_type, item_id)
);

CREATE INDEX idx_fan_out_items_step ON workflow_run_step_fan_out_items(step_run_id, status);
```

No typed FK on `item_id` — the three target tables (`tickets`, `repos`,
`workflow_runs`) cannot all be referenced from a single column in SQLite without
a polymorphic FK workaround. The application layer enforces referential integrity.

#### `workflow_run_steps` additions

```sql
ALTER TABLE workflow_run_steps ADD COLUMN fan_out_total     INTEGER;
ALTER TABLE workflow_run_steps ADD COLUMN fan_out_completed INTEGER DEFAULT 0;
ALTER TABLE workflow_run_steps ADD COLUMN fan_out_failed    INTEGER DEFAULT 0;
ALTER TABLE workflow_run_steps ADD COLUMN fan_out_skipped   INTEGER DEFAULT 0;
```

---

### 6. Resumability

On restart, the engine finds `workflow_run_steps` with `status = 'running'` whose
snapshot node is `foreach`. It reconstructs the in-memory queue from
`workflow_run_step_fan_out_items`:

- `pending` → add to queue (not yet dispatched)
- `running` → child run exists; if still running, monitor; if orphaned, apply
  `on_child_fail` semantics
- `completed` | `failed` | `skipped` → already terminal, skip

No work is re-dispatched. Consistent with the engine's snapshot-based resume model.

---

### 7. Cycle detection algorithm

Applies only to `over = tickets` with `ordered = true`.

```
fn detect_ticket_cycles(
    tickets: &[TicketId],
    deps: &[(TicketId, TicketId)],
) -> Option<Vec<TicketId>>
```

Standard DFS with a `visited` set and a `stack` set (current path):

1. Build an adjacency list from `deps` filtered to tickets in scope.
2. For each unvisited ticket, run DFS.
3. On entering a node: add to `stack`. On leaving: remove from `stack`.
4. If a neighbor is already in `stack`: cycle found — return the path.

Error message format:
```
Ticket cycle detected: TICKET-42 → TICKET-17 → TICKET-8 → TICKET-42
```

`on_cycle = warn` drops the back-edge from the adjacency list before dispatch.
The tickets remain in scope; they just lose the circular constraint.

---

### 8. Global concurrency cap

`max_parallel` is step-scoped. Multiple concurrent `foreach` steps across repos
could still saturate the machine.

**V1:** No global cap. `max_parallel` is the author's responsibility. Documented
as a known gap.

**V2 path:** A `[defaults] max_agent_runs` in `config.toml` imposes a machine-wide
cap. The dispatch loop hook is already present: before claiming a slot, check total
`workflow_runs` with `status = 'running'` against the global cap. No schema change
needed.

---

### 9. AST representation

```rust
// conductor-core/src/workflow_dsl/types.rs

enum WorkflowNode {
    // ... existing variants ...
    ForEach(ForEachNode),
}

struct ForEachNode {
    pub name:          String,
    pub over:          ForeachOver,
    pub scope:         Option<TicketScope>,   // tickets only
    pub filter:        HashMap<String, String>,
    pub ordered:       bool,                  // tickets only
    pub on_cycle:      OnCycle,               // tickets + ordered only
    pub max_parallel:  u32,
    pub workflow:      String,
    pub inputs:        HashMap<String, String>,
    pub on_child_fail: OnChildFail,
}

enum ForeachOver    { Tickets, Repos, WorkflowRuns }
enum TicketScope    { TicketId(String), Label(String) }
enum OnChildFail    { Halt, Continue, SkipDependents }
enum OnCycle        { Fail, Warn }
```

---

### 10. Validator additions

For all `foreach` nodes:

1. **`over` present** — error if missing.
2. **`max_parallel` present** — error if missing.
3. **`workflow` resolves** — error if the named `.wf` file is not found.
4. **Input compatibility** — warn if the referenced workflow has a `required` input
   not covered by `inputs` or `{{item.*}}`.

Type-specific:

5. **`scope` required for `over = tickets`** — error if missing.
6. **`ordered` rejected for non-tickets** — error.
7. **`skip_dependents` requires `ordered = true`** — warn if set without it.
8. **`filter` required for `over = workflow_runs`** — error if absent. Without it
   the set is every terminal run in the DB, which is almost never correct.
9. **`filter.status` must be terminal** — error if `running` or `paused` on
   `over = workflow_runs`.

Cycle detection is **not** run at validate time (runtime data).

---

### 11. TUI and web surface

#### TUI — `foreach` step row

Progress bar in the workflow run detail view:

```
► sprint-work [████████░░░░░░░]  8/15  (2 running, 5 pending, 0 failed)
```

Expanding the step (`→` or `Enter`) shows per-item rows. Label adapts to `over` type:

```
  ✓ TICKET-12   Build auth module          completed
  ✓ TICKET-8    Add login endpoint         completed
  ◐ TICKET-17   Implement token refresh    running
  ◐ TICKET-23   Add logout flow            running
  ⏳ TICKET-42   Write auth tests           pending  (blocked by TICKET-17)
  ✗ TICKET-5    Update OpenAPI spec        failed
  ⊘ TICKET-9    Integration tests          skipped  (dep failed)
```

For `over = repos`:
```
  ✓ conductor-ai       completed
  ◐ conductor-web      running
  ⏳ conductor-mobile   pending
```

A lock icon appears next to any worktree whose ticket is `pending` in an active
ordered fan-out.

#### Web

The `/workflows/<run-id>` detail page renders `foreach` as a collapsible panel
with counters and a per-item status table. Clicking an item row navigates to the
child workflow run.

---

## Decisions Made

1. **`foreach` over `for_each_ticket`** — one keyword handles tickets, repos, and
   workflow runs. Eliminates the need for a separate `supervisor` workflow type
   (AUTONOMOUS-SDLC stage 7b) and any future `for_each_repo` primitive.

2. **`ordered = true` opt-in** — not all ticket fan-outs need dep ordering (e.g.,
   "check stale docs on all sprint tickets" doesn't). Making it explicit keeps
   simple fan-outs simple and makes the dep-ordering cost visible to the author.

3. **Different `on_child_fail` defaults per type** — `skip_dependents` for ordered
   tickets (dep graph makes it meaningful); `continue` for repos and workflow runs
   (failures on one item should not halt unrelated items).

4. **Single `fan_out_items` table, polymorphic `item_type`** — avoids three
   separate tracking tables. SQLite's lack of typed polymorphic FKs is handled at
   the application layer, consistent with how `workflow_runs.ticket_id` is already
   treated (nullable, unenforced FK).

5. **`filter` reserved but unevaluated for `repos` in v1** — the grammar slot is
   open so `.wf` files using it don't break when filtering ships.

6. **`supervisor` workflow type removed from AUTONOMOUS-SDLC** — `foreach
   workflow_runs { filter = { status = "failed" } }` inside a cron-triggered
   workflow is the same thing with less ceremony. The supervisor concept lives on
   as a usage pattern, not a distinct primitive.

7. **`filter` required for `over = workflow_runs`** — an unfiltered `foreach
   workflow_runs` would iterate over every terminal run in the DB, which is
   unbounded and almost never correct. Requiring at least one `filter` key forces
   the author to express intent explicitly. `over = tickets` and `over = repos`
   do not require a filter because their sets are naturally bounded (tickets scoped
   to a repo; repos are the registered set).

8. **No shared context between parent and child workflows** — consistent with the
   shallow composition model from RFC 008.

9. **`over = workflow_runs` deduplicates via `fan_out_items`** — when collecting
   items for a `workflow_runs` fan-out, the engine excludes any run already present
   as an `item_id` in `workflow_run_step_fan_out_items` (regardless of child run
   outcome). This makes the failure watchdog naturally idempotent across cron
   firings: each failed run is processed exactly once. If a child `diagnose-and-issue`
   run itself fails, the original run is still considered processed and will not
   re-enter the queue. A legitimately mis-diagnosed run requires manual re-queuing.
   No `since` filter or `requeue_on_child_fail` escape hatch in v1 — the permanent
   exclusion tradeoff is acceptable to avoid processing loops.

10. **Stall condition ends the step as `completed`** — when the queue is non-empty
    but no items are eligible and `in_flight_count == 0` (permanent dep-cycle
    blockage), the step ends with status `completed` and a warning marker in
    `context_out`. The parent workflow is not failed; it can inspect the marker
    and branch if needed. A stall is a data condition, not an executor error.

11. **`child_run_id` in `fan_out_items` is FK-less** — consistent with
    migration 058 which dropped the FK on `workflow_run_steps.child_run_id`.
    SQLite cannot enforce cross-table polymorphic FKs cleanly; referential
    integrity is enforced at the application layer.

12. **Child runs linked via `parent_workflow_run_id`** — the column for
    workflow-to-workflow parent linking is `parent_workflow_run_id` (migration 031),
    not `parent_run_id` (which is an FK to `agent_runs`). `foreach` child runs
    set `parent_workflow_run_id` to the enclosing workflow run's id.

---

## Open Questions

**1. `min_success` threshold**

`on_child_fail = continue` allows the step to succeed with any number of failures.
Should there be a `min_success = N` option analogous to `parallel`'s `min_success`?
V1 leaves this implicit. Can be added later without schema changes.

**2. `{{item.raw_json}}` for tickets**

Exposing the full upstream ticket payload would let child workflows access fields
not in the typed `{{item.*}}` set. Deferred — couples child prompts to
source-specific JSON structure. Revisit if a concrete agent prompt needs it.

**3. Cross-repo ticket fan-out**

A Vantage project spanning two GitHub repos would require resolving which worktree
to create per ticket. Not supported in v1. Touches RFC 008 feature branch
coordination.

**4. `foreach workflow_runs` and the daemon**

A cron-triggered `foreach workflow_runs` workflow is a known polling approximation
of event-driven remediation. Good enough for v1; the v2 daemon makes it reactive.
Not a blocker — noted in AUTONOMOUS-SDLC.md.

---

## Prerequisites

- RFC 009 implementation: `ticket_dependencies` table, `get_ready_tickets()`,
  `conductor_get_ready_tickets` MCP tool — **done as of 2026-04-04**
- No other blocking dependencies

---

## Implementation Order

1. DB migration: `workflow_run_step_fan_out_items` table + `fan_out_*` columns
2. AST: `ForEachNode` + supporting enums in `types.rs`
3. Parser: lexer + recursive descent parser for `foreach` syntax
4. Validator: all checks from §10
5. Engine: item collection, cycle detection, dispatch loop, completion handler,
   resumability — for all three `over` types
6. CLI: `workflow run-show` renders fan-out progress
7. TUI: progress bar + per-item expansion in workflow run detail view
8. Web: fan-out progress panel
9. Update `docs/workflow/engine.md` with `foreach` construct documentation

Steps 1–6 land together. Steps 7–9 are independent follow-ups.

---

## What This Enables

**Sprint automation (tickets, ordered):**

```
workflow process-sprint {
  meta {
    description = "Implement all tickets in a sprint deliverable"
    trigger     = "manual"
    targets     = ["repo"]
  }

  inputs {
    root_ticket_id  required
  }

  foreach sprint-work {
    over         = tickets
    scope        = { ticket_id = "{{inputs.root_ticket_id}}" }
    ordered      = true
    max_parallel = 3
    workflow     = "ticket-to-pr"
    inputs       = { ticket_id = "{{item.id}}" }
    on_child_fail = skip_dependents
  }
}
```

**Cross-repo test coverage audit (repos):**

```
workflow coverage-audit {
  meta {
    description = "Assess test coverage and file issues across all repos"
    trigger     = "manual"
    targets     = ["repo"]
  }

  foreach coverage-check {
    over         = repos
    max_parallel = 2
    workflow     = "assess-coverage"
    inputs       = { repo_slug = "{{item.slug}}" }
    on_child_fail = continue
  }
}
```

**Workflow failure triage (workflow_runs) — replaces AUTONOMOUS-SDLC supervisor:**

```
workflow triage-failures {
  meta {
    description = "Find failed workflow runs and file improvement issues"
    trigger     = "manual"
    targets     = ["repo"]
  }

  foreach failed-runs {
    over         = workflow_runs
    filter       = { status = "failed" }
    max_parallel = 4
    workflow     = "diagnose-and-issue"
    inputs       = { run_id = "{{item.id}}" }
    on_child_fail = continue
  }
}
```

When run on a cron schedule, `triage-failures` is the AUTONOMOUS-SDLC stage 7b
supervisor — without requiring a new primitive.
