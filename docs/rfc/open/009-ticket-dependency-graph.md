# RFC 009: Ticket Dependency Graph

**Status:** Draft
**Date:** 2026-04-01
**Author:** Devin

---

## Problem

Conductor treats tickets as a flat list. Every ticket source — GitHub, Jira, Vantage — models relationships between work items: parent/child deliverables, blocking dependencies, epic membership. Today those relationships are discarded at sync time; only the full upstream JSON survives in `raw_json`. Conductor has no way to reason about work order, block on incomplete dependencies, or fan out over a structured set of related tickets.

This creates a ceiling on what workflows can do. A workflow can operate on a single ticket, but there is no mechanism to drive work across a ticket graph — to automatically churn through a project's deliverables in dependency order, parallelizing where safe and sequencing where required.

This RFC proposes:
1. A `ticket_dependencies` join table that stores dependency relationships source-agnostically
2. Additions to `TicketInput` so each source can populate that table during sync
3. A set of higher-order workflow primitives that traverse the graph and spawn per-ticket runs

---

## Context: Supersedes RFC 006

[RFC 006](../closed/006-workflow-driven-ticket-sources.md) proposed replacing the hardcoded `github`/`jira` source dispatch with workflow-driven ticket operations — teams would configure `.wf` files to sync, create, and update tickets in any external system without modifying conductor's core.

RFC 009 supersedes RFC 006 for two reasons:

**1. The dependency data model breaks the workflow-driven sync approach.** `TicketInput.blocked_by` and `TicketInput.children` are typed fields that `TicketSyncer::upsert_tickets()` resolves into `ticket_dependencies` rows within a single transaction. A sync workflow implemented as a shell script cannot participate in that transaction — it would need to call `conductor ticket upsert --dep-from ... --dep-to ...` CLI commands, turning a typed Rust interface into a fragile CLI convention.

**2. A `TicketSource` trait solves the dispatch problem with less complexity.** RFC 006's primary motivation was eliminating the `match source_type` fan-out across multiple dispatch sites. A `TicketSource` trait in `conductor-core` achieves this without the infrastructure RFC 006 required (per-repo config discovery, structured workflow output format, migration path for existing sources). Adding a new built-in source becomes one Rust file implementing the trait plus a registry entry — the same effort as writing a `.wf` file, with typed guarantees and full access to the DB transaction.

The `TicketSource` trait does not solve zero-code extensibility (teams still cannot add Linear without a PR). If that becomes a real user need, it warrants a fresh RFC with RFC 009's dependency model constraints in scope. RFC 006 is closed.

---

## Proposed Design

### 1. Schema: `ticket_dependencies` table

A new join table captures directed dependency relationships between tickets already stored in the `tickets` table.

```sql
CREATE TABLE ticket_dependencies (
    from_ticket_id TEXT NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    to_ticket_id   TEXT NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    dep_type       TEXT NOT NULL DEFAULT 'blocks',
    PRIMARY KEY (from_ticket_id, to_ticket_id)
);
```

Semantics: `(from, to, 'blocks')` means ticket `from` must be resolved before work on ticket `to` should begin. A parent/child relationship (epic → story) is represented as `(epic, story, 'parent_of')`.

Both `dep_type` values drive different engine behavior (see §3), so the distinction is load-bearing, not cosmetic.

On re-sync, dependencies for a ticket are replaced: delete all rows for `to_ticket_id` from the source being synced, then reinsert from the fresh data. This keeps the table consistent with upstream without a full table wipe.

### 2. `TicketInput` additions

```rust
pub struct TicketInput {
    // existing fields...

    /// Source IDs of tickets (within the same source) that block this ticket.
    /// Populated by each source adapter during sync.
    /// Stored in `ticket_dependencies` as (blocker → this, 'blocks').
    pub blocked_by: Vec<String>,

    /// Source IDs of tickets (within the same source) that are children of this ticket.
    /// Populated for parent/epic-style tickets.
    /// Stored in `ticket_dependencies` as (this → child, 'parent_of').
    pub children: Vec<String>,
}
```

Each source adapter is responsible for populating these from its native relationship model:
- **GitHub**: linked issues ("closes #N", linked PRs via `SubIssue`)
- **Jira**: `issuelinks` of type `blocks`/`is blocked by`; `parent` field for epics
- **Vantage**: `blocked_by` and parent deliverable relationships from the SDLC API

