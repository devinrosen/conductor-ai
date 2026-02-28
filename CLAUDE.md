# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

```bash
cargo build                    # Build all crates
cargo build --release          # Release build
cargo test                     # Run all tests
cargo test --lib github        # Run specific module tests (e.g., github)
cargo test -p conductor-core   # Test a single crate
cargo clippy -- -D warnings    # Lint (CI enforces -D warnings)
cargo fmt --check              # Check formatting
cargo build --bin conductor     # Build CLI only
cargo build --bin conductor-tui # Build TUI only
cargo build --bin conductor-web # Build web UI (requires frontend built first)

# Web frontend (must be built before cargo build --bin conductor-web)
cd conductor-web/frontend && npm install && npm run build
```

No Makefile/justfile — use `cargo` directly.

## Architecture

**Conductor** is a multi-repo orchestration tool: manages git repos, worktrees, tickets, and AI agent runs locally with SQLite.

### Workspace Layout

Four crates in a Cargo workspace:

- **conductor-core** — Library crate with all domain logic. Everything lives here.
- **conductor-cli** — Thin binary wrapping core with clap subcommands.
- **conductor-tui** — TUI binary using ratatui + crossterm.
- **conductor-web** — Web UI binary using axum + React (Vite + Tailwind frontend embedded via `rust_embed`).

### Library-First (v1)

No daemon, no IPC, no async runtime. CLI and TUI import `conductor-core` directly. SQLite WAL mode handles concurrency. Designed for future daemon extraction in v2 (all domain structs already derive `Serialize`/`Deserialize`).

### Manager Pattern

Domain logic is organized into manager structs that take `&Connection` + `&Config`:
- `RepoManager` — CRUD for registered repos
- `WorktreeManager` — Git worktree lifecycle (branch, create worktree, auto-install JS deps)
- `TicketSyncer` — Upsert/list tickets, link to worktrees
- `IssueSourceManager` — Configure per-repo issue sources (GitHub, Jira)
- `AgentManager` — Launch/stop Claude agents in tmux, track runs and events

### Error Handling

- `conductor-core`: Custom `ConductorError` enum via `thiserror`, with `Result<T>` type alias
- Binaries: `anyhow::Result` for one-shot error reporting

### Git & External Tools

All git operations and GitHub sync use `std::process::Command` (synchronous subprocess calls):
- Worktree ops: `git branch`, `git worktree add/remove`
- GitHub sync: `gh issue list` (requires `gh` CLI installed and authed)

### Database

SQLite at `~/.conductor/conductor.db` with WAL mode, foreign keys on, 5s busy timeout. Schema managed via versioned migrations in `conductor-core/src/db/migrations/`. Tables: `repos`, `repo_issue_sources`, `worktrees`, `tickets`, `agent_runs`, `sessions`, `session_worktrees`, `_conductor_meta`. (Sessions are slated for removal — see #76.)

### Data Directory

```
~/.conductor/
├── conductor.db
├── config.toml
└── workspaces/<repo-slug>/<worktree-slug>/
```

## CI & Branch Rules

GitHub Actions runs on PRs to `main` (`.github/workflows/ci.yml`):
- **Format** — `cargo fmt --all --check`
- **Clippy** — `cargo clippy --workspace --all-targets -- -D warnings`
- **Test** — `cargo test --workspace`

Branch ruleset on `main`: PRs required, linear history (squash/rebase only), `Clippy` + `Test` must pass. Tag ruleset: `v*` tags cannot be deleted or overwritten.

## Project Status (per docs/SPEC.md)

- Phase 1 (done): Core library + CLI
- Phase 2 (done): TUI with ratatui
- Phase 3: Jira integration
- Phase 4 (in progress): AI agent orchestration (agent launch/stop, runs, events)
- Phase 5: Daemon extraction (v2, async with tokio)
- Phase 6: Web UI (in progress — basic CRUD done, agent features missing)

## Key Conventions

- All record IDs are ULIDs (sortable, collision-resistant)
- All timestamps are ISO 8601 strings
- Worktree branch naming auto-detects `feat-`/`fix-` prefix and normalizes to `feat/`/`fix/` branches
- JS dep auto-install detects package manager via lockfile: bun > pnpm > yarn > npm
- Ticket upserts use `ON CONFLICT DO UPDATE` on `(repo_id, source_type, source_id)` for idempotency
