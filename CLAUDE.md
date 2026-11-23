# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

```bash
cargo build                    # Build all crates
cargo build --release          # Release build

# Progressive testing — only test what you touched (much faster dev loop)
cargo test -p conductor-core          # touched core logic
cargo test -p conductor-tui           # touched TUI
cargo test -p conductor-web           # touched web backend
cargo test -p conductor-core --lib agent  # specific module within a crate
cargo test -p conductor-core <test_name>  # single test by name

# Full suite — run before pushing (matches CI)
cargo test --workspace

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

# One-time dev setup: enable git hooks (pre-commit fmt check + pre-push E2E tests)
git config core.hooksPath .githooks

# Playwright E2E tests run automatically on push when conductor-web files change.
# To skip: SKIP_E2E=1 git push
# To run manually: cd conductor-web/frontend && bun run test:e2e

### Cross-worktree build cache (recommended)

Each conductor worktree gets its own `target/` (Cargo default), so parallel
worktrees don't fight over a build lock — but they redundantly recompile the
same crates. Wire up `sccache` once per machine to share compiled artifacts
across worktrees while keeping per-worktree `target/` for linking:

```bash
brew install sccache

# Make it apply to every cargo invocation, in every shell, without polluting env.
mkdir -p ~/.cargo
cat >> ~/.cargo/config.toml <<'EOF'
[build]
rustc-wrapper = "sccache"
incremental = false
EOF

