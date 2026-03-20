# Conductor Roadmap

Priority order as of 2026-03-20. See linked GitHub issues for full details.

> **Note:** This file tracks upcoming work only. Completed items should be removed, not moved to a "recently completed" section — git history and closed issues are the source of truth for what's done.

## Tier 1 — Near-term, High Value

Small scope, immediately useful. Start here.

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 1 | [#1358](https://github.com/devinrosen/conductor-ai/issues/1358) | push-and-pr fails when feature_base_branch variable is not resolved | Blocks most ticket-to-pr workflows; unresolved `{{feature_base_branch}}` passed literally to git |
| 2 | [#1367](https://github.com/devinrosen/conductor-ai/issues/1367) | 'Cannot determine repo for this workflow run' on historical runs | Affects 98% of historical workflow runs; repo_id backfill + deleted worktree fallback |
| 3 | [#1353](https://github.com/devinrosen/conductor-ai/issues/1353) | workflow_run_id not pre-populated when running workflow on a workflow run | Missing prefill in form modal |
| 4 | [#1356](https://github.com/devinrosen/conductor-ai/issues/1356) | 'w' in workflow runs pane targets worktree instead of selected workflow run | Input routing bug — workflow column focus not checked |
| 5 | [#1357](https://github.com/devinrosen/conductor-ai/issues/1357) | Consolidate workflow target resolution into single method | Cleanup to prevent future target-routing bugs like #1353 and #1356 |
| 6 | [#140](https://github.com/devinrosen/conductor-ai/issues/140) | Role-based tool profiles for scoped agent MCP access | Important as parallel agent usage scales |

---

## Tier 2 — Quality & Safety

Mostly independent, high signal-to-effort ratio.

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 7 | [#794](https://github.com/devinrosen/conductor-ai/issues/794) | Surface workflow-produced store files in TUI/web | Depends on #793 design landing first |

---

## Tier 3 — Larger Investments

High value but require more design and implementation work.

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 8 | [#793](https://github.com/devinrosen/conductor-ai/issues/793) | Workflow-produced data storage (extensible KV layer) | RFC phase — design must land before #794 |
| 9 | [#274](https://github.com/devinrosen/conductor-ai/issues/274) | Dependency graph, impact analysis, and conflict-aware scheduling | Phased delivery via #432–436; absorbs cost-awareness from #142 |
| 10 | [#484](https://github.com/devinrosen/conductor-ai/issues/484) | Workflow-postmortem phase 2: multi-run pattern analysis | Builds on phase 1 |
| 11 | [#144](https://github.com/devinrosen/conductor-ai/issues/144) | Cost analytics dashboard — spend over time by repo | Feeds into #274's cost-aware scheduling |
| 12 | [#142](https://github.com/devinrosen/conductor-ai/issues/142) | Cost budgeting and spending limits per run, workflow, and repo | Hard spend caps as safety net; smart scheduling (#274) higher priority |
| 13 | [#618](https://github.com/devinrosen/conductor-ai/issues/618) | Agent credential management — capability-based identity | RFC phase |

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
