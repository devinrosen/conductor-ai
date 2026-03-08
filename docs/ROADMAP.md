# Conductor Roadmap

Priority order as of 2026-03-08. See linked GitHub issues for full details.

## Tier 1 — Near-term, High Value

Small scope, immediately useful. Start here.

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 1 | [#275](https://github.com/devinrosen/conductor-ai/issues/275) | Support custom base/target branch for worktrees and PRs | Small change, unlocks release branch workflows |
| 2 | [#219](https://github.com/devinrosen/conductor-ai/issues/219) | Test-coverage workflow — validate PR tests and commit missing ones | `.wf` DSL is ready; just needs the workflow file and agents |
| 3 | [#146](https://github.com/devinrosen/conductor-ai/issues/146) | Plugin system for custom ticket sources via CLI adapter | New ticketing system integration in progress |

## Tier 2 — Quality & Safety

Mostly independent, high signal-to-effort ratio.

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 4 | [#217](https://github.com/devinrosen/conductor-ai/issues/217) | Use GitHub App identity when filing off-diff issues during PR review | Clean bot identity for filed issues |
| 5 | [#218](https://github.com/devinrosen/conductor-ai/issues/218) | Run PR review swarm from a GitHub PR URL without a local clone | Useful for reviewing external PRs |
| 6 | [#140](https://github.com/devinrosen/conductor-ai/issues/140) | Role-based tool profiles for scoped agent MCP access | Important as parallel agent usage scales |

## Tier 3 — Larger Investments

High value but require more design and implementation work.

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 7 | [#274](https://github.com/devinrosen/conductor-ai/issues/274) | Dependency graph, impact analysis, and conflict-aware scheduling | Phased: dependency edges → impact analysis → DAG-aware scheduling → merge queue integration. Absorbs cost-awareness from #142 as a scheduling signal. |
| 8 | [#137](https://github.com/devinrosen/conductor-ai/issues/137) | Agent-to-human notifications from agent runs | |
| 9 | [#144](https://github.com/devinrosen/conductor-ai/issues/144) | Cost analytics dashboard — spend over time by repo | Feeds into #274's cost-aware scheduling |
| 10 | [#142](https://github.com/devinrosen/conductor-ai/issues/142) | Cost budgeting and spending limits per run, workflow, and repo | Deferred — smart scheduling (#274) is higher priority; hard spend caps remain useful as a safety net |

## Known Limitations

| Area | Limitation | Details |
|------|-----------|---------|
| GitHub sync | Sub-issues not supported | Ticket sync uses `gh issue list` which returns a flat list. GitHub sub-issues (parent/child relationships) require the GraphQL API and are not yet pulled in. |

## Deferred — Phase 5

| Issue | Title | Notes |
|-------|-------|-------|
| [#9](https://github.com/devinrosen/conductor-ai/issues/9) | Daemon extraction — async service with IPC | Build once parallel agent workflows make the TUI-must-be-open limitation painful. Requirements will be clearer then. |
