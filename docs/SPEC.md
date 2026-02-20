# Conductor: Multi-Repo Orchestration Service + TUI

**Status:** Draft
**Date:** 2026-02-20

## Problem

Developer workflows that span multiple repositories need a consistent way to:
- Track what you're working on across projects
- Manage git worktrees without remembering paths and branch naming conventions
- See issues and tickets from GitHub and Jira in one place, tied to the repo they belong to
- Orchestrate AI-assisted development (planning, implementation, review) against any registered repo

Today this is done ad-hoc with shell scripts, per-project CLAUDE.md files, and manual context switching. Conductor productizes this into a local-first service with a TUI (primary) and optional desktop/web app.

## Core Concepts

### Repo
A registered git repository. Conductor stores a local clone and manages worktrees within a workspace directory. Each repo has:
- A **slug** (e.g., `orchestra`, `api-gateway`)
- A **local path** to the bare or main checkout
- A **remote URL** (origin)
- An **issue source** configuration (GitHub, Jira, or both)
- A **workspace directory** containing worktrees

### Worktree
A git worktree checked out from a repo. Worktrees are the unit of parallel work — each represents a feature branch, fix, or experiment. Conductor manages their lifecycle:
- Create (branch from a base, install deps)
- List (show status, branch, last activity)
- Delete (prune branch, clean up directory)
- Push / create PR

### Ticket
A normalized work item from GitHub Issues or Jira. Tickets are synced locally and associated with a repo. A ticket can be **linked** to a worktree (this worktree implements this ticket).

### Session
A bounded work session with a start time, set of active worktrees, and optional postmortem. Sessions provide the context boundary for AI orchestration runs.

## Architecture

### v1: Library-First (Embedded)

v1 uses a library-first architecture. All logic lives in a shared `conductor-core` library crate. The CLI and TUI are thin binaries that link against core directly — no daemon, no IPC, no socket.

```
conductor-cli (binary)      conductor-tui (binary)
      |                           |
      +------ conductor-core -----+
              (library crate)
              |       |       |
              v       v       v
          SQLite    git     GitHub/Jira
                  (local)     APIs
```

#### Crate Structure

```
conductor/
  conductor-core/     — library: all domain logic, no IO assumptions
    src/
      lib.rs
      repo.rs         — RepoManager (add, remove, list, detect remote)
      worktree.rs     — WorktreeManager (create, delete, list, status, push)
      tickets.rs      — TicketSyncer (sync, list, link)
      session.rs      — SessionTracker (start, end, notes)
      db/             — schema, migrations, repo queries
      github.rs       — GitHub API client (gh CLI wrapper)
      config.rs       — TOML config loading
      error.rs        — shared error types
  conductor-cli/      — binary: CLI commands, imports core
    src/main.rs
  conductor-tui/      — binary: Ratatui UI, imports core
    src/main.rs
```

Both binaries open the SQLite database directly and call `conductor-core` functions. No network, no IPC. Single process per binary.

#### Why Library-First

- **Simplest possible v1.** No daemon lifecycle, no IPC protocol, no "is the service running?" failure mode.
- **SQLite handles the concurrency that matters.** WAL mode supports concurrent readers with a single writer. The realistic case — TUI open while a CLI one-shot runs — works fine with a busy timeout.
- **Clean extraction path.** The core library's API (`RepoManager::create_worktree(...)`, `TicketSyncer::sync(...)`) can be wrapped in a daemon later without rewriting business logic.
- **Single binary per tool.** No install complexity — `conductor` (CLI) and `conductor-tui` are self-contained.

#### Known Limitations (addressed in v2 daemon)

