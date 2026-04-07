# Conductor Roadmap

Priority order as of 2026-03-28. See linked GitHub issues for full details.

> **Note:** This file tracks upcoming work only. Completed items should be removed, not moved to a "recently completed" section — git history and closed issues are the source of truth for what's done.

## Autonomous SDLC

The long-term direction for conductor is full-cycle SDLC automation: ticket quality validation, pre-implementation design review, resolution validation, deployment verification, failure remediation, and product-directed research orchestration. See [docs/AUTONOMOUS-SDLC.md](./AUTONOMOUS-SDLC.md) for the full vision and stage breakdown.

---

## Tier 1 — Near-term, High Value

Small scope, immediately useful. Start here.

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 1 | [#140](https://github.com/devinrosen/conductor-ai/issues/140) | Role-based tool profiles for scoped agent MCP access | Important as parallel agent usage scales |
| 2 | — | `validate_resolution` workflow step type | L1 of Autonomous SDLC; RFC phase — see AUTONOMOUS-SDLC.md stage 4 |
| 3 | — | Ticket quality gate (`pre_flight` step type) | Pre-flight validation before workflow spawn; RFC phase — see AUTONOMOUS-SDLC.md stage 1 |

---

## Tier 2 — Quality & Safety

Mostly independent, high signal-to-effort ratio.

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 3 | [#794](https://github.com/devinrosen/conductor-ai/issues/794) | Surface workflow-produced store files in TUI/web | Depends on #793 design landing first |

---

## Tier 3 — Larger Investments

High value but require more design and implementation work.

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 4 | [#793](https://github.com/devinrosen/conductor-ai/issues/793) | Workflow-produced data storage (extensible KV layer) | RFC phase — design must land before #794 |
| 5 | [#274](https://github.com/devinrosen/conductor-ai/issues/274) | Dependency graph, impact analysis, and conflict-aware scheduling | Phased delivery via #432–436; absorbs cost-awareness from #142 |
| 6 | [#484](https://github.com/devinrosen/conductor-ai/issues/484) | Workflow-postmortem phase 2: multi-run pattern analysis | Builds on phase 1 |
| 7 | [#144](https://github.com/devinrosen/conductor-ai/issues/144) | Cost analytics dashboard — spend over time by repo | Feeds into #274's cost-aware scheduling |
| 8 | [#142](https://github.com/devinrosen/conductor-ai/issues/142) | Cost budgeting and spending limits per run, workflow, and repo | Hard spend caps as safety net; smart scheduling (#274) higher priority |
| 9 | [#618](https://github.com/devinrosen/conductor-ai/issues/618) | Agent credential management — capability-based identity | RFC phase |

---

## Known Limitations

| Area | Limitation | Details |
|------|-----------|---------|
| GitHub sync | Sub-issues not supported | Ticket sync uses `gh issue list` which returns a flat list. GitHub sub-issues require the GraphQL API and are not yet pulled in. |

