# Development Guide

## One-time setup

```bash
# Enable git hooks (pre-commit fmt check + pre-push E2E tests)
git config core.hooksPath .githooks
```

## Build & test

```bash
cargo build                     # Build all crates
cargo build --release           # Release build
cargo build --bin conductor     # Build CLI only
cargo build --bin conductor-tui # Build TUI only
cargo build --bin conductor-web # Build web UI (requires frontend built first)

# Web frontend
cd conductor-web/frontend && bun install && bun run build

cargo test                      # Run all tests
cargo test --lib github         # Run specific module tests
cargo test -p conductor-core    # Test a single crate

cargo clippy -- -D warnings     # Lint (CI enforces -D warnings)
cargo fmt --all                 # Auto-format
cargo fmt --all --check         # Check formatting (CI gate)
```

## E2E tests

Playwright E2E tests run automatically on push when `conductor-web` files change.

```bash
# Skip on push
SKIP_E2E=1 git push

# Run manually
cd conductor-web/frontend && bun run test:e2e
```

## Shared cargo target directory

All git worktrees share the same `target/` directory (Cargo resolves it relative to the workspace `Cargo.toml`). This has tradeoffs:

**Benefits**
- Incremental compilation is shared — switching between worktrees reuses cached artifacts.
- Disk usage stays low — no duplicate `target/` per worktree.

**Drawbacks**
- Concurrent builds across worktrees can race on the same binary. If two worktrees build `conductor` simultaneously, one may read a partially-written binary and fail with `No such file or directory`.
- This shows up most visibly in workflow runs: a review or lint step that spawns `conductor headless` can fail if another worktree's build is in progress at the same moment. Resuming the failed run after the build finishes resolves it.

**Per-worktree isolation (opt-in)**

If you need fully isolated builds (e.g. long-running parallel CI), set `CARGO_TARGET_DIR` to a worktree-specific path:

```bash
export CARGO_TARGET_DIR=~/.conductor/workspaces/conductor-ai/<worktree-slug>/target
```

This eliminates races but loses incremental compilation sharing and uses significantly more disk.

## Tracing SQLite write-lock holders

Set `CONDUCTOR_DB_TRACE=<absolute log path>` in every process you want to observe
(CLI, TUI, web, and the session hook's environment). Each process appends one
tab-separated line per slow statement (`stmt`) or long transaction (`tx`):
`timestamp  pid  process  thread  kind  ms  sql`. `CONDUCTOR_DB_TRACE_MS` sets the
threshold in ms (default 20). With the variable unset nothing is installed.

```bash
export CONDUCTOR_DB_TRACE=/tmp/conductor-db-trace.log
# run a normal TUI + web + workflow session, then:
sort -t$'\t' -k6 -nr /tmp/conductor-db-trace.log | head -30
```

`tx` lines run from `BEGIN` to `COMMIT`/`ROLLBACK` and show the first statement inside,
so they name the code path holding the lock. `BEGIN DEFERRED` transactions only take the
write lock at their first write, so their `tx` lines over-approximate the hold.
`CONDUCTOR_SESSION_HOOK_DEBUG=1` also prints the hook's open / record / close timings.

### Findings so far (#3189)

- The startup migration check takes no write lock on an up-to-date database (a test keeps
  it that way).
- A hook that is the only open connection checkpointed the whole WAL on close under an
  exclusive lock, so a concurrent hook got `SQLITE_BUSY` and spooled. The hook connection
  now sets `SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE`.
- Other possible holders, to be confirmed with a trace: the explicit transactions in
  `tickets/syncer.rs` and `workflow/manager/lifecycle.rs`, and large autocommit deletes
  that cascade into `agent_run_events`.
