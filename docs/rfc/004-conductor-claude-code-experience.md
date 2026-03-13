# RFC 004: Conductor as a Native Claude Code Experience

**Status:** Draft / Exploring
**Created:** 2026-03-12

---

## Background

Conductor's TUI is a keyboard-driven, real-time, stateful interface. It assumes the developer has it open and is watching it. Most developers on teams that *use* conductor-managed repos — umbrella, emblem-ios, etc. — will not run the TUI. They work in their editor, in the terminal, and increasingly in Claude Code.

This RFC explores how to expose conductor's full capability surface through Claude Code's native interaction model, so that a developer who has never opened the TUI can still:

- See their repos, tickets, and worktrees
- Create and manage worktrees
- Run workflows and approve gates
- Know what's running and what needs their attention

The goal is not to replicate the TUI inside Claude Code. It is to design an experience that feels native to Claude Code — conversational, contextual, and composable — while exposing the same power.

---

## Design principles

**1. Feel like Claude Code, not a ported TUI.**
The TUI is great for keyboard-driven navigation of lists. Claude Code is great for intent-driven, natural-language interaction. The experience should lean into that: "create a worktree for the auth bug" is better than a menu of tickets to select from.

**2. Data should be referenceable, not just printed.**
Conductor entities (tickets, worktrees, workflow runs) should be first-class `@`-mentionable objects that appear in the input picker. You should be able to drag them into any conversation, not just conductor-specific ones.

**3. Actions should be composable.**
`/conductor:create-worktree` should work standalone, but it should also be callable by Claude as part of a longer conversation: "look at the open tickets, create worktrees for the two highest priority ones, and run ticket-to-pr on each."

