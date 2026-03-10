---
name: tui-keys
description: Audit all TUI keyboard bindings — universal, view-specific, modal-scoped — and flag conflicts, undocumented keys, and cleanup candidates.
---

# tui-keys

Produce a complete keybinding reference for the conductor TUI, grouped by scope, with conflict detection and a diff against the in-app help text.

## Source files

All keyboard logic lives in two files — read both in full before producing any output:

- `conductor-tui/src/input.rs` — single source of truth for all key→action mappings (`map_key()`)
- `conductor-tui/src/ui/help.rs` — the help modal shown to users (`?` key)

Do **not** rely on grep or partial reads. Read each file completely.

## Steps

### 1. Extract all key bindings from `input.rs`

Walk through `map_key()` in priority order and record every binding:

1. **Global override** — keys that fire regardless of any state (e.g. `Ctrl+C`)
2. **Modal-scoped** — one sub-table per `Modal::*` variant
3. **Filter mode** — keys active when `state.any_filter_active()` is true
4. **Global vim-scroll** — `gg` chord, `Ctrl+d`, `Ctrl+u` (before view-specific block)
5. **View-specific** — one sub-table per `View::*` variant, noting any additional conditions (agent active, pane focused, etc.)
6. **Global normal** — the final `match key.code` fallthrough block

For each binding record: key, condition (if any), action name, scope.

### 2. Extract documented bindings from `help.rs`

Parse every `help_line(key, description)` call and collect the set of documented keys.

### 3. Output the matrix

#### Universal Keys
Keys from the global override, global vim-scroll, and global normal blocks.

```
## Universal Keys
| Key | Action | Notes |
|---|---|---|
| `Ctrl+C` | Quit | Always fires, highest priority |
...
```

#### View-Specific Keys
One table per view. Include the condition column when bindings are conditional.

```
## View: WorktreeDetail
| Key | Condition | Action | Shadows Global? |
|---|---|---|---|
...

## View: Workflows
...
```

#### Modal-Scoped Keys
One table per modal type.

```
## Modal: Confirm
| Key | Action |
|---|---|
...
```

#### Filter Mode
```
## Filter Mode
| Key | Action |
|---|---|
...
```

### 4. Flag conflicts

After the tables, print a **Conflicts & Shadows** section listing every case where:
- A view-specific binding fires the same key as a global binding (shadowing)
- The same key maps to different actions in two or more views
- A modal binding conflicts with expected global behavior (e.g. a modal swallowing `q`)

Format:
```
## Conflicts & Shadows
- `a` — global: AddRepo | WorktreeDetail (agent active): AttachAgent
- `j` — global: MoveDown | WorktreeDetail: AgentActivityDown (panel navigation lost)
...
```

### 5. Diff against help.rs

Print two lists:

**Undocumented keys** — bindings in `input.rs` not present in `help.rs`:
```
## Undocumented (in input.rs but missing from help.rs)
- `!` → ToggleStatusBar
- `3` → GoToWorkflows
...
```

**Stale help entries** — keys documented in `help.rs` that don't exist or differ in `input.rs`:
```
## Stale Help Entries (in help.rs but not in input.rs)
- (none) or list items
```

### 6. Flag candidates for cleanup

After the diffs, print an actionable **Cleanup Candidates** section. Categorize each item:

- **Redundant binding** — two keys do the same thing (e.g. `2` and `t` both GoToTickets); suggest removing one
- **Confusing shadow** — a context-dependent shadow that could surprise users; suggest renaming the key or adding a guard note
- **Undocumented** — present in code, absent from help; suggest adding to `help.rs`
- **Chord conflict** — a key that can intercept a chord's first keypress (e.g. `g` in WorkflowRunDetail intercepting `gg`)

Format:
```
## Cleanup Candidates
- **Redundant**: `t` and `2` both map to GoToTickets — remove `t` or `2`
- **Confusing shadow**: `o` means OrchestrateAgent (WorktreeDetail, agent inactive) vs OpenTicketUrl (global) — consider a less-common key for orchestrate
...
```

End with a one-line summary:
```
X universal keys, Y view-specific bindings across Z views, W modal scopes. N conflicts, M undocumented.
```