See [v2: Daemon Extraction](#v2-daemon-extraction) for the full analysis.

- No background ticket sync when neither binary is running
- No cross-process event push (TUI must poll DB for external changes)
- Schema migrations must be coordinated across binary versions
- No shared long-running resources (file watchers, webhook listeners)

### State Storage

SQLite database at `~/.conductor/conductor.db` (or XDG-compliant path). Contains:
- Repo registry
- Cached tickets
- Worktree metadata
- Session history
- User preferences

Opened with WAL mode enabled for concurrent read access.

### TUI

Primary interface. Built with Ratatui + Crossterm. Imports `conductor-core` directly.

**Views:**
1. **Dashboard** — overview of all repos, active worktrees, recent tickets
2. **Repo detail** — worktrees for this repo, recent tickets, branches
3. **Worktree detail** — status, diff summary, linked ticket, actions (push, PR, delete)
4. **Tickets** — filterable list across all repos or scoped to one, with status/assignee/labels
5. **Session** — current session's active worktrees, timeline, postmortem entry

The TUI polls the SQLite database on a short interval (1-2s) to detect changes made by CLI commands in other terminals. This is cheap — SQLite reads from WAL are fast and the query set is small.

### App (future, optional)

A Tauri v2 desktop app or a localhost web UI. Would require the v2 daemon to avoid SQLite contention with the TUI. Not in scope for v1.

## Data Model

### `repos` table
| Column | Type | Description |
|--------|------|-------------|
| id | TEXT (ULID) | Primary key |
| slug | TEXT UNIQUE | Short name |
| local_path | TEXT | Path to main checkout |
| remote_url | TEXT | Git remote origin |
| default_branch | TEXT | e.g., `main` |
| workspace_dir | TEXT | Where worktrees live |
| created_at | TEXT (ISO 8601) | |

### `repo_issue_sources` table
| Column | Type | Description |
|--------|------|-------------|
| id | TEXT (ULID) | Primary key |
| repo_id | TEXT FK | References `repos` |
| source_type | TEXT | `github` or `jira` |
| config_json | TEXT | Source-specific config (see below) |

**GitHub config:**
```json
{
  "owner": "user-or-org",
  "repo": "repo-name"
}
```
Inferred from `remote_url` by default. Uses `gh` CLI for auth.

**Jira config:**
```json
{
  "base_url": "https://company.atlassian.net",
  "project_key": "PROJ",
  "auth": "env:JIRA_API_TOKEN"
}
```

### `worktrees` table
| Column | Type | Description |
|--------|------|-------------|
| id | TEXT (ULID) | Primary key |
| repo_id | TEXT FK | References `repos` |
| slug | TEXT | Worktree name (e.g., `feat-smart-playlists`) |
| branch | TEXT | Full branch name |
| path | TEXT | Filesystem path |
| ticket_id | TEXT FK NULL | Linked ticket |
| status | TEXT | `active`, `merged`, `abandoned` |
| created_at | TEXT | |

### `tickets` table
| Column | Type | Description |
|--------|------|-------------|
| id | TEXT (ULID) | Primary key |
| repo_id | TEXT FK | References `repos` |
| source_type | TEXT | `github` or `jira` |
| source_id | TEXT | Issue number or Jira key |
| title | TEXT | |
| body | TEXT | |
| state | TEXT | `open`, `in_progress`, `closed` |
| labels | TEXT (JSON array) | |
| assignee | TEXT NULL | |
| priority | TEXT NULL | Jira priority or GitHub label-based |
| url | TEXT | Link to the web UI |
| synced_at | TEXT | Last sync timestamp |
| raw_json | TEXT | Full API response for reference |

### `sessions` table
| Column | Type | Description |
|--------|------|-------------|
| id | TEXT (ULID) | Primary key |
| started_at | TEXT | |
| ended_at | TEXT NULL | |
| notes | TEXT NULL | Postmortem / session notes |

### `session_worktrees` table
| Column | Type | Description |
|--------|------|-------------|
| session_id | TEXT FK | |
| worktree_id | TEXT FK | |

## Repo Lifecycle

### Adding a repo

```
conductor repo add <remote-url> [--slug <name>] [--workspace <dir>]
```

1. Clone the repo (or register an existing local checkout)
2. Detect default branch
3. Auto-detect GitHub issue source from remote URL
4. Optionally configure Jira source interactively
5. Create workspace directory (default: `~/.conductor/workspaces/<slug>/`)

### Worktree operations

```
conductor worktree create <repo-slug> <name> [--from <branch>] [--ticket <id>]
conductor worktree list [<repo-slug>]
conductor worktree delete <repo-slug> <name>
conductor worktree push <repo-slug> <name>
conductor worktree pr <repo-slug> <name> [--draft]
```

### Ticket sync

```
conductor tickets sync [<repo-slug>]    # sync one or all repos
conductor tickets list [<repo-slug>]    # list cached tickets
conductor tickets link <ticket-id> <worktree>  # associate ticket with worktree
```

Sync runs on:
- Manual trigger (CLI command or TUI action)
- TUI startup
- TUI background interval (configurable, default: 15 minutes)
- External cron/launchd job running `conductor tickets sync` (for sync without TUI)

## TUI Layout

### Dashboard (default view)

```
Conductor                                          session: 2h 14m
+-- Repos -------------------------+-- Active Worktrees ---------------+
| > orchestra          3 worktrees |  orchestra/feat-smart-playlists   |
|   api-gateway        1 worktree  |    branch: feat/smart-playlists   |
|   shared-lib         0 worktrees |    ticket: #42 Smart playlists    |
|                                  |    status: 3 files changed        |
|                                  |  orchestra/fix-scan-crash         |
|                                  |    branch: fix/scan-crash         |
|                                  |    ticket: #38 Scanner crash      |
|                                  |  api-gateway/feat-rate-limiting   |
|                                  |    branch: feat/rate-limiting     |
+----------------------------------+-----------------------------------+
|  Tickets (12 open)                                                   |
|  #42  orchestra     Smart playlist support          enhancement      |
|  #38  orchestra     Scanner crashes on symlinks     bug              |
|  PROJ-101  api-gw   Rate limiting for /v2 endpoints high priority    |
+----------------------------------------------------------------------+
```

### Keybindings

| Key | Action |
|-----|--------|
| `Tab` / `Shift+Tab` | Cycle focus between panels |
| `j` / `k` | Navigate within panel |
| `Enter` | Drill into selected item |
| `c` | Create (worktree, session, etc. — context-dependent) |
| `d` | Delete (with confirmation) |
| `p` | Push current worktree |
| `P` | Create PR for current worktree |
| `s` | Sync tickets |
| `l` | Link ticket to worktree |
| `/` | Filter / search |
| `?` | Help |
| `q` | Quit |

## Integration Details

### GitHub

**Auth:** Delegates to `gh` CLI (already authenticated). Falls back to `GITHUB_TOKEN` env var.

**Operations:**
- List open issues: `gh issue list --repo <owner/repo> --state open --json ...`
- View issue detail: `gh issue view <num> --repo <owner/repo> --json ...`
- Create PR: `gh pr create --repo <owner/repo> ...`

**Rate limiting:** GitHub API has generous limits for authenticated requests (5000/hr). With 15-min sync intervals and typical repo counts, this is not a concern.

### Jira

**Auth:** API token stored as env var (referenced in config as `env:JIRA_API_TOKEN`). Basic auth with email + token.

**Operations:**
- Search issues: `GET /rest/api/3/search?jql=project=PROJ AND status!=Done`
- View issue: `GET /rest/api/3/issue/{key}`
- Transition issue: `POST /rest/api/3/issue/{key}/transitions` (optional, for status updates)

**Mapping:**
| Jira field | Conductor field |
|------------|-----------------|
| `key` | `source_id` |
| `summary` | `title` |
| `description` (ADF → markdown) | `body` |
| `status.name` | `state` (mapped to open/in_progress/closed) |
| `priority.name` | `priority` |
| `assignee.displayName` | `assignee` |
| `labels` | `labels` |

## AI Orchestration Layer (future)

The current CLAUDE.md-driven workflow (planning subagents, domain-aware implementation agents, code review, postmortems) can run on top of Conductor. The core library provides:

- Repo and worktree management (replaces shell scripts)
- Ticket context (agents can read the linked ticket for a worktree)
- Session tracking (postmortems tied to sessions)

This is out of scope for v1 but the data model supports it. In v1, AI agents can invoke the CLI directly (`conductor worktree create ...`). In v2, agents would connect to the daemon as IPC clients for tighter integration.

## Tech Stack

| Component | Technology | Rationale |
|-----------|-----------|-----------|
| Core library | Rust | Performance, single binary, shared across CLI and TUI |
| CLI | `clap` | Standard Rust CLI framework |
| TUI | Ratatui + Crossterm | Mature Rust TUI framework |
| Database | SQLite (`rusqlite`, WAL mode) | Local-first, zero config, concurrent reads |
| GitHub | `gh` CLI (via `std::process::Command`) | Auth delegation, already installed for most devs |
| Jira | `reqwest` + REST API v3 | Standard HTTP client |
| IDs | ULID (`ulid` crate) | Sortable, no coordination needed |
| Config | TOML (`toml` crate) | `~/.conductor/config.toml` |
| Async | None in v1 | CLI is synchronous, TUI uses blocking calls on background threads. Avoids pulling in tokio for v1. |

## Configuration

`~/.conductor/config.toml`:
```toml
[general]
workspace_root = "~/.conductor/workspaces"
sync_interval_minutes = 15

[defaults]
default_branch = "main"
worktree_prefix_feat = "feat-"
worktree_prefix_fix = "fix-"

# Per-repo overrides via `conductor repo add` are stored in the DB,
# not in this file.
```

## Phasing

### Phase 1: Core library + CLI

- `conductor-core` crate with `RepoManager`, `WorktreeManager`, `TicketSyncer`
- SQLite storage with migrations
- Repo registry (add, remove, list)
- Worktree lifecycle (create, delete, list, status, push, PR)
- GitHub ticket sync
- `conductor-cli` binary wrapping core
- TOML config file

**Goal:** Replace the current shell scripts with a unified tool that works across multiple repos. Validate the data model and core API.

### Phase 2: TUI

- `conductor-tui` crate importing `conductor-core`
- Dashboard, repo detail, worktree detail, ticket list views
- DB polling for cross-process change detection (1-2s interval)
- Background ticket sync on a timer within the TUI process
- Keybindings for all common operations

**Goal:** Visual interface for managing repos, worktrees, and tickets without leaving the terminal.

### Phase 3: Jira integration

- Jira REST API client in `conductor-core`
- Ticket normalization (Jira → Conductor ticket model)
- ADF-to-markdown conversion for Jira descriptions
- Per-repo source configuration

**Goal:** Teams using Jira can pull tickets alongside GitHub issues.

### Phase 4: AI orchestration hooks

- Session management (start, end, postmortem)
- Worktree-to-ticket linking as context for AI agents
- Event hooks for triggering AI workflows (e.g., "ticket assigned → create worktree → plan")
- CLAUDE.md generation per worktree from ticket context

**Goal:** The AI orchestration layer from orchestra-conductor, generalized for any repo.

### Phase 5: Daemon extraction (v2)

See [v2: Daemon Extraction](#v2-daemon-extraction) below.

---

## v2: Daemon Extraction

The library-first architecture is the right choice for v1, but several limitations will surface as usage grows. This section documents the triggers, design, and migration path for extracting a persistent daemon in v2.

### When to Extract

The daemon becomes necessary when any of these are true:

1. **Multiple concurrent clients.** A desktop/web app needs to access the same state as the TUI — two processes doing direct SQLite writes will cause contention beyond what WAL mode handles gracefully.
2. **Real-time reactivity.** You want file watchers (detect new commits), webhook listeners (GitHub/Jira push events), or CI status polling running continuously, not just when the TUI is open.
3. **Background sync is critical.** Cron-based `conductor tickets sync` isn't sufficient — you need sub-minute freshness or want push-based updates.
4. **AI agents as clients.** Orchestration agents need to create worktrees, read tickets, and update session state concurrently with a human using the TUI.

### Specific Issues the Daemon Solves

#### 1. Cross-process state staleness

**Problem:** With library-first, the TUI and CLI are separate processes sharing a SQLite file. If the CLI creates a worktree, the TUI's in-memory state (repo list, worktree counts, active ticket links) is stale until the next DB poll cycle (1-2s). There is no event bus between processes.

**Impact:** Mild in v1 — a 1-2 second delay is acceptable for human use. Becomes problematic when AI agents are rapidly creating/modifying worktrees and need immediate feedback.

**Daemon solution:** All clients connect to the daemon via IPC. State mutations emit events over the connection, so clients update immediately. No polling needed.

#### 2. API surface tension (sync vs async)

**Problem:** The `conductor-core` API must serve two different calling patterns:
- **CLI:** Synchronous, one-shot. `create_worktree()` runs and exits.
- **TUI:** Long-lived process that needs non-blocking operations. Ticket sync (network IO) or repo clone (minutes) can't block the UI thread.

In v1, the TUI handles this by running blocking core calls on background threads (`std::thread::spawn`) and communicating back via channels. This works but leads to duplicated concurrency plumbing across every TUI action.

**Impact:** Manageable in v1 with a handful of operations. Gets unwieldy as the operation count grows.

**Daemon solution:** The daemon runs an async runtime (tokio). Clients send requests over IPC and receive responses/events asynchronously. The TUI becomes a thin event-driven UI with no threading logic of its own.

#### 3. Schema migration coordination

**Problem:** `conductor-core` owns the SQLite schema. Both the CLI and TUI run migrations on startup. If a user has CLI v1.2 and TUI v1.3, whichever runs first migrates the DB, potentially breaking the older binary.

**Impact:** Low risk for a single developer keeping both tools updated. Real risk if distributed to a team or if binaries are updated independently (e.g., TUI installed via cargo, CLI via homebrew).

**Daemon solution:** The daemon is the single owner of the database. Only it runs migrations. Clients declare a minimum protocol version; the daemon rejects clients that are too old. Schema and protocol versioning are decoupled.

#### 4. No shared long-running resources

**Problem:** Features like these require a persistent process:
- **File watchers** — detect new commits or branch changes in worktrees without polling git
- **Webhook listeners** — receive push events from GitHub/Jira for instant ticket updates
- **CI status polling** — watch for PR checks completing
- **Persistent connections** — Jira streaming API, GitHub webhook server

Each binary would have to set these up independently. The TUI's watchers die when you close it.

**Impact:** Not needed in v1 (on-demand sync is sufficient). Becomes the main motivator for a daemon when real-time features are prioritized.

**Daemon solution:** The daemon owns all long-running resources. They run continuously regardless of which clients are connected.

#### 5. Serialization overhead for daemon transition

**Problem:** Extracting the daemon requires:
- Adding `Serialize`/`Deserialize` to every type in `conductor-core` that crosses the IPC boundary
- Designing an IPC protocol (JSON-RPC over Unix domain socket is the likely choice)
- Error types need wire-friendly representations
- The TUI needs a second code path: direct core calls (embedded mode) vs. service client (daemon mode)

**Impact:** This is a real refactoring cost. It's not a rewrite — the core logic stays the same — but the plumbing takes effort. Estimated at 1-2 weeks of focused work.

**Mitigation:** v1 core types should derive `Serialize`/`Deserialize` from the start (cheap to add, zero runtime cost if unused). This removes the biggest mechanical blocker from the daemon extraction.

### Daemon Architecture (v2)

```
conductor-cli ----+
                  |     JSON-RPC over Unix domain socket
conductor-tui ----+---> conductor-service (daemon)
                  |         |
future app -------+         +-- conductor-core (library)
                            |       |       |
                            v       v       v
                        SQLite    git     APIs
```

**New crate:** `conductor-service`
- Imports `conductor-core`
- Runs a tokio async runtime
- Listens on `~/.conductor/conductor.sock`
- JSON-RPC protocol for requests, server-sent events for push notifications
- Manages daemon lifecycle (PID file, graceful shutdown, auto-restart)

**TUI changes:**
- Adds a `ServiceClient` that speaks JSON-RPC
- On startup: check for running daemon. If found, connect. If not, either start one or fall back to embedded mode.
- Embedded mode preserved as a fallback — the TUI always works standalone.

### Preparing for v2 in v1

These low-cost decisions in v1 make the daemon extraction easier later:

1. **Derive `Serialize`/`Deserialize` on all core types** — even though v1 doesn't need it
2. **Keep `conductor-core` free of IO assumptions** — no `println!`, no terminal access, no assumptions about blocking vs async
3. **Return `Result<T, ConductorError>` everywhere** — error types that map cleanly to wire errors
4. **Use opaque IDs (ULIDs)** — no in-memory pointers or process-local state leaking into the API

## Open Questions

1. **Workspace layout:** Should each repo get its own subdirectory under a root workspace, or should the user choose arbitrary paths? Recommendation: default to `~/.conductor/workspaces/<slug>/` but allow overrides.

2. **Dep installation:** The current `create-worktree.sh` runs `npm install`. Should Conductor detect the package manager and run install automatically? Recommendation: yes, detect `package.json` → npm/yarn/pnpm/bun, `Cargo.toml` → skip (cargo builds on demand), `go.mod` → skip.

3. **Ticket write-back:** Should Conductor update ticket status in GitHub/Jira when a worktree is merged? Recommendation: Phase 4 — close-on-merge is useful but not essential early.

4. **Multi-remote support:** Some repos have multiple remotes (fork + upstream). How should Conductor handle this? Recommendation: v1 tracks origin only, revisit if needed.

5. **Project name:** "Conductor" is the working title. It fits the orchestration metaphor but is a common name. Alternatives: `forge`, `workbench`, `loom`, `dispatch`.