`TicketSyncer::upsert_tickets()` resolves `source_id` values to internal ticket ULIDs and writes the `ticket_dependencies` rows. Because both sides of a dependency must exist in the `tickets` table for the FK to resolve, upsert order matters: upsert all tickets first, then resolve and write dependencies in a second pass within the same transaction.

### 3. Ready-ticket semantics

A ticket is **ready** when it has no unresolved blockers. The definition of "resolved" is:

- For `'blocks'` edges: the blocking ticket's `state` is `'closed'` **and** any workflow run linked to it has `status = 'completed'`. Both conditions must hold. This avoids the case where a ticket is administratively closed but its associated work (branch, PR, tests) is not actually done.
- For `'parent_of'` edges: readiness propagates bottom-up. A parent ticket is only eligible for a fan-out workflow once all its children are ready or completed. Children have no readiness dependency on their parent.

The ready-ticket query:

```sql
SELECT t.*
FROM tickets t
WHERE t.state != 'closed'
  -- No unresolved 'blocks' blockers
  AND NOT EXISTS (
      SELECT 1
      FROM ticket_dependencies dep
      JOIN tickets blocker ON blocker.id = dep.from_ticket_id
      LEFT JOIN workflow_runs wr ON wr.ticket_id = blocker.id
      WHERE dep.to_ticket_id = t.id
        AND dep.dep_type = 'blocks'
        AND (blocker.state != 'closed' OR COALESCE(wr.status, 'completed') != 'completed')
  )
  -- Not already linked to an active run
  AND NOT EXISTS (
      SELECT 1
      FROM workflow_runs wr
      WHERE wr.ticket_id = t.id
        AND wr.status IN ('running', 'waiting_for_feedback', 'paused')
  )
```

This query becomes a method on `TicketSyncer` and is exposed as an MCP tool so agent steps in higher-order workflows can call it.

### 4. Higher-order workflow primitives

A higher-order workflow takes a scope (a parent ticket ID, a label, or a source project ID) and drives per-ticket work runs in dependency order.

**New step type: `for_each_ticket`**

```yaml
- id: process-deliverables
  type: for_each_ticket
  scope:
    ticket_id: "{{ inputs.root_ticket_id }}"   # fan out over children
    # OR
    label: "sprint-12"                          # fan out over label
    # OR
    query: "state = 'open' AND repo_id = '...'" # arbitrary filter
  max_parallel: 4          # concurrency cap; required field, no default
  workflow: ".conductor/workflows/ticket-to-pr.wf"
  inputs:
    ticket_id: "{{ item.id }}"
```

The engine evaluates readiness before spawning each child run. Tickets whose blockers are not yet resolved are queued; the engine re-evaluates the queue each time a child run completes. This continues until the queue is empty or all remaining tickets are blocked by unresolvable dependencies (engine surfaces this as a warning, not a hard failure).

**`max_parallel` is required.** No default is provided intentionally — forcing the workflow author to set a cap prevents accidental runaway fan-out on large projects.

**New MCP tool: `conductor_get_ready_tickets`**

```
conductor_get_ready_tickets(repo_slug, root_ticket_id?, label?, limit?)
  → [{ id, source_id, title, url, dep_type, blocker_count }]
```

Exposes the ready-ticket query to agent steps. Useful for higher-order workflows implemented as agent steps rather than `for_each_ticket` steps, and for human inspection via the MCP client.

### 5. TUI and web surface

The ticket detail view gains a **Dependencies** section showing:
- Tickets this ticket is blocked by (with their current state and run status)
- Tickets this ticket blocks
- Children (for parent-of relationships)

The worktree/run list gains a subtle indicator (e.g., a lock icon) when a run is queued but blocked on upstream tickets.

---

## Open Questions

**1. `dep_type`: is `'blocks'` / `'parent_of'` the right vocabulary?**

