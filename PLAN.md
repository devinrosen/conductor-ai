# Plan: Add PR Pane to Repo Detail TUI View (#488)

## Summary

Add a PR pane to the repo detail view that lists open GitHub pull requests for
the selected repo. The layout changes the bottom row from a single full-width
Tickets pane to a horizontal 50/50 split: Tickets (left) | PRs (right). Tab/
BackTab cycling extends to Worktrees → Tickets → PRs → Worktrees. PR data is
fetched via `gh pr list` in a background thread (same subprocess pattern as
ticket sync) and is kept in-memory only — not persisted to SQLite.

## Proposed Layout

```
┌──────────────────────────────────────────┐
│ Info (8 lines)                           │
├──────────────────────────────────────────┤
│ Worktrees (50%)                          │
├─────────────────────┬────────────────────┤
│ Tickets (50%)       │ PRs (50%)          │
└─────────────────────┴────────────────────┘
```

## Files to Modify

### 1. `conductor-core/src/github.rs`
- Add `GithubPr` struct: `{ number: i64, title: String, author: String, state: String, head_ref_name: String }`.
- Add `pub fn list_open_prs(remote_url: &str) -> Result<Vec<GithubPr>>`:
  - Calls `parse_github_remote` to extract owner/repo; returns `Ok(vec![])` for non-GitHub remotes.
  - Runs `gh pr list --repo <slug> --state open --json number,title,author,state,headRefName --limit 50`.
  - Parses JSON; the `author` field is an object `{ "login": "..." }` so a helper struct is needed.
  - Returns `Ok(vec![])` (not an error) on gh failure so the pane degrades gracefully.

### 2. `conductor-tui/src/action.rs`
- Add `PrsRefreshed { repo_id: String, prs: Vec<conductor_core::github::GithubPr> }` variant.

### 3. `conductor-tui/src/state.rs`
- Add `Prs` variant to `RepoDetailFocus`.
- Replace `toggle()` with `next()` and `prev()` for the 3-way cycle:
  `next`: Worktrees → Tickets → Prs → Worktrees; `prev` reverses.
- Add `detail_prs: Vec<GithubPr>` and `detail_pr_index: usize` to `AppState`.
- Initialise new fields to `Vec::new()` / `0` in `AppState::new()`.
- Update the `current_list_len()` / `set_list_index()` methods (the helpers
  used by `move_up`/`move_down` in app.rs) to handle the `Prs` arm.

### 4. `conductor-tui/src/background.rs`
- Add `pub fn spawn_pr_fetch_once(tx: BackgroundSender, remote_url: String, repo_id: String)`:
  spawns a thread that calls `conductor_core::github::list_open_prs` and sends
  `Action::PrsRefreshed { repo_id, prs }`.

### 5. `conductor-tui/src/app.rs`
- **`Action::PrsRefreshed`**: if `repo_id == state.selected_repo_id`, update
  `state.detail_prs`, reset `state.detail_pr_index = 0`, record timestamp for
  throttle.
- **Entering RepoDetail** (in `select()`, Dashboard Repos branch): clear
  `state.detail_prs` (avoid stale data), call `spawn_pr_fetch_once`.
- **Periodic refresh** (in `Action::DataRefreshed` handler, or `Action::Tick`):
  if `view == RepoDetail` and last PR fetch was >30 s ago, call
  `spawn_pr_fetch_once`. Use a `static AtomicI64` guard (same pattern as
  `LAST_REAP` in background.rs).
- **`next_panel()` / `prev_panel()`**: replace `toggle()` calls with `next()` /
  `prev()`.
- **`move_up()` / `move_down()`**: add `RepoDetailFocus::Prs` arms that
  adjust `state.detail_pr_index`.
- **`select()`**: add `RepoDetailFocus::Prs` arm (no-op for now).
- **`clamp_indices()`**: clamp `detail_pr_index` to `detail_prs.len()`.

### 6. `conductor-tui/src/ui/repo_detail.rs`
- **Layout**: split `layout[2]` (bottom row) horizontally 50/50 into
  `bottom[0]` (Tickets) and `bottom[1]` (PRs).
- **PR list widget**: `List<ListItem>` with one row per PR:
  `#<number>  <title>  [<state>]  @<author>  <head_ref_name>`
  State is coloured green for `open`.
- **Focus border**: cyan when `RepoDetailFocus::Prs`, dark-grey otherwise.
- **Empty state**: show `"(loading…)"` when `detail_prs` is empty and no fetch
  has completed yet; `"(no open PRs)"` once a fetch has returned empty.
  Distinguish via `pr_last_fetched_at` field in AppState.

### 7. `conductor-tui/src/ui/help.rs` (minor)
- If the help modal lists per-view shortcuts, update the RepoDetail section to
  mention Tab → "cycle focus (Worktrees / Tickets / PRs)".

## Design Decisions

| Decision | Rationale |
|---|---|
| In-memory only (no DB) | Keeps schema unchanged; list is re-fetched on view entry, which is acceptable. |
| Background thread per fetch | `gh pr list` takes 0.5–3 s on networks; must not block the event loop. |
| 30 s throttle via `DataRefreshed` | Piggybacks on existing 2 s DB poll; avoids a dedicated PR-poller thread. |
| `next()` / `prev()` replacing `toggle()` | 3-way cycle cannot be expressed with a boolean toggle. |
| No action on PR select | Deferred per ticket scope; future ticket will add `review-pr` workflow launch. |
| Limit 50 PRs | Prevents slow fetches on busy repos; covers team awareness use-case. |

## Risks / Unknowns

- **`gh` not installed / not authenticated**: `list_open_prs` must return
  `Ok(vec![])` on subprocess failure (currently `run_gh` returns `Result` —
  callers use `.unwrap_or_default()`). Verify the error path and ensure no
  error modal is surfaced for a missing `gh` CLI.
- **Non-GitHub remotes**: `parse_github_remote` returns `None` → early return
  with empty list. Optionally show `"(GitHub only)"` in the pane title.
- **Author JSON shape**: `gh pr list --json author` returns `{"login":"..."}`,
  not a flat string. A serde helper struct is required.
- **Terminal height**: The 3-row layout with a horizontal bottom split leaves
  ~5–7 rows per pane on 24-line terminals. Test on small terminals and verify
  usability.
