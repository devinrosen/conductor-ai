# RFC 019: Living Documentation

**Status:** Draft
**Date:** 2026-04-15
**Author:** Devin

---

## Problem

The diagrams in `docs/diagrams/` cover component inventory well but become unreliable as soon as the code changes. Four specific shortcomings make them less useful than reading source directly:

1. **No staleness signal.** There is no way to know if a diagram reflects the current code or a state from six months ago. Diagrams are trusted or ignored by instinct, not evidence.

2. **Happy paths only.** Failure modes, error transitions, and recovery paths are absent. This is exactly the information needed when debugging — it cannot be reconstructed from a component inventory.

3. **No code cross-references.** Diagram nodes have no link back to the source that implements them. Finding the code requires a grep.

4. **Planned vs. implemented is ambiguous.** A node on a diagram could represent running code or a desired future state. There is no convention to distinguish them.

The cumulative effect: diagrams are maintained by hand on an ad-hoc basis, drift from the code silently, and are less useful than reading source for anything beyond orientation.

---

## Goals

- Diagrams stay accurate with low ongoing maintenance cost
- Failure paths and edge cases are documented, not just happy paths
- Every diagram node can be traced back to source
- Planned work is visually distinct from implemented work
- The update mechanism is conductor eating its own dog food — a workflow, not a bespoke CI script

## Non-Goals

- Full auto-generation of all diagrams from code annotations (too brittle for behavioral/architectural diagrams)
- Replacing prose documentation in `docs/workflow/` or `docs/rfc/`
- Enforcing diagram updates as a hard CI gate on every PR

---

## Proposed Design

### 1. Two diagram classes: Generated and Illustrated

**Generated diagrams** are derived directly from code or schema and are always authoritative. Anything that can be extracted mechanically should be. Candidates:

| Diagram | Source |
|---|---|
| `database-schema.mmd` | Migrations in `conductor-core/src/db/migrations/` |
| CLI command tree (new) | `conductor-cli/src/commands.rs` (clap definitions) |
| `WorkflowRunStatus` / `WorkflowStepStatus` state machine | Rust enum `CHECK` constraints + engine transitions |

A generation script (or conductor workflow step) runs on each PR that touches the relevant source files and opens a follow-up commit if the output has changed.

**Illustrated diagrams** are hand-authored and cover behavioral/architectural content that cannot be mechanically derived: threading models, failure paths, control flow, and cross-cutting concerns. These are maintained by convention and tooling, not generation.

---

### 2. Diagram metadata header

Every `.mmd` file gets a short metadata block at the top:

```
%% RFC-019
%% class: illustrated | generated
%% verified: 2026-04-15
%% commit: abc1234
%% covers: conductor-core/src/workflow/engine.rs, conductor-core/src/workflow/executors/
```

- `class` — tells readers and tooling whether the diagram is authoritative or best-effort
- `verified` + `commit` — the last date a human or agent confirmed the diagram matches the code at that commit
- `covers` — the source files this diagram is responsible for; used by the staleness detector (see §4)

---

### 3. Code cross-reference convention

Diagram nodes that map to a specific function or struct get a comment on the same line:

```
ROA["reap_orphaned_runs()"] %% agent/manager/orphans.rs:AgentManager::reap_orphaned_runs
```

This is low friction to write and makes diagrams navigable. An LSP or grep can resolve the reference; no tooling required to consume it.

---

### 4. Planned vs. implemented distinction

Nodes representing work that does not yet exist use a dashed border (Mermaid `:::planned` style class):

```
classDef planned stroke-dasharray: 5 5
CLASSIFIER["classifier()"]:::planned
```

This makes it safe to add forward-looking nodes to a diagram without misleading readers about current state. The `planned` class is stripped from generated diagrams automatically.

---

### 5. Staleness detector workflow

A conductor workflow (`docs-staleness-check.wf`) runs on each PR:

1. **Script step**: diff the PR's changed files against each diagram's `covers` list
2. **Gate step**: if any diagram covers a changed file and its `verified` date is older than the commit being merged, surface a warning (not a hard block — illustrated diagrams require human judgment)
3. **Actor step** (optional, on-demand): if triggered manually, an agent reads the changed source files and the affected diagram, identifies specific outdated nodes, and proposes a diff

The workflow is intentionally non-blocking for illustrated diagrams. Generated diagrams are regenerated in CI and fail the check if the output diverges.

---

### 6. Failure path convention for illustrated diagrams

Illustrated diagrams adopt a convention for failure paths: error transitions use red-tinted nodes (`:::error` style class) and are annotated with the error message set in code:

```
classDef error fill:#fee,stroke:#c00
RWD["reap_workflow_runs_with_dead_parent()"]
RWD -->|"error = 'parent agent run reached\nterminal state...'"| WR:::error
```

This is enforced by convention in review, not by tooling. The RFC establishes the expectation; the diagram issues (#2190–#2192) implement it for the first three high-priority gaps.

---

## Implementation Plan

### Phase 1 — Conventions and backfill (no tooling)
- Add metadata headers to all existing `.mmd` files
- Add code cross-references to existing nodes
- Mark any forward-looking nodes with `:::planned`
- Backfill failure paths into the three highest-value diagrams: TUI threading (#2190), notification hooks (#2191), workflow engine control flow (#2192)

### Phase 2 — Generation for mechanical diagrams
- Write generation script for `database-schema.mmd` from migrations
- Write generation script for CLI command tree
- Wire both into CI as a diff-and-commit check

### Phase 3 — Staleness detector workflow
- Implement `docs-staleness-check.wf`
- Add it to the PR workflow alongside `lint-fix`
- Tune threshold (suggested: warn if `verified` is >90 days behind the modified commit)

---

## Open Questions

1. **Hard gate or warning for illustrated diagrams?** The proposal is a warning only, since illustrated diagrams require human judgment to update. Is there a scenario where a hard gate is worth the friction?

2. **Agent-assisted update in Phase 3 — scope?** The actor step that proposes diagram diffs is the most valuable part of Phase 3 but also the hardest to get right. Should it propose full replacements or surgical node-level diffs?

3. **Where does the generation script live?** A script in `.conductor/scripts/` keeps it close to the workflow that calls it. A script in `tools/` is more discoverable. Preference?

---

## Related

- devinrosen/conductor-ai#2185 — atomic insert+start (the debugging session that surfaced the diagram gaps)
- devinrosen/conductor-ai#2190 — TUI threading model diagram
- devinrosen/conductor-ai#2191 — notification hooks diagram
- devinrosen/conductor-ai#2192 — workflow engine control flow diagram
- docs/rfc/closed/005-diagram-workflows.md — prior RFC on diagramming workflows (different scope)
