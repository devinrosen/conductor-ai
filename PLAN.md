# Plan: Remove pr_review.rs and post_run.rs (#504)

## Summary

Remove ~3400 lines of pre-workflow orchestration code (`pr_review.rs`, `post_run.rs`, `review_config.rs`) that have been fully superseded by the workflow engine (`review-pr.wf`, `ticket-to-pr.wf`, `ticket-to-pr-auto-merge.wf`). Both prerequisites (#502, #503) are already merged.

`merge_queue.rs` is **kept** — it is still actively used by the web API routes (`conductor-web/src/routes/merge_queue.rs`) and the CLI's `MergeQueue` subcommands. Only `pr_review.rs` imported it from core; once that is gone, `merge_queue.rs` has no obsolete consumers.

`review_config.rs` is **removed** — the audit confirms it is exclusively imported by `pr_review.rs` (no other consumers outside the module itself). The comment in `workflow_config.rs` referencing it is a doc comment only and does not constitute a runtime dependency.

---

## Files to Delete

| File | Lines | Reason |
|------|-------|--------|
| `conductor-core/src/pr_review.rs` | ~2528 | Superseded by `review-pr.wf` |
| `conductor-core/src/post_run.rs` | ~843 | Superseded by `ticket-to-pr.wf` + `ticket-to-pr-auto-merge.wf` |
| `conductor-core/src/review_config.rs` | ~390 | Only consumed by `pr_review.rs` |

---

## Files to Modify

### `conductor-core/src/lib.rs`
Remove three `pub mod` declarations:
```rust
pub mod post_run;
pub mod pr_review;
pub mod review_config;
```

### `conductor-cli/src/main.rs`

1. **Remove imports** (lines 20–21):
   ```rust
   use conductor_core::post_run::{self, PostRunInput};
   use conductor_core::pr_review::{self, ReviewSwarmConfig, ReviewSwarmInput};
   ```

2. **Remove `Commands::Approve` variant** (lines 74–80) — superseded by `conductor workflow gate-approve <run-id>`

3. **Remove `AgentCommands::PostRun` variant** (lines 188–194) — superseded by `conductor workflow run ticket-to-pr`

4. **Remove `AgentCommands::Review` variant** (lines 195–211) — superseded by `conductor workflow run review-pr`

5. **Remove three match arms** for the above variants:
   - `AgentCommands::PostRun { ... }` (~lines 929–947)
   - `AgentCommands::Review { ... }` (~lines 948–999)
   - `Commands::Approve { ... }` (~lines 1753–1765)

---

## What Is NOT Changed

- `conductor-core/src/merge_queue.rs` — kept; used by web API and CLI `merge-queue` subcommands
- `conductor-cli/src/main.rs` `MergeQueue` subcommand block — kept
- TUI and web routes — no direct imports of the removed modules; no changes needed
- Review swarm agent `.md` files in `.conductor/agents/` — used by `review-pr.wf`

---

## Design Decisions

- **`merge_queue.rs` stays**: The web API exposes merge queue CRUD endpoints and the CLI has a full `merge-queue` subcommand. The `ticket-to-pr-auto-merge.wf` workflow's `merge-and-close` agent interacts with the merge queue via these surfaces.

- **No deprecation warnings**: The issue calls for a clean removal. These commands are entirely internal orchestration surfaces (not user-facing APIs with external consumers).

- **TUI unaffected**: `conductor-tui` does not directly import `pr_review`, `post_run`, or `review_config`. Zero changes needed there.

---

## Task List

### task-1: Delete pr_review.rs, post_run.rs, review_config.rs
**Files:**
- `conductor-core/src/pr_review.rs` (delete)
- `conductor-core/src/post_run.rs` (delete)
- `conductor-core/src/review_config.rs` (delete)

### task-2: Remove module declarations from lib.rs
**Files:** `conductor-core/src/lib.rs`

Remove the three `pub mod` lines for `post_run`, `pr_review`, `review_config`.

### task-3: Remove CLI commands and imports
**Files:** `conductor-cli/src/main.rs`

Remove imports, `Commands::Approve`, `AgentCommands::PostRun`, `AgentCommands::Review`, and their match arms.

### task-4: Verify build and tests pass
Run `cargo build --workspace` and `cargo test --workspace` to confirm no hidden consumers remain.