**4. Passive visibility without interruption.**
The status line (RFC 003 / issue #599) handles ambient awareness. Skills and MCP are for when you want to act. These are separate concerns.

**5. Don't replicate what the CLI already does well.**
`conductor workflow run`, `conductor worktree create`, etc. already work. The Claude Code experience wraps and composes these — it doesn't replace them.

---

## The four surfaces

### 1. Status line (ambient awareness)
Always visible below the input. Shows active workflow runs, pending gates, recent failures. Covered by issue #599. No interaction — read-only awareness.

### 2. MCP resources (conductor data as `@`-mentionable objects)
A `conductor` MCP server exposes conductor entities as resources. Any resource appears in the `@` mention picker in the Claude Code input. Selecting one injects the resource's content as context.

This is the most powerful surface: it makes conductor data available in *any* conversation, not just conductor skills.

### 3. Skills (lightweight action commands)
User-invocable slash commands for the most common conductor actions. Invoke with intent, Claude does the work.

### 4. Conversational (no skill needed)
With the MCP server providing tools, Claude can act on conductor directly from plain conversation once the user has established context. "Create a worktree for ticket 42" — no skill needed if the MCP tools are available.

---

## MCP server design

The conductor MCP server runs as a local process and talks to `~/.conductor/conductor.db` directly. No daemon, no network — same as the CLI.

### Resources

Resources appear in the `@` picker and inject content when selected.

| Resource URI | Content injected |
|---|---|
| `conductor://repos` | List of all registered repos with slugs, paths, issue source config |
| `conductor://repo/{slug}` | Single repo details |
| `conductor://tickets/{repo}` | All open tickets for a repo (title, status, assignee, URL) |
| `conductor://ticket/{repo}/{id}` | Single ticket with full body, labels, linked worktree |
| `conductor://worktrees/{repo}` | All worktrees for a repo with branch, status, linked ticket |
| `conductor://worktree/{repo}/{slug}` | Single worktree with branch, path, linked ticket, last agent run |
| `conductor://runs/{repo}` | Recent workflow run history for a repo |
| `conductor://run/{id}` | Single workflow run with step-by-step detail, markers, context |
| `conductor://workflows/{repo}` | Available workflow definitions with descriptions and targets |

### Tools

Tools are callable by Claude during a conversation to take actions or fetch live data.

| Tool | Arguments | Description |
|---|---|---|
| `conductor_list_tickets` | `repo` | Fetch open tickets for a repo |
| `conductor_list_worktrees` | `repo` | List worktrees for a repo |
| `conductor_create_worktree` | `repo, name, ticket_id?` | Create a worktree, optionally linked to a ticket |
| `conductor_delete_worktree` | `repo, slug` | Soft-delete a worktree |
| `conductor_sync_tickets` | `repo` | Sync tickets from GitHub/Jira |
| `conductor_run_workflow` | `workflow, repo, worktree?, pr_url?, inputs?` | Start a workflow run |
| `conductor_list_runs` | `repo, worktree?` | List workflow runs |
| `conductor_get_run` | `run_id` | Get step-by-step detail for a run |
| `conductor_approve_gate` | `run_id, feedback?` | Approve a waiting gate |
| `conductor_reject_gate` | `run_id, feedback?` | Reject a waiting gate |
| `conductor_push_worktree` | `repo, slug` | Push worktree branch to origin |

### MCP server implementation

Two options:

**Option A — Standalone binary (`conductor-mcp`)**
New crate in the workspace. Ships alongside `conductor` binary. Added to `.claude/settings.json` as an MCP server:
```json
{ "mcpServers": { "conductor": { "command": "conductor-mcp" } } }
```
Clean separation. Adding a new crate is non-trivial but keeps concerns separated.

**Option B — Subcommand of the existing CLI (`conductor mcp`)**
`conductor mcp serve` starts the MCP server. Simpler distribution — one binary does everything. Slightly awkward since MCP is a long-running server not a one-shot command.

**Recommendation:** Option B for v1. `conductor mcp serve` avoids a new crate and ships automatically with the existing binary. Can always extract to `conductor-mcp` later.

---

## Skill catalog

Skills are user-invocable slash commands. With a good MCP server, these become thin wrappers that establish context and guide intent — Claude does the actual work via MCP tools.

| Skill | Invocation | What it does |
|---|---|---|
| `tickets` | `/conductor:tickets [repo]` | Injects open ticket list into context; Claude can then create worktrees, label, triage |
| `worktrees` | `/conductor:worktrees [repo]` | Injects worktree list; Claude can push, create PRs, run workflows |
| `runs` | `/conductor:runs [repo]` | Injects recent workflow run history; Claude can resume, approve gates, run postmortem |
| `status` | `/conductor:status` | Full snapshot: all active runs, pending gates, recent failures across all repos |
| `create-worktree` | `/conductor:create-worktree [repo] [name]` | Guided worktree creation — if no args, lists available repos and open tickets for Claude to work with |
| `run-workflow` | `/conductor:run-workflow [name] [target]` | Guided workflow execution — lists available workflows if name not provided |
| `approve-gate` | `/conductor:approve-gate [run-id]` | Approves pending gate, optionally with feedback; lists pending gates if run-id not provided |

Skills are intentionally sparse. The MCP tools allow Claude to handle most conductor interactions conversationally once context is established.

---

## What the developer experience looks like

### Discovering open tickets and creating a worktree

```
/conductor:tickets umbrella
```
→ Claude injects ticket list into context

```
"Create a worktree for the most critical open ticket"
```
→ Claude calls `conductor_create_worktree` via MCP, confirms with the worktree slug and branch name

---

### Running a workflow on a PR

```
@conductor:ticket://umbrella/PROJ-42  run the publish-docs workflow on this ticket's PR
```
→ Claude reads the ticket resource (including linked PR URL), calls `conductor_run_workflow` with `pr_url`

---

### Checking on running workflows and approving a gate

```
/conductor:status
```
→ Claude injects full status (or user already sees it in the status line)

```
"Approve the merge-when-ready gate on umbrella/feat-login"
```
→ Claude calls `conductor_approve_gate` with the run ID

---

### Full natural-language workflow

```
"Look at the open umbrella tickets, find the two highest priority ones,
create worktrees for them, and kick off ticket-to-pr on each"
```
→ Claude calls `conductor_list_tickets`, selects two, calls `conductor_create_worktree` twice, calls `conductor_run_workflow` twice — all without any skill invocation

---

## What can't be replicated

Some TUI capabilities don't map cleanly to Claude Code:

| TUI capability | Status in Claude Code |
|---|---|
| Live agent log streaming | Not possible — logs are in tmux, no streaming API |
| Real-time workflow step progress | Status line shows current step; no live update without polling |
| Visual step graph | Can be generated as HTML via a skill; not inline |
| Keyboard-driven list navigation | Not applicable — Claude Code is not keyboard-nav driven |
| Multi-worktree side-by-side comparison | Not applicable |

These gaps are acceptable — developers who need real-time log watching or visual step graphs should use the TUI or web UI. The Claude Code experience is for developers who want to interact with conductor conversationally and want conductor data available in context while they work.

---

## What gets installed

When a developer installs the conductor plugin:

```
/plugin install conductor@lively-video
```

This configures:
1. **MCP server** — `conductor mcp serve` registered in `.claude/settings.json`
2. **Status line** — `~/.conductor/statusline.py` installed, `statusLineTool` configured
3. **Skills** — all skills in the conductor plugin's `skills/` directory available

One install command sets up the complete experience.

---

## Phasing

**Phase 1 — Status line + skills (no MCP)**
- Issue #599: status line script + `conductor statusline install`
- Issue #588: conductor plugin with action skills (`/conductor:tickets`, `/conductor:runs`, `/conductor:approve-gate`)
- Skills inject data via context; Claude uses the CLI via Bash for actions
- No MCP server needed — skills call `conductor` CLI directly

**Phase 2 — MCP server**
- `conductor mcp serve` subcommand
- Resources: repos, tickets, worktrees, runs
- Tools: the full action surface
- Skills become thin wrappers around MCP context establishment

**Phase 3 — Conversational**
- With MCP tools established, reduce skill surface
- Rely on Claude's ability to compose conductor operations from natural language
- Skills remain for the "lean forward" cases where you want a specific, fast interaction

Phase 1 can ship without any MCP work. Phase 2 is the highest-leverage investment and unlocks Phase 3.

---

## Open questions

1. **MCP server lifetime:** `conductor mcp serve` is long-running. How does it restart if it crashes? Does the plugin manage this, or does Claude Code handle MCP server restarts?

2. **DB contention:** The MCP server holds a connection to `~/.conductor/conductor.db`. Does this conflict with the TUI or CLI also holding connections? SQLite WAL mode should handle concurrent readers, but write contention during `conductor_create_worktree` needs testing.

3. **Auth / scope:** The MCP server has full read/write access to the conductor DB. Should destructive tools (`conductor_delete_worktree`) require confirmation, or is that Claude Code's responsibility?

4. **Multi-machine:** `~/.conductor/conductor.db` is local. Developers on different machines have different DB state. Is there a path toward a shared/synced conductor DB, or is per-machine state acceptable?

5. **Discovery:** How does a developer on umbrella know to install the conductor plugin? Should `conductor repo add` print a suggestion? Should the getting-started doc (issue #586) be the canonical path?

---

## Related

- Issue #588 — conductor Claude Code plugin (Phase 1)
- Issue #599 — status line implementation (Phase 1)
- Issue #600 — status line in plugin (Phase 1)
- RFC 003 — `--path` flag on `workflow run` (unlocks Phase 1 skills for unregistered repos)
- `docs/getting-started-cli.md` — entry point for teams adopting conductor CLI
