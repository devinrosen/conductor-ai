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
cargo fmt --all                # Auto-format
cargo fmt --all --check        # Check formatting (CI gate)
cargo build --bin conductor     # Build CLI only
cargo build --bin conductor-tui # Build TUI only
cargo build --bin conductor-web # Build web UI (requires frontend built first)

# Web frontend (must be built before cargo build --bin conductor-web)
cd conductor-web/frontend && bun install && bun run build
```

No Makefile/justfile — use `cargo` directly.

# One-time dev setup: enable pre-commit hook that runs cargo fmt --all --check
git config core.hooksPath .githooks

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

SQLite at `~/.conductor/conductor.db` with WAL mode, foreign keys on, 5s busy timeout. Schema managed via versioned migrations in `conductor-core/src/db/migrations/`. Tables: `repos`, `repo_issue_sources`, `worktrees`, `tickets`, `agent_runs`, `workflow_runs`, `workflow_run_steps`, `_conductor_meta`.

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

## Project Context

- **Vision & motivation:** [docs/VISION.md](docs/VISION.md)
- **Current priorities:** [docs/ROADMAP.md](docs/ROADMAP.md)
- **Workflow engine design:** [docs/workflow/engine.md](docs/workflow/engine.md)

## TUI Threading Rule

**Never call blocking operations on the TUI main thread.** The TUI renders on a single thread — any synchronous blocking call (git, network, file I/O, subprocess) freezes the UI completely.

**What counts as blocking:** anything in `conductor-core` that calls `std::process::Command` (all git ops, `gh` CLI, dep installs), large file reads (agent logs), or slow DB queries.

**The required pattern:**

```rust
// 1. Capture data needed by the thread
let tx = self.bg_tx.clone();
let repo_slug = repo_slug.clone();

// 2. Show a non-dismissable progress modal
self.state.modal = Modal::Progress { message: "Pushing branch…".into() };

// 3. Do the work off-thread
std::thread::spawn(move || {
    let db = open_database(&db_path()).unwrap();
    let config = Config::load().unwrap();
    let result = WorktreeManager::new(&db, &config).push(&repo_slug, &wt_slug);
    let _ = tx.send(Action::PushComplete { result: result.map_err(|e| e.to_string()) });
});

// 4. Handle the result action back on the main thread
Action::PushComplete { result } => {
    self.state.modal = Modal::None;
    match result {
        Ok(msg) => self.state.status_message = Some(msg),
        Err(e)  => self.state.modal = Modal::Error { message: e },
    }
}
```

Reference implementations already using this pattern correctly:
- `has_merged_pr()` check before worktree delete (`app.rs` ~line 2827)
- Workflow execution and resume (`app.rs` ~lines 4893, 1705)
- PR fetch background task (`background.rs`)

## Worktree Workflow (REQUIRED)

**Always create a conductor worktree before starting any fix or feature.** Never make changes directly on `main` or in the primary working directory.

```bash
# Create a worktree (branch auto-normalizes: feat- → feat/, fix- → fix/)
cargo run --bin conductor -- worktree create conductor-ai <name>
# e.g. cargo run --bin conductor -- worktree create conductor-ai fix-800-snapshot-crash
#      cargo run --bin conductor -- worktree create conductor-ai feat-801-new-thing

# Worktree lands at:
~/.conductor/workspaces/conductor-ai/<name>/
```

Do all work — edits, builds, tests, commits — inside the worktree directory. Push and create the PR from there.

```bash
cd ~/.conductor/workspaces/conductor-ai/<name>
# ... make changes, run cargo test, cargo fmt --all ...
git add <files> && git commit -m "..."
git push -u origin <branch>
gh pr create ...
```

## Key Conventions

- All record IDs are ULIDs (sortable, collision-resistant)
- All timestamps are ISO 8601 strings
- Worktree branch naming auto-detects `feat-`/`fix-` prefix and normalizes to `feat/`/`fix/` branches
- JS dep auto-install detects package manager via lockfile: bun > pnpm > yarn > npm
- Ticket upserts use `ON CONFLICT DO UPDATE` on `(repo_id, source_type, source_id)` for idempotency
