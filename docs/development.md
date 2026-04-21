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
