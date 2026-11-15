# Verify recipe — conductor-ai

What `verify-acceptance` runs to check a reviewed PR against its issue. CI already gates fmt, clippy and the full test suite, so do not re-run the workspace suite: run only what shows the issue's behaviour. Run everything from this worktree.

## Build

- `cargo build --bin conductor`, and `cargo build --bin conductor-web` when `conductor-web/` changed (build the frontend first: `cd conductor-web/frontend && bun install && bun run build`).
- Use `target/debug/conductor` and `target/debug/conductor-web` from this worktree. Never the installed `conductor` in `~/.cargo/bin`, and never `install-cli.sh`, `cargo install` or `conductor dev reinstall`.

## Exercise what the issue names

- **CLI:** run the commands the issue names through `target/debug/conductor`.
- **MCP tools:** start `target/debug/conductor mcp serve` and drive it over stdio with JSON-RPC (`initialize`, then `tools/call`), or use the CLI equivalent when there is one.
- **Web:** start `target/debug/conductor-web` with `CONDUCTOR_PORT` set to a spare port (not 3000, and not a port already in use), hit the endpoints with `curl`, then kill it.
- **Validation:** when workflow validation changed, run `target/debug/conductor workflow validate` with the branch binary.

## Data safety

- If the diff adds a migration under `conductor-core/src/db/migrations/` (`.conductor/scripts/detect-db-migrations.sh` lists them), every command runs with `CONDUCTOR_HOME=$(mktemp -d)`. Never run the branch's build against `~/.conductor`: it migrates the database and locks out the installed binaries.
- Use a scratch `CONDUCTOR_HOME` for anything that writes, migration or not.
- Never touch real repos' worktrees or tickets, and never run a workflow that makes model calls.

## Pipeline changes

- A change under `.conductor/scripts/**`, `.conductor/tests/**` or `scripts/release/**`: run `.conductor/tests/run.sh`.
- A changed `.conductor/workflows/*.wf` or `conductor-core/templates/*.wft`: run `target/debug/conductor workflow validate --dry-run <name>` for each changed workflow from this worktree (`--input k=v` for required inputs). For a changed child workflow, smoke it and a parent that calls it. Put each run ID in `run_ids`.

## Tests

Run a targeted `cargo test -p <crate> <name>` only when it demonstrates a criterion that no command above shows.

## Release targets

- The PR must target the open `release/X.Y.Z` branch of its milestone (`git branch -r --list 'origin/release/*'`, `gh pr view --json milestone,baseRefName`). A PR for a release milestone that targets `main` is a `fail`.
- Linked issues are closed by `close-linked-issues.yml`. Do not close anything.