The proposed schema uses two dep types that drive different engine behavior. Are there other relationship types from real sources (Jira's "relates to", "duplicates", "is cloned by") that need representation? For now: anything that is not a hard ordering dependency is ignored (not stored). Revisit if a concrete need arises.

**2. Cross-source dependencies**

The schema allows a GitHub issue to block a Vantage deliverable (both are rows in `tickets`). In practice this requires both sources to be synced before dependency resolution, and creates ordering sensitivity in sync. V1 should scope dependencies to same-source relationships only, enforced in `upsert_tickets()` with a log warning when a cross-source reference is encountered. Cross-source support can be added later with an explicit config opt-in.

**3. Fan-out concurrency: repo-level cap vs. step-level cap**

`max_parallel` on the step prevents runaway fan-out, but multiple simultaneous `for_each_ticket` workflows across different repos could still saturate the machine. A future repo-level or global concurrency cap may be needed. Out of scope for this RFC; flag as a known gap.

**4. How does the engine re-evaluate blocked tickets?**

When a child run completes, the engine needs to re-check the ready queue and potentially unblock waiting tickets. The polling interval is the existing DB poll tick (already used for orphan reaping and background sync). No new mechanism is needed — the `for_each_ticket` step registers a completion handler that re-evaluates the queue on each tick while the step is active.

**5. Circular dependency detection**

A project with a cycle (A blocks B, B blocks A) would deadlock the queue silently. The engine must detect cycles at step start and fail fast with a clear error listing the cycle. Depth-first search on the dependency subgraph at fan-out start is sufficient; the subgraph is small enough that this is not a performance concern.

---

## Dependencies

- **`TicketSource` trait refactor:** The `match source_type` dispatch currently spans four sites in the CLI and MCP layers. The trait should land before per-source dependency population is added — it gives each source a typed `sync() -> Vec<TicketInput>` return path, which is where `blocked_by` and `children` will be populated. Without it, dependency sync requires yet another dispatch arm per source.
- **`feat/vantage-ticket-source` branch:** Adds Vantage as a source. The `blocked` status currently maps to `open` in `map_vantage_status()`. Once the `ticket_dependencies` table exists, the sync should extract Vantage's blocking relationships into `TicketInput.blocked_by` instead of discarding them.
- **`workflow_runs.status` queryability:** `for_each_ticket` needs to know when a child run succeeded vs. failed to decide whether to unblock dependents. This is already the case via `workflow_runs.status` — no new mechanism needed.

---

## What This Enables

Once this RFC is implemented, a single workflow file can:

```yaml
name: process-sprint
inputs:
  - name: root_ticket_id
    type: string

steps:
  - id: fan-out
    type: for_each_ticket
    scope:
      ticket_id: "{{ inputs.root_ticket_id }}"
    max_parallel: 3
    workflow: ".conductor/workflows/ticket-to-pr.wf"
    inputs:
      ticket_id: "{{ item.id }}"
```

Run against an epic or Vantage project deliverable, this workflow automatically:
- Finds all child tickets in dependency order
- Spawns up to 3 concurrent agent runs
- Respects blocking relationships — waits for A before starting B if A blocks B
- Detects and surfaces cycles rather than deadlocking silently
- Re-evaluates the queue as runs complete until the project is done

---

## Future Extension: Conflict-Score-Aware Batching

Among tickets that are simultaneously ready (no dependency ordering constraint between them), `for_each_ticket` currently picks any `max_parallel` tickets. A future optimization could use **Jaccard overlap scoring** over a `ticket_impact` table to prefer low-overlap ticket pairs for the same parallel batch — minimizing merge conflict risk without changing the correctness of dependency ordering.

```
overlap(A, B) = |files(A) ∩ files(B)| / |files(A) ∪ files(B)|
```

This requires a `ticket_impact` table populated by static label-to-path mappings, AI analysis, or historical merge data — none of which are in scope for this RFC. Noted here as the natural second-order optimization once that data exists. See closed issues #432–#436 for prior design work on the impact data model.

---

## Next Steps

- [ ] Open a GH issue for the `ticket_dependencies` schema migration (prerequisite, self-contained)
- [ ] Add `blocked_by` and `children` to `TicketInput`; update `TicketSyncer::upsert_tickets()` to write the join table in a second pass
- [ ] Implement the ready-ticket query as `TicketSyncer::get_ready_tickets()`
- [ ] Expose `conductor_get_ready_tickets` as an MCP tool
- [ ] Design and implement the `for_each_ticket` step type in the workflow engine
- [ ] Update `map_vantage_status()` to extract blocking relationships into `TicketInput.blocked_by` (depends on `feat/vantage-ticket-source` merge)
- [ ] Add cycle detection to the fan-out step
- [ ] TUI/web dependency display in ticket detail view
