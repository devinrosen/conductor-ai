# Conductor Roadmap

Priority order as of 2026-03-19. See linked GitHub issues for full details.

> **Note:** This file tracks upcoming work only. Completed items should be removed, not moved to a "recently completed" section — git history and closed issues are the source of truth for what's done.

## Tier 1 — Near-term, High Value

Small scope, immediately useful. Start here.

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 1 | [#1311](https://github.com/devinrosen/conductor-ai/issues/1311) | Per-repo .conductor/config.toml for repo-level settings | Active — PR [#1314](https://github.com/devinrosen/conductor-ai/pull/1314) in review |
| 2 | [#140](https://github.com/devinrosen/conductor-ai/issues/140) | Role-based tool profiles for scoped agent MCP access | Important as parallel agent usage scales |

---

## Tier 2 — Quality & Safety

Mostly independent, high signal-to-effort ratio.

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 3 | [#1312](https://github.com/devinrosen/conductor-ai/issues/1312) | Workflow hooks — auto-trigger workflows on lifecycle events | |
| 4 | [#794](https://github.com/devinrosen/conductor-ai/issues/794) | Surface workflow-produced store files in TUI/web | Depends on #793 design landing first |

---

## Tier 3 — Larger Investments

High value but require more design and implementation work.

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 5 | [#793](https://github.com/devinrosen/conductor-ai/issues/793) | Workflow-produced data storage (extensible KV layer) | RFC phase — design must land before #794 |
| 6 | [#274](https://github.com/devinrosen/conductor-ai/issues/274) | Dependency graph, impact analysis, and conflict-aware scheduling | Phased delivery via #432–436; absorbs cost-awareness from #142 |
| 7 | [#484](https://github.com/devinrosen/conductor-ai/issues/484) | Workflow-postmortem phase 2: multi-run pattern analysis | Builds on phase 1 |
| 8 | [#137](https://github.com/devinrosen/conductor-ai/issues/137) | Agent-to-human notifications from agent runs | |
| 9 | [#144](https://github.com/devinrosen/conductor-ai/issues/144) | Cost analytics dashboard — spend over time by repo | Feeds into #274's cost-aware scheduling |
| 10 | [#142](https://github.com/devinrosen/conductor-ai/issues/142) | Cost budgeting and spending limits per run, workflow, and repo | Hard spend caps as safety net; smart scheduling (#274) higher priority |
| 11 | [#618](https://github.com/devinrosen/conductor-ai/issues/618) | Agent credential management — capability-based identity | RFC phase |

---

## Known Limitations

| Area | Limitation | Details |
|------|-----------|---------|
| GitHub sync | Sub-issues not supported | Ticket sync uses `gh issue list` which returns a flat list. GitHub sub-issues require the GraphQL API and are not yet pulled in. |

---

## Deferred — Phase 5

| Issue | Title | Notes |
|-------|-------|-------|
| [#9](https://github.com/devinrosen/conductor-ai/issues/9) | Daemon extraction — async service with IPC | Build once parallel agent workflows make the TUI-must-be-open limitation painful. Requirements will be clearer then. |