sccache --show-stats   # after a cargo clean + build, "Cache misses" > 0
```

`incremental = false` is required: sccache classifies incremental builds as
non-cacheable (the incremental cache state isn't deterministic), so without
this every invocation shows up as `Non-cacheable calls` and nothing is stored.
Cost is small — the cross-worktree cache more than pays for the lost
intra-worktree incremental speedup.

The first worktree pays full compile cost; subsequent worktrees pull unchanged
crates from cache. `target/` still grows per-worktree (linker output) and is
cleaned up automatically when conductor deletes the worktree.

## Architecture

**Conductor** is a multi-repo orchestration tool: manages git repos, worktrees, tickets, and AI agent runs locally with SQLite.

### Workspace Layout

Conductor crates in a Cargo workspace (the `runkon-*` engine crates are now consumed from crates.io and live in their own [runkon/runkon](https://github.com/runkon/runkon) repo):

- **conductor-core** — Library crate with all domain logic. Everything lives here.
- **conductor-cli** — Thin binary wrapping core with clap subcommands.
- **conductor-tui** — TUI binary using ratatui + crossterm.
- **conductor-web** — Web UI binary using axum + React (Vite + Tailwind frontend embedded via `rust_embed`).
- **conductor-desktop** — Native macOS desktop app using Tauri v2 (wraps conductor-web).

### Library-First (v1)

No daemon, no IPC, no async runtime. CLI and TUI import `conductor-core` directly. SQLite WAL mode handles concurrency. Designed for future daemon extraction in v2 (all domain structs already derive `Serialize`/`Deserialize`).

### Manager Pattern

Domain logic is organized into manager structs that take `&Connection` + `&Config`:
- `RepoManager` — CRUD for registered repos
- `WorktreeManager` — Git worktree lifecycle (branch, create worktree, auto-install JS deps)
- `TicketSyncer` — Upsert/list tickets, link to worktrees
- `IssueSourceManager` — Configure per-repo issue sources (GitHub, Jira)
- `AgentManager` — Launch/stop Claude agents via PID-based headless subprocess, track runs and events

### Error Handling

- `conductor-core`: Custom `ConductorError` enum via `thiserror`, with `Result<T>` type alias
- Binaries: `anyhow::Result` for one-shot error reporting

### Git & External Tools

All git operations and GitHub sync use `std::process::Command` (synchronous subprocess calls):
- Worktree ops: `git branch`, `git worktree add/remove`
- GitHub sync: `gh issue list` (requires `gh` CLI installed and authed)

### Database

SQLite at `~/.conductor/conductor.db` with WAL mode, foreign keys on, 5s busy timeout. Schema managed via versioned migrations in `conductor-core/src/db/migrations/`. Key tables by domain:
- **Repos/worktrees:** `repos`, `repo_issue_sources`, `worktrees`
- **Tickets:** `tickets`, `ticket_labels`, `ticket_dependencies`
- **Agent runs:** `agent_runs`, `agent_run_events`, `agent_created_issues`, `conversations`
- **Workflows:** `workflow_runs`, `workflow_run_steps`, `workflow_run_step_fan_out_items`
- **Misc:** `notification_log`, `push_subscriptions`

**Schema compatibility:** a binary refuses to open a database whose schema version is newer than it knows. So the first binary built from a branch with a new migration that opens `~/.conductor/conductor.db` migrates it and locks out every older installed binary: the MCP server, TUI, web and the `conductor` that workflow script steps call. Upgrade them together (see **Releases and milestones**), and never run a new-migration branch's build against the real home.

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
- **Pipeline Scripts** — `.conductor/tests/run.sh`, fixture tests for `.conductor/scripts/*`; also covers `scripts/release/bump.sh`; runs when `.conductor/scripts/**`, `.conductor/tests/**` or `scripts/release/**` changes and reports success when skipped

Branch ruleset on `main`: PRs required, linear history (squash/rebase only), `Clippy`, `Test` and `Validate Workflows` must pass. Branch ruleset on `release/**`: `Clippy`, `Test` and `Validate Workflows` must pass to merge a PR, no deletion or force push; repo admins may bypass, because the branch's creation and its `release:` bump commit are pushed directly. `Validate Workflows` runs `conductor workflow validate --all --templates` and reports success when its path filter skips it. Tag ruleset: `v*` tags cannot be deleted or overwritten.

### Tests never touch the real conductor home

Every test binary sets `CONDUCTOR_HOME` to its own temp dir before `main` runs, through `conductor_core::isolate_test_home!()`, and under nextest `conductor_dir()` panics when `CONDUCTOR_HOME` is unset. A **new** test binary (a new `tests/*.rs` file or a new crate) must invoke `conductor_core::isolate_test_home!();` at the top level, which needs conductor-core's `test-helpers` feature, or it fails CI. Code that spawns a thread which opens its own connection must pass the caller's database path down (`db::connection_db_path(conn)`, `Conductor::conn_db_path()`), never fall back to `config::db_path()`: before this was enforced, such threads made `cargo test` migrate the developer's real database (#3171, #3175).

Test databases come from `test_helpers::create_test_conn()` (in-memory) and `test_helpers::open_test_db_file(path)` (a fresh temp file). Both copy a per-build migrated template kept in the system temp dir (never under `CONDUCTOR_HOME`, so nextest's per-test processes share it) instead of running every migration. A test of the migrations themselves calls `migrations::run` on a fresh connection.

Tests that spawn the `conductor` binary build the command with `conductor_cmd()` from `conductor-cli/tests/common/mod.rs`. It clears the environment and pins `PATH` to `/usr/bin:/bin`, so output doesn't depend on which tools (`gh`, `tmux`, `claude`) or variables (`TMUX`, `CLAUDE_PID`, `CONDUCTOR_DB_PATH`) the developer has. A test that needs a variable sets it explicitly. Without this, #3235 passed locally and failed only in CI.

## Releases and milestones

Each release has a GitHub milestone and a `release/X.Y.Z` branch cut from `main`.

- **Per-issue PRs** target the release branch and are squash-merged, one issue per PR, with a written commit message (not the default list of commits). A reviewed per-issue PR can be verified against its issue and squash-merged with the `verify-and-merge` workflow ([docs/workflow/verify-and-merge.md](docs/workflow/verify-and-merge.md)).
- **Parallel PRs can conflict without a textual conflict.** GitHub's mergeability check doesn't compile anything. After one PR merges into the release branch, merge the release branch into the next PR, build and test it, and let CI run again before merging. Two PRs that each add the same constant, or one that adds a struct field another's test doesn't set, both show as cleanly mergeable.
- **Script changes need `.conductor/tests/run.sh` to pass.** A change to `.conductor/scripts/*` also adds a fixture case for any new decision logic (see `.conductor/tests/README.md`).
- **Pipeline changes need a smoke run.** A PR that changes a `.conductor/workflows/*.wf` file or a workflow template (`conductor-core/templates/*.wft`) needs a passing `conductor workflow validate --dry-run <name>` of each changed workflow before merge, run from the PR's worktree (`--input k=v` for required inputs), with its run ID in the PR description. For a changed child workflow, smoke the child and a parent that calls it. The command runs the static checks, then starts a recorded engine dry run that skips every script step, auto-approves gates and cancels at the first agent step, so it makes no model calls and has no side effects. The pipeline runs these smokes for you (#3453): `verify.sh`, which ticket-to-pr, iterate-pr and quick-fix-pr call, runs the smoke of each changed workflow and of each workflow that calls one when the diff changes a `.wf` or `.wft` file. A failing smoke fails verify, and the PR body's `## Smokes` section lists each workflow, its result and its run ID (iterate-pr updates it after a push). Run a smoke by hand only for a workflow the section lists as skipped (a required input with no value) or a PR the pipeline didn't open.
- **Smoke before releasing.** Before the release commit, smoke-start `ticket-to-pr`, `iterate-pr` and `review-pr` on the release branch with `conductor workflow validate --dry-run <name>` (`--input ticket_id=…` as needed), using the binary built from that branch. If the release adds a migration, run it against a scratch `CONDUCTOR_HOME`, not the real home.
- **Release commit:** the last commit on the release branch is `release: X.Y.Z`, made with `scripts/release/bump.sh X.Y.Z`. It bumps `[workspace.package] version` in `Cargo.toml` **and** `plugin/.claude-plugin/plugin.json` (`plugin_tests` fails when the two differ), runs `cargo update -w --offline`, and fails if `Cargo.lock` changes anything beyond the workspace crates' own versions. It then commits only those three files as `release: X.Y.Z` and checks the commit's subject and files. It refuses to start when anything is staged. It prints `git push origin HEAD:release/X.Y.Z` without running it. `--no-commit` stops before the commit and prints the commit command.
- **Release PR:** `release/X.Y.Z` → `main`. Its description becomes the GitHub release notes, so write it as the notes, following the previous release's format. **Rebase-merge** it, so each issue's commit lands on `main` individually. The Release workflow (`.github/workflows/release.yml`) tags `vX.Y.Z` when a `release: ` commit reaches `main`.
- **After the release PR merges:** run `scripts/release/finish.sh X.Y.Z` from the main checkout (on `main`, no uncommitted changes). It pulls main first and re-runs itself when the pull changed `finish.sh`, so the release's own script changes take effect (finishing a release whose predecessor's script lacks this: `git pull --ff-only origin main` first). It waits for the Release workflow and verifies the `vX.Y.Z` tag and GitHub release, closes the `X.Y.Z` milestone (not if it still has open issues), fast-forwards each later unreleased `release/*` branch that has no commits of its own (and says which ones need main merged in instead), runs `./install-cli.sh`, updates the conductor plugin (`claude plugin marketplace update conductor-ai`, `claude plugin update conductor@conductor-ai`) and checks it is at `X.Y.Z`, then prints what is left by hand (restart Claude Code or `/mcp`). A missing `claude` or a failed update is listed as left over and the script exits 1. `--dry-run` shows the actions without taking them.
- **After pulling a release** (`finish.sh` does this for the release it finishes; on another checkout or machine do it by hand): run `./install-cli.sh`, then reconnect the conductor MCP server (`/mcp`) to pick up new MCP tools. Runs started through MCP (`conductor_run_workflow`, `conductor_resume_run`) execute in their own process on the installed binary, so they use the new build without a reconnect and survive one. The installed `conductor` in `~/.cargo/bin` is also the MCP server, and `./restart.sh` only rebuilds and launches the TUI, warning when the installed CLI is stale. `conductor dev reinstall` does the same from an installed 0.17.1 or later, and `conductor dev processes` lists running processes on an older build.

## Project Context

- **Vision & motivation:** [docs/VISION.md](docs/VISION.md)
- **Autonomous SDLC vision:** [docs/AUTONOMOUS-SDLC.md](docs/AUTONOMOUS-SDLC.md)
- **Current priorities:** https://github.com/devinrosen/conductor-ai/issues?q=is%3Aopen+label%3Aroadmap
- **Workflow engine design:** [docs/workflow/engine.md](docs/workflow/engine.md)
- **Workflow language reference** (semantics for `.wf` authors and reviewers; update it, and the Workflow Semantics brief, together): [docs/workflow/language.md](docs/workflow/language.md)
- **Notification hooks reference:** [docs/notification-hooks.md](docs/notification-hooks.md)

## TUI Threading Rule

> Visual reference: [docs/tui-threading.md](docs/tui-threading.md)

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
- `has_merged_pr()` check before worktree delete (`conductor-tui/src/app/crud_operations.rs`)
- Workflow execution (`conductor-tui/src/app/workflow_management.rs` — `spawn_workflow_in_background`)
- Workflow resume (`conductor-tui/src/app/workflow_management.rs` — `handle_resume_workflow`)
- PR fetch background task (`conductor-tui/src/background.rs`)

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
# ... make changes, run cargo test -p <affected-crate>, cargo fmt --all ...
git add <files> && git commit -m "..."
git push -u origin <branch>
gh pr create ...
```

## Key Conventions

- All record IDs are ULIDs (sortable, collision-resistant)
- All timestamps are ISO 8601 strings
- Worktree branch naming maps a fixed list of name prefixes to branch prefixes: `feat-`, `fix-`, `bug-` (→ `fix/`), `release-`, `chore-`, `docs-`, `refactor-`, `test-`, `ci-` and `perf-` (`SLUG_PREFIXES` in `conductor-core/src/worktree/manager.rs`). Any other name gets `feat-` prepended: `bump-0223` becomes slug `feat-bump-0223` on branch `feat/bump-0223`. A worktree named `release-X.Y.Z` maps to the `release/X.Y.Z` branch and collides with the release branch; name a release-commit worktree `chore-bump-XYZ` (branch `chore/bump-XYZ`)
- A GitHub issue filed after the last ticket sync isn't known to conductor yet: run a full `conductor_sync_tickets` (syncing that single ticket fails) before passing its number as `ticket_id`
- JS dep auto-install detects package manager via lockfile: bun > pnpm > yarn > npm
- Ticket upserts use `ON CONFLICT DO UPDATE` on `(repo_id, source_type, source_id)` for idempotency
