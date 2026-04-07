# Conductor

A local-first orchestration tool for managing multiple git repos, worktrees, tickets, and AI agent runs — all backed by SQLite.

## What is Conductor?

Conductor is a local tool for managing AI-assisted development across multiple git worktrees. If you're already using Claude to write and review code, Conductor is the layer that keeps everything organized when you're running multiple agents in parallel.

**The problem it solves:**

When you let an agent work on a branch, you don't want to sit and watch it. You want to kick off the work and move on to something else. But the moment you have two or three agents running at once — each on its own worktree — the overhead of managing them adds up fast: tracking which terminal has which branch, knowing when something needs your attention, keeping your ticket tracker in sync with what's actually happening in git.

Conductor handles that overhead. It gives you one place to see all your repos, worktrees, and in-flight work, and a workflow system for defining how agents should handle common tasks (fix-ci, review-pr, iterate-pr, etc.) so you're not copy-pasting prompts.

**Key things it does:**
- Manages git worktrees for you — create, push, PR, delete — with branch naming handled automatically
- Runs agent workflows (pre-defined sequences of Claude tasks) against a worktree, PR, or ticket with a single keypress
- Syncs GitHub issues so your tickets live next to your code, not in a separate browser tab
- Lets multiple workflows run in parallel without you babysitting them

**Interfaces:** The primary interface is a TUI (terminal UI), with a CLI for scripting. There's also a web app and a Mac app — both are usable today but still being refined, so if you're not a TUI person they're worth trying.

**What it is:** Local-first, no cloud, no account. Runs on your machine, your Claude API key stays yours.

## Prerequisites

- [Rust](https://rustup.rs/) (stable toolchain)
- [Node.js](https://nodejs.org/) (for the web UI frontend)
- [GitHub CLI (`gh`)](https://cli.github.com/) — installed and authenticated (for GitHub issue sync)
- [tmux](https://github.com/tmux/tmux) (for AI agent sessions)
- [Claude Code CLI (`claude`)](https://docs.anthropic.com/en/docs/claude-code) — installed and authenticated

## Build

The recommended way to build everything (frontend + all Rust crates):

```bash
./build.sh
```

This installs the web frontend dependencies, builds the React bundle, then runs `cargo build --workspace`. Run it after pulling `main` or setting up a new worktree.

Individual commands for day-to-day development:

```bash
cargo build                                              # Build all crates (without frontend)
cargo build --release                                    # Release build
cargo test                                               # Run all tests
cargo clippy --workspace --all-targets -- -D warnings    # Lint (CI enforces -D warnings)
cargo fmt --all                                          # Auto-format
```

## Install

After running `./build.sh`, install the binaries to `~/.cargo/bin`:

```bash
cargo install --path conductor-cli
cargo install --path conductor-tui
cargo install --path conductor-web   # requires frontend already built by ./build.sh
```

During development you can skip the install step and use `cargo run --bin <name>` instead (see examples below).

## Usage

### CLI

```bash
# After install
conductor repo add <remote-url>           # Register a repo
conductor repo list                       # List registered repos
conductor worktree create <repo> <name>   # Create a worktree
conductor tickets sync <repo>             # Sync tickets from GitHub/Jira

# Without installing
cargo run --bin conductor -- repo list
```

### TUI

Interactive terminal UI for browsing repos, worktrees, and tickets. Supports launching Claude agent sessions in tmux.

```bash
conductor-tui                        # After install
cargo run --bin conductor-tui        # Without installing
```

### Web UI

Opens a local web server with a React-based dashboard.

```bash
conductor-web                        # After install
cargo run --bin conductor-web        # Without installing
```

### Desktop (macOS)

Native Mac app powered by Tauri, embedding the same web UI. Still being refined but usable today.

```bash
# Dev mode — builds Rust crates, starts Vite dev server, launches the app
bun run desktop

# Production build — outputs Conductor.app
bun run desktop:build
# App bundle: target/release/bundle/macos/Conductor.app
```

Other convenience scripts (run from the workspace root):

```bash
bun run web          # Start the standalone web server (cargo run --bin conductor-web)
bun run cli:build    # Rebuild just the CLI binary
bun run build        # Full build of all Rust crates + frontend
```

## Workflows

Conductor includes a workflow engine that orchestrates multi-step AI agent pipelines. Workflows are defined in `.wf` files using a minimal custom DSL.

```bash
conductor workflow list                              # list available workflows
conductor workflow show <name>                       # ASCII step graph
conductor workflow validate <name>                   # check agents, inputs, cycles, snippets
conductor workflow run <name> [--input k=v] [--dry-run]
conductor workflow cancel <run-id>
conductor workflow runs [--worktree id]              # run history
conductor workflow run-show <run-id>                 # per-step detail
conductor workflow gate-approve  <run-id>
conductor workflow gate-reject   <run-id>
conductor workflow gate-feedback <run-id> "<text>"
```

Workflow files live in `.conductor/workflows/<name>.wf`. Agent prompts live in `.conductor/agents/<name>.md`. The DSL supports sequential steps, `parallel` blocks, `if`/`unless`/`while`/`do-while` control flow, `gate` steps for human or automated approvals, `always` cleanup blocks, and `call workflow` for shallow composition.

For full details on the DSL grammar, constructs, structured output, and design tradeoffs, see [docs/workflow/engine.md](docs/workflow/engine.md).

## Architecture

Four crates in a Cargo workspace:

| Crate | Role |
|---|---|
| **conductor-core** | Library with all domain logic (repos, worktrees, tickets, agents, DB) |
| **conductor-cli** | Thin CLI binary using clap |
| **conductor-tui** | Terminal UI using ratatui + crossterm |
| **conductor-web** | Web UI using axum + React (Vite + Tailwind, embedded via `rust_embed`) |

Data lives in `~/.conductor/` — a single SQLite database and per-repo worktree directories. No daemon or background process; the CLI and TUI link directly against `conductor-core`.

## License

MIT
