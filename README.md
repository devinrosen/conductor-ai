# Conductor

A local-first orchestration tool for managing multiple git repos, worktrees, tickets, and AI agent runs — all backed by SQLite.

## Prerequisites

- [Rust](https://rustup.rs/) (stable toolchain)
- [Node.js](https://nodejs.org/) (for the web UI frontend)
- [GitHub CLI (`gh`)](https://cli.github.com/) — installed and authenticated (for GitHub issue sync)
- [tmux](https://github.com/tmux/tmux) (for AI agent sessions)

## Build

```bash
cargo build                     # Build all crates
cargo build --release           # Release build
cargo test                      # Run all tests
cargo clippy -- -D warnings     # Lint
```

The web UI requires building the frontend first:

```bash
cd conductor-web/frontend && npm install && npm run build
cargo build --bin conductor-web
```

## Usage

### CLI

```bash
conductor repo add <remote-url>           # Register a repo
conductor repo list                       # List registered repos
conductor worktree create <repo> <name>   # Create a worktree
conductor tickets sync <repo>             # Sync tickets from GitHub/Jira
```

### TUI

```bash
conductor-tui
```

Interactive terminal UI for browsing repos, worktrees, and tickets. Supports launching Claude agent sessions in tmux.

### Web UI

```bash
conductor-web
```

Opens a local web server with a React-based dashboard.

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
