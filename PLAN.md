# Plan: Worktree Creation Modal Offers Workflow Selection (#453)

## Summary

Replace the current binary "start agent?" confirm dialog shown after worktree creation with a selection list that offers: **Start agent**, **Run: \<workflow\>** (one per manual workflow def), and **Skip**. This lets users kick off a workflow like `ticket-to-pr` directly from worktree creation without a separate CLI step.

## Files to Modify

### 1. `conductor-tui/src/state.rs` — New modal variant + action enum

- Add `Modal::PostCreatePicker` variant:
  ```rust
  PostCreatePicker {
      items: Vec<PostCreateChoice>,
      selected: usize,
      worktree_id: String,
      worktree_path: String,
      worktree_slug: String,
      ticket_id: String,
      repo_path: String,
  }
  ```
- Add `PostCreateChoice` enum:
  ```rust
  pub enum PostCreateChoice {
      StartAgent,
      RunWorkflow { name: String, def: WorkflowDef },
      Skip,
  }
  ```
  (Display impl returns "Start agent", "Run: ticket-to-pr", "Skip" etc.)

### 2. `conductor-tui/src/app.rs` — Replace `maybe_start_agent_for_worktree` logic

- **`maybe_start_agent_for_worktree` (~line 3474):** Instead of directly showing a Confirm or AgentPrompt modal, build a `PostCreatePicker`:
  1. Look up the repo path from the worktree's `repo_id`.
  2. Call `WorkflowManager::list_defs(worktree_path, repo_path)` to discover workflows.
  3. Filter to `trigger == Manual`.
  4. Build `items` vec: `[StartAgent]` + one `RunWorkflow` per def + `[Skip]`.
  5. For `AutoStartAgent::Never`, skip entirely (existing behavior).
  6. For `AutoStartAgent::Always`, show the picker with `StartAgent` pre-selected (index 0).
  7. For `AutoStartAgent::Ask`, show the picker with `StartAgent` pre-selected (index 0). This replaces the old Confirm modal.

- **Add `handle_post_create_pick` method:** Handle the user's selection:
  - `StartAgent` → call existing `show_agent_prompt_for_ticket()` (unchanged).
  - `RunWorkflow { def, .. }` → spawn workflow execution in background thread, pre-filling `ticket_id` in the inputs map. Reuse the pattern from `handle_run_workflow` (~line 4099) but with the `ticket_id` input injected.
  - `Skip` → dismiss modal, no-op.

- **`handle_confirm` (~line 1410):** Remove `ConfirmAction::StartAgentForWorktree` handling (dead code after this change). Or keep it for backward compat if used elsewhere — verify first.

- **MoveUp/MoveDown handling in main event loop:** Add arm for `Modal::PostCreatePicker` to update `selected` index (same pattern as `WorkTargetPicker`).

- **InputSubmit / SelectPostCreateChoice handling:** Add arm to dispatch to `handle_post_create_pick`.

### 3. `conductor-tui/src/input.rs` — Key bindings for new modal

- Add `Modal::PostCreatePicker { items, .. }` arm (copy pattern from `WorkTargetPicker`, ~line 112):
  - `Esc` → `DismissModal`
  - `j/k` or `Up/Down` → `MoveUp/MoveDown`
  - `Enter` → new `Action::SelectPostCreateChoice(usize::MAX)` (sentinel for "use selected")
  - `1-9` digit → `Action::SelectPostCreateChoice(n - 1)`

### 4. `conductor-tui/src/action.rs` — New action variant

- Add `Action::SelectPostCreateChoice(usize)` to the Action enum.

### 5. `conductor-tui/src/ui/modal.rs` — Render the picker

- Add `render_post_create_picker()` function (modeled on `render_work_target_picker`, ~line 417):
  - Title: "Start work on #\<source_id\>?"
  - List items with `▸` cursor, number prefix, highlighted selection.
  - Footer: `"1-9 select  Enter confirm  Esc skip"`
  - Workflow entries show the workflow name (e.g., "Run: ticket-to-pr").

### 6. `conductor-tui/src/ui/mod.rs` — Wire rendering

- Add `Modal::PostCreatePicker { .. }` match arm calling `render_post_create_picker()`.

### 7. `conductor-tui/src/state.rs` — Debug impl

- Add `Modal::PostCreatePicker { .. }` arm to the existing `fmt::Debug` impl (~line 277).

## Design Decisions

1. **Single modal replaces Confirm + AgentPrompt flow.** The picker replaces the old "Start agent? y/n" confirm modal. Selecting "Start agent" still opens the AgentPrompt for the user to edit the prompt text — the two-step flow is preserved for agent starts, just the first step changes.

2. **Workflow loading happens at modal open time.** Workflows are loaded from disk when the picker opens, not cached. This keeps the list fresh and avoids stale state. The `list_defs` call is cheap (reads a few small files).

3. **Only manual-trigger workflows are shown.** PR-triggered and scheduled workflows don't make sense to run manually from this context.

4. **`ticket_id` is auto-injected.** When launching a workflow, the linked ticket's ID is inserted into the workflow's `inputs` map under key `"ticket_id"`. If the workflow doesn't declare a `ticket_id` input, the extra input is harmless (unused).

5. **No prompting for additional inputs.** Per the ticket: if a workflow requires inputs beyond `ticket_id`, we don't prompt — we launch with just `ticket_id` filled. The workflow's own defaults or error handling apply. This keeps the UX simple. A future enhancement could add a form step.

6. **`AutoStartAgent::Never` skips the picker entirely.** Users who opted out of auto-start don't see the picker at all, preserving their preference.

7. **Default selection is "Start agent" (index 0).** Users who hit Enter quickly get the same behavior as today.

## Risks and Unknowns

1. **`ConfirmAction::StartAgentForWorktree` removal.** Need to verify this variant is only used in the confirm modal path. If other code references it, keep the variant and just stop producing it from the worktree creation flow. (Grep shows it's only used at lines 1410 and 3487 — both in the flow being replaced, so safe to remove.)

2. **Repo path lookup.** `maybe_start_agent_for_worktree` currently doesn't receive `repo_path`. We need to look it up from `state.data.repos` using the worktree's `repo_id`. The worktree struct should have `repo_id` available since it was just created.

3. **Empty workflow list.** If no manual workflows exist, the picker shows just "Start agent" and "Skip" — still useful but a minor UX regression from the simpler confirm dialog. This is acceptable.

4. **Modal stacking.** Only one modal is active at a time. Selecting "Start agent" dismisses the picker and opens AgentPrompt — this is sequential, not stacked, so it works fine with the current architecture.
