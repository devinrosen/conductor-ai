# Conductor Roadmap

Priority order as of 2026-03-03. See linked GitHub issues for full details.

## Tier 1 — Foundation

Everything else builds on these. Start here.

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 1 | [#138](https://github.com/devinrosen/conductor-ai/issues/138) | Auto-inject startup context into agent prompts | Zero DB changes, immediate value |
| 2 | [#132](https://github.com/devinrosen/conductor-ai/issues/132) | Durable plan steps as individual DB records | Unlocks #133, #136, #139 |
| 3 | [#133](https://github.com/devinrosen/conductor-ai/issues/133) | Auto-resume runs with incomplete plan steps | Requires #132 |

## Tier 2 — Core Orchestration

The real differentiator. Makes conductor a serious multi-agent platform.

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 4 | [#125](https://github.com/devinrosen/conductor-ai/issues/125) | Orchestrate child agent runs automatically | Prerequisite for #139 |
| 5 | [#134](https://github.com/devinrosen/conductor-ai/issues/134) | Merge queue for serializing parallel agent merges | Prerequisite for safe parallelism + #139 auto-merge |
| 6 | [#139](https://github.com/devinrosen/conductor-ai/issues/139) | Multi-agent PR review + auto-merge + fix-review loop | Flagship feature; requires #125, #134 |

## Tier 3 — Quality of Life

High value, mostly independent. Can be picked up in parallel with Tier 2.

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 7 | [#141](https://github.com/devinrosen/conductor-ai/issues/141) | Improve agent output copyability in TUI | Quick win, daily annoyance |
| 8 | [#145](https://github.com/devinrosen/conductor-ai/issues/145) | Git diff view in TUI | Natural pre-review workflow step |
| 9 | [#140](https://github.com/devinrosen/conductor-ai/issues/140) | Role-based tool profiles for scoped agent MCP access | Important once parallel agents are running |
| 10 | [#142](https://github.com/devinrosen/conductor-ai/issues/142) | Cost budgeting and spending limits | Safety net before running campaigns at scale |

## Tier 4 — Organizational Features

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 11 | [#135](https://github.com/devinrosen/conductor-ai/issues/135) | Campaigns for grouping related worktrees and runs | |
| 12 | [#136](https://github.com/devinrosen/conductor-ai/issues/136) | Workflow templates for reusable prompt and plan definitions | Requires #132 |
| 13 | [#104](https://github.com/devinrosen/conductor-ai/issues/104) | Human-in-the-loop feedback and approval gates | |
| 14 | [#106](https://github.com/devinrosen/conductor-ai/issues/106) | Automated review of agent output (tests, lint) | Complements #139 |
| 15 | [#143](https://github.com/devinrosen/conductor-ai/issues/143) | Automatic worktree housekeeping and cleanup | |
| 16 | [#137](https://github.com/devinrosen/conductor-ai/issues/137) | Agent-to-human notifications from agent runs | |

## Tier 5 — Analytics & Extensibility

| Priority | Issue | Title | Notes |
|----------|-------|-------|-------|
| 17 | [#144](https://github.com/devinrosen/conductor-ai/issues/144) | Cost analytics dashboard | Do after #142 |
| 18 | [#146](https://github.com/devinrosen/conductor-ai/issues/146) | Plugin system for custom ticket sources via CLI adapter | |

## Deferred — Phase 5

| Issue | Title | Notes |
|-------|-------|-------|
| [#9](https://github.com/devinrosen/conductor-ai/issues/9) | Daemon extraction — async service with IPC | Build after #134 and #139. Once parallel agents are running and the TUI-must-be-open limitation becomes painful, the daemon's requirements will be clear. Building it before then risks designing for the wrong things. |
