# Conductor

A local-first orchestration tool for managing multiple git repos, worktrees, tickets, and AI agent runs — all backed by SQLite.

## Prerequisites

- [Rust](https://rustup.rs/) (stable toolchain)
- [Node.js](https://nodejs.org/) (for the web UI frontend)
- [GitHub CLI (`gh`)](https://cli.github.com/) — installed and authenticated (for GitHub issue sync)
- [tmux](https://github.com/tmux/tmux) (for AI agent sessions)

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

## PR Review Swarm

Conductor can run a multi-agent review swarm against a PR — spawning one AI reviewer per role and aggregating the results into a single PR comment.

### Reviewer roles

Each reviewer is a Markdown file in `.conductor/reviewers/` at the repo root. The filename (minus `.md`) is used as the role name if no `name` is set in the frontmatter.

**File format** — YAML frontmatter + Markdown body:

```markdown
---
name: security
description: Input validation, auth gaps, injection risks, secrets in code
model: opus
required: true
---

You are a security-focused code reviewer working on a Rust CLI tool.
Focus exclusively on:
- Command injection risks in subprocess calls
- Path traversal in file system operations
- Authentication and authorization issues
...
```

**Frontmatter fields:**

| Field | Required | Default | Description |
|---|---|---|---|
| `name` | no | filename stem | Short identifier used in output and PR comments |
| `description` | no | filename stem | Human-readable focus area shown in the PR comment |
| `model` | no | — | Claude model to use (e.g. `opus`, `sonnet`) |
| `required` | no | `true` | If `true`, blocking findings from this reviewer prevent auto-merge |

The Markdown body becomes the reviewer's system prompt.

### Swarm settings

Create `.conductor/review.toml` in the repo root to control swarm-level behavior:

```toml
# Post an aggregated review comment to the GitHub PR (default: true).
post_to_pr = true

# Auto-enqueue for merge when all required reviewers approve (default: true).
auto_merge = true
```

Both settings default to `true` — the file is optional.

### Lookup order

When a review runs, Conductor looks for `.conductor/reviewers/` in the PR branch worktree first, then falls back to the main repo checkout. This lets you develop and test new reviewer roles in a branch before merging them. See the trust model note in `conductor-core/src/review_config.rs` if your repo has untrusted external contributors.

## Architecture

Four crates in a Cargo workspace:

| Crate | Role |
|---|---|
| **conductor-core** | Library with all domain logic (repos, worktrees, tickets, agents, DB) |
| **conductor-cli** | Thin CLI binary using clap |
| **conductor-tui** | Terminal UI using ratatui + crossterm |
| **conductor-web** | Web UI using axum + React (Vite + Tailwind, embedded via `rust_embed`) |

Data lives in `~/.conductor/` — a single SQLite database and per-repo worktree directories. No daemon or background process; the CLI and TUI link directly against `conductor-core`.

## Contributing

One-time setup after cloning — enables a pre-commit hook that enforces formatting:

```bash
git config core.hooksPath .githooks
```

CI runs format, clippy, and tests on every PR to `main`. Squash or rebase merges only (no merge commits).

## License

MIT
