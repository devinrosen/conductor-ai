# RFC 003: `--path` Flag for `conductor workflow run`

**Status:** Implemented
**Created:** 2026-03-11

## Background

Today, running a workflow requires either:
1. A registered repo + worktree (`conductor workflow run <repo> <worktree> <name>`)
2. A GitHub PR URL via the ephemeral shallow-clone path (`conductor workflow run <name> --pr <url>`)

Teams that want to use conductor as an embedded tool in a project — running workflows against their local checkout without registering a repo or managing worktrees — have no clean path. They must run `conductor repo add` and `conductor worktree create` even for a simple one-shot workflow execution.

`conductor workflow validate` already accepts `--path <dir>` to skip DB lookup. This RFC proposes extending the same flag to `conductor workflow run`.

## Proposed interface

```bash
conductor workflow run <name> --path <dir> [--input k=v] [--dry-run]
```

**Examples:**
```bash
# Run a repo-level workflow against the current directory
conductor workflow run publish-docs --path .

# Run from an explicit path with inputs
conductor workflow run publish-docs --path ~/LivelyVideo/umbrella --input force=true

# Dry run
conductor workflow run ticket-to-pr --path ~/my-project --input ticket_id=123 --dry-run
```

`--path` would conflict with `<repo>` and `<worktree>` positional arguments (same as `--pr` today).

## Key decisions

### 1. Does it write to the DB?

**Option A — Full DB record (like normal runs)**
Creates a `workflow_run` record with `worktree_id = NULL` and `repo_id = NULL`, same as the `--pr` ephemeral path. Run history is visible via `conductor workflow runs` (though without a repo/worktree association it may not surface cleanly).

**Option B — No DB record (truly ephemeral)**
Executes the workflow entirely in memory, no persistence. Simple to implement, but loses resumability and run history.

**Recommendation:** Option A. Resumability is a core conductor guarantee. If a `--path` run fails mid-way, the user should be able to `conductor workflow resume <run-id>` to pick up where it left off. A run with `worktree_id = NULL` and `repo_id = NULL` already works (the `--pr` path uses this today).

### 2. What is the working directory for agents?

For `--pr`, the working directory is the shallow clone temp dir. For `--path`, it should be the directory passed in — resolved to an absolute path. Agents run in tmux windows with their CWD set to that directory.

This means `--path` runs operate on the **real local checkout**, not a copy. Agents with `can_commit: true` can commit and push directly. This is intentional — it matches how a developer would run the workflow manually.

### 3. How does it relate to `--pr`?

They are complementary, not competing:

| | `--pr` | `--path` |
|---|---|---|
| Source | Shallow clone from GitHub | Local directory |
| Cleanup | Temp dir deleted after run | Nothing deleted |
| Can commit | No (no push target) | Yes (if agent has `can_commit: true`) |
| Registration required | No | No |
| Resumable | No (temp dir gone) | Yes |
| Use case | Ephemeral analysis on any PR | Running against a local checkout |

`--pr` remains the right choice for read-only analysis on PRs from any repo. `--path` is the right choice for persistent automation on a local checkout.

### 4. Where does the workflow definition come from?

Same resolution as `--path` on `workflow validate`: load from `<dir>/.conductor/workflows/<name>.wf`. No DB lookup, no registered repo required.

### 5. What about agent resolution and prompt snippets?

Same `resolve_conductor_subdir` logic used elsewhere: check `<dir>/.conductor/agents/` and `<dir>/.conductor/prompts/`. This is already how the `--pr` path works (using the cloned repo root as both `worktree_path` and `repo_path`).

### 6. Run history discoverability

With no `repo_id` or `worktree_id`, `conductor workflow runs` (which requires a repo slug) won't surface `--path` runs. Options:

- Add `conductor workflow runs --path <dir>` to list runs associated with a path
- Store the `working_dir` path in the `workflow_runs` table and allow querying by it
- Accept the limitation for now (runs are still accessible by ID via `conductor workflow run-show <id>`)

**Recommendation:** Accept the limitation in v1. Print the run ID to stdout at start so the user can reference it. Address discoverability in a follow-up.

## Implementation sketch

The change is largely in `conductor-cli/src/main.rs`:

1. Add `--path` to the `Run` command (conflicts with `repo`, `worktree`)
2. When `--path` is set, call a new `run_workflow_on_path()` function (analogous to `run_workflow_on_pr()` in `workflow_ephemeral.rs`)
3. `run_workflow_on_path()` resolves the absolute path, loads the workflow def, validates inputs, and calls `execute_workflow()` with `worktree_id: None`, `working_dir: &abs_path`, `repo_path: &abs_path`

The core engine (`workflow.rs`) requires no changes — it already supports `worktree_id: None` runs.

## What this unlocks

- **Embedded CLI use case:** Teams add `.conductor/` to their repo, install the `conductor` binary, and run `conductor workflow run <name> --path .` with no other setup
- **CI/CD integration:** A GitHub Actions job can download the conductor binary and run `conductor workflow run validate-docs --path $GITHUB_WORKSPACE` without any DB state
- **Local dev without worktree management:** Run a workflow against your current checkout when you just want a one-shot execution, not a tracked worktree

## Open questions

- Should `--path` runs be resumable? Resuming requires the working directory to still exist at the same path — reasonable assumption for local use, less so for CI.
- Should there be a `--no-db` flag to opt into truly ephemeral execution (Option B above) for CI use cases where DB state is unwanted?
- Should `conductor workflow list --path <dir>` also be added for symmetry? (`validate` already has it; `list` does not.)

## Related

- `docs/getting-started-cli.md` — CLI guide that motivates this RFC
- `conductor workflow validate --path` — existing precedent in `conductor-cli/src/main.rs`
- `workflow_ephemeral.rs` — `--pr` implementation, the direct analogue
- RFC 002 — workflow targets expansion
