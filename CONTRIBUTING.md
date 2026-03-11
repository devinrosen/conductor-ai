# Contributing to Conductor

## The spirit

Conductor exists to make AI-assisted development repeatable and observable. The best way to contribute is to use it — run the `ticket-to-pr` workflow on an open issue and let the tool do what it was built to do.

---

## Quick start

### 1. Prerequisites

- Rust stable toolchain
- [GitHub CLI (`gh`)](https://cli.github.com/) — installed and authenticated
- [tmux](https://github.com/tmux/tmux)
- [Node.js](https://nodejs.org/) (only needed if touching the web UI)
- Claude Code — `npm install -g @anthropic-ai/claude-code`

### 2. Clone and build

```bash
git clone git@github.com:devinrosen/conductor-ai.git
cd conductor-ai
git config core.hooksPath .githooks   # enforces cargo fmt on commit
./build.sh
```

### 3. Enable the pre-commit hook

```bash
git config core.hooksPath .githooks
```

This runs `cargo fmt --all --check` before every commit. CI enforces the same check, so enabling it locally saves a round-trip.

---

## Finding work

- Browse [open issues](https://github.com/devinrosen/conductor-ai/issues)
- Issues labeled `good first issue` are a reasonable starting point
- Leave a comment on the issue before starting so others know it's in progress
- If you have a bug fix or idea not yet tracked, open an issue first

---

## The preferred workflow: ticket-to-pr

If you have conductor set up, use it to work on conductor.

```bash
# Register the repo (one-time)
conductor repo add git@github.com:devinrosen/conductor-ai.git

# Sync issues from GitHub
conductor tickets sync conductor-ai

# In the TUI: navigate to the ticket, press w, select ticket-to-pr
conductor-tui
```

The `ticket-to-pr` workflow will:
1. Read the ticket and write a `PLAN.md`
2. Implement the plan in a worktree branch
3. Push and open a PR
4. Run the `review-pr` swarm and iterate until clean

You can also run it from the CLI:

```bash
conductor workflow run ticket-to-pr --input ticket_id=<id>
```

---

## Manual workflow

If you're not yet set up with conductor:

```bash
git checkout -b feat/<issue-number>-short-description
# make changes
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git push -u origin HEAD
gh pr create --fill
```

---

## CI requirements

Every PR to `main` must pass:

| Check | Command |
|---|---|
| Format | `cargo fmt --all --check` |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` |
| Tests | `cargo test --workspace` |

`main` requires PRs, linear history (squash or rebase only — no merge commits), and all checks green.

---

## Code conventions

- All record IDs are ULIDs
- All timestamps are ISO 8601 strings
- Domain logic belongs in `conductor-core`; binaries are thin wrappers
- Custom `ConductorError` + `Result<T>` in core; `anyhow::Result` in binaries
- No `unwrap()` in library code — propagate errors
- Tests live alongside the code they test (`#[cfg(test)]` modules)

---

## PR review

PRs are reviewed by the `review-pr` swarm (architecture, security, performance, error handling, test coverage, dry abstraction, DB migrations). Reviewers post a single aggregated comment. Blocking findings must be addressed before merge.

You can run the swarm yourself before pushing:

```bash
conductor workflow run review-pr --input pr_url=<your-pr-url>
```
