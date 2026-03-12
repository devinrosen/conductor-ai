# Conductor Roadmap

Priority order as of 2026-03-12. See linked GitHub issues for full details.

## Tier 1 — Near-term, High Value

Small scope, immediately useful. Start here.

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 1 | [#146](https://github.com/devinrosen/conductor-ai/issues/146) | Plugin system for custom ticket sources via CLI adapter | New ticketing system integration in progress |

## Tier 2 — Quality & Safety

Mostly independent, high signal-to-effort ratio.

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 2 | [#662](https://github.com/devinrosen/conductor-ai/issues/662) | TUI redesign: persistent workflow column, footer status bar, simplified navigation | Remove tickets view, replace dynamic header with 1-line footer, add always-visible workflow column (right ~35%), directional column switching (`Ctrl+h`/`Ctrl+l`) |
| 3 | [#667](https://github.com/devinrosen/conductor-ai/issues/667) | In-TUI theme picker with immediate preview | Modal picker, live preview, persists to config.toml on confirm |
| 5 | [#140](https://github.com/devinrosen/conductor-ai/issues/140) | Role-based tool profiles for scoped agent MCP access | Important as parallel agent usage scales |

## Tier 3 — Larger Investments

High value but require more design and implementation work.

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 6 | [#274](https://github.com/devinrosen/conductor-ai/issues/274) | Dependency graph, impact analysis, and conflict-aware scheduling | Phased: dependency edges → impact analysis → DAG-aware scheduling. Absorbs cost-awareness from #142 as a scheduling signal. |
| 7 | [#137](https://github.com/devinrosen/conductor-ai/issues/137) | Agent-to-human notifications from agent runs | |
| 8 | [#144](https://github.com/devinrosen/conductor-ai/issues/144) | Cost analytics dashboard — spend over time by repo | Feeds into #274's cost-aware scheduling |
| 9 | [#142](https://github.com/devinrosen/conductor-ai/issues/142) | Cost budgeting and spending limits per run, workflow, and repo | Deferred — smart scheduling (#274) is higher priority; hard spend caps remain useful as a safety net |

## Known Limitations

| Area | Limitation | Details |
|------|-----------|---------|
| GitHub sync | Sub-issues not supported | Ticket sync uses `gh issue list` which returns a flat list. GitHub sub-issues (parent/child relationships) require the GraphQL API and are not yet pulled in. |

## Deferred — Phase 5

| Issue | Title | Notes |
|-------|-------|-------|
| [#9](https://github.com/devinrosen/conductor-ai/issues/9) | Daemon extraction — async service with IPC | Build once parallel agent workflows make the TUI-must-be-open limitation painful. Requirements will be clearer then. |
