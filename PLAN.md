# Plan: feat/521 — Conductor Workflow Purge

## Summary

Add a `conductor workflow purge` subcommand that deletes completed, failed, and cancelled workflow runs from the database. Mirrors the existing `worktree purge` pattern. Steps are cleaned up automatically via `ON DELETE CASCADE` (verified in migration `020_workflow_runs.sql`: `workflow_run_id TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE`).

---

## Files to Create or Modify

### 1. `conductor-core/src/workflow.rs` — Add `purge()` and `purge_count()` to `WorkflowManager`

`WorkflowManager<'a>` takes `&'a Connection` + `&'a Config`. Add two new methods:

```rust
pub fn purge(
    &self,
    repo_id: Option<&str>,
    statuses: &[&str],
) -> Result<usize>
```

- `statuses` defaults to `["completed", "failed", "cancelled"]` at the call site (callers may pass a subset)
- If `repo_id` is `Some(...)`, scopes deletion to runs whose `worktree_id` belongs to that repo:
  ```sql
  DELETE FROM workflow_runs
  WHERE status IN (...)
    AND worktree_id IN (SELECT id FROM worktrees WHERE repo_id = ?)
  ```
- If `repo_id` is `None`, deletes across all repos:
  ```sql
  DELETE FROM workflow_runs WHERE status IN (...)
  ```
- Returns `Ok(count)` of deleted rows
- `workflow_run_steps` are cleaned up automatically by `ON DELETE CASCADE`

```rust
pub fn purge_count(
    &self,
    repo_id: Option<&str>,
    statuses: &[&str],
) -> Result<usize>
```

- Same WHERE clause as `purge()` but uses `SELECT COUNT(*)` — no deletion
- Used by `--dry-run`

**Note:** The `status IN (...)` clause must be built dynamically since rusqlite doesn't support binding a slice directly. Use the same placeholder-construction pattern already used elsewhere in `workflow.rs`.

### 2. `conductor-cli/src/main.rs` — Add `Purge` variant to `WorkflowCommands` and handler

**Enum addition** (after `GateFeedback`, before the closing `}`):
```rust
/// Delete completed, failed, and cancelled workflow runs
Purge {
    /// Only purge runs for this repo slug
    #[arg(long)]
    repo: Option<String>,
    /// Filter by status: completed, failed, cancelled, all (default: all terminal)
    #[arg(long)]
    status: Option<String>,
    /// Print what would be deleted without deleting
    #[arg(long)]
    dry_run: bool,
},
```

**Handler** (inside the `WorkflowCommands` match arm, after `GateFeedback`):
1. Resolve `--status` into `Vec<&str>`:
   - `None` or `"all"` → `["completed", "failed", "cancelled"]`
   - A specific value → validate against allowed set, error on unknown
2. If `--repo` provided: resolve slug → `repo_id` via `RepoManager::get_by_slug()`
3. If `--dry-run`: call `WorkflowManager::purge_count(repo_id, &statuses)`, print without deleting
4. Otherwise: call `WorkflowManager::purge(repo_id, &statuses)`, print count

**Example output:**
```
Purged 14 workflow runs (12 completed, 2 failed).
```
or for dry-run:
```
Would purge 14 workflow runs (dry run).
```

---

## Design Decisions

- **Cascade handles steps:** `workflow_run_steps` has `ON DELETE CASCADE` on `workflow_run_id`, so no explicit step deletion is needed.
- **Status validation:** CLI validates `--status` values against the allowed set (`completed`, `failed`, `cancelled`, `all`) and returns a user-friendly error for unknown values.
- **`all` shorthand:** `--status all` expands to all three terminal statuses.
- **No TUI/web change required** by the ticket; CLI-only scope per acceptance criteria.
- **`--dry-run`** uses a separate `COUNT(*)` query rather than a transaction rollback, keeping it simple and consistent with user expectations.
- **Repo resolution:** If `--repo` is provided but doesn't exist, `RepoManager::get_by_slug()` returns an error — same behavior as other repo-scoped commands.
- **Dynamic IN clause:** Build placeholder string (`?,?,?`) at runtime from the `statuses` slice length — same pattern used in other variable-filter queries in `workflow.rs`.

---

## Risks / Unknowns

- None identified. The schema already supports cascades, the manager pattern is established, and the CLI pattern is well-defined by `worktree purge`.
