# Conductor: Direction

**Date:** 2026-04-16

This document is the current north star for conductor. It supersedes [VISION.md](./VISION.md), which is preserved as the founding design and historical record.

Two premises in VISION have shifted:

1. **"TUI is the primary interface."** The TUI is now an observation and gate-keeping surface. Workflows and agents are the primary users of conductor's API.
2. **"AI orchestration is Phase 4."** Phases 1–3 (core library, TUI, GitHub/Jira sync) shipped. The orchestration layer is no longer a future phase — it is the product.

This document captures how those shifts change what goes into the codebase and what stays out.

---

## Principles

### 1. Platform, not jig

From [PHILOSOPHY.md](./PHILOSOPHY.md): conductor is the platform for building jigs, not the jig itself. The jigs are `.wf` files, agent configs, and templates that users fork and customize. Conductor provides the primitives; users encode the work.

### 2. The compact-core test

> *If removing the code would just mean writing a `.wf` file instead, the code shouldn't exist.*

Add general primitives to the engine. Do not add special-purpose constructs that encode one workflow pattern in Rust. When in doubt: can two unrelated workflows use this primitive today? If not, it probably belongs in a `.wf` file, not the engine.

Concrete consequences:

- `FeatureManager` and similar managers whose behavior is expressible as a workflow are candidates for removal. See [IDEAS-feature-table-replacement.md](./IDEAS-feature-table-replacement.md).
- New engine step types for SDLC stages (e.g. `pre_flight`, `design_review`, `validate_resolution`) should be workflow templates, not Rust constructs. [AUTONOMOUS-SDLC.md](./AUTONOMOUS-SDLC.md) describes the loop; it does not prescribe new engine primitives.
- New `foreach` data sources must meet the compact-core test.

### 3. Agent-first surfaces

MCP is the primary API. The CLI is a human bootstrap tool. The TUI is observation and gate-keeping. Every new surface should ask: *does an agent need this?* If yes, MCP first. If no, prefer the TUI.

This inverts VISION's human-first premise explicitly. Downstream consequences:

- `CLAUDE.md`'s worktree workflow block is CLI-centric and reflects the human-first era — agents increasingly drive worktree creation via MCP.
- TUI-centric features like the Features tab are candidates for folding into general Workflows views.
- Small MCP improvements (typed `data` in `CONDUCTOR_OUTPUT`, `CONDUCTOR_RUN_ID` scoping, a `conductor://worktree/context` resource) are higher leverage than equivalent TUI work.

### 4. Library-first until measured

The v2 daemon remains the right endgame, but the trigger is measured signal, not asserted vision. Library-first handles more of agent-first than is obvious — polling covers peer visibility, single-process pool supervisors cover short-lived runs, SQLite WAL handles the read fan-out.

The daemon becomes necessary when a measurement forces it:

- Observed SQLite write contention under concurrent agent load
- Latency floor on pool polling that a supervised process would fix
- A specific need for push (webhooks, cross-run pool sharing, background work with no binary open)

Until then, build against library-first and keep `Serialize`/`Deserialize` on core types as VISION already specifies.

---

## What this direction is not

- **Not a roadmap.** Current priorities live in GitHub issues labeled `roadmap`.
- **Not an engine spec.** See [docs/workflow/engine.md](./workflow/engine.md).
- **Not a rewrite.** The core library, manager pattern, and crate layout from VISION all stand. The shift is in what *new* code is worth adding.

## Related documents

| Doc | Role |
|---|---|
| [PHILOSOPHY.md](./PHILOSOPHY.md) | The jig metaphor — why conductor exists |
| [VISION.md](./VISION.md) | Founding design, preserved as historical |
| [AUTONOMOUS-SDLC.md](./AUTONOMOUS-SDLC.md) | SDLC loop as a target; workflow patterns, not engine constructs |
| [IDEAS-feature-table-replacement.md](./IDEAS-feature-table-replacement.md) | Worked example of the compact-core test |
| [workflow/engine.md](./workflow/engine.md) | Engine spec |
