# Plan: Claude Code Skill for Workflow Authoring (#499)

## Summary

Add three Claude Code skills to lower the barrier for workflow authoring. Skills live in `.claude/skills/` as `SKILL.md` files and appear in Claude Code's `/` command menu. No Rust changes are needed — this is purely new skill definition files.

The three skills are:
- **`/conductor-workflow-create`** — Guided workflow authoring from a plain-English description
- **`/conductor-workflow-update`** — Plain-English change requests against an existing workflow
- **`/conductor-workflow-validate`** — Interpret `conductor workflow validate` output and surface actionable fixes

---

## Files to Create

### `.claude/skills/conductor-workflow-create/SKILL.md`

Guides Claude through creating a new workflow from scratch:

1. Ask the user what the workflow should do and what it operates on (`targets`)
2. Introspect existing repo state:
   - `ls .conductor/agents/` — available agents to `call`
   - `ls .conductor/workflows/` — existing workflows for `call workflow` composition (and cycle-avoidance)
   - `ls .conductor/prompts/` — available prompt snippets for `with =`
3. Draft a `.wf` structure using the appropriate DSL constructs
4. Identify any new agent `.md` files that need to be created (with frontmatter stubs)
5. Write the `.wf` file to `.conductor/workflows/<name>.wf`
6. Write any new agent stub files to `.conductor/agents/<name>.md`
7. Run `conductor workflow validate <name>` to confirm clean
8. Report what was created and any remaining TODOs (e.g., flesh out agent prompts)

**DSL knowledge to embed:** Full grammar, all constructs (`call`, `parallel`, `if`/`unless`/`while`/`do {} while`, `do`, `gate`, `always`), loop options (`max_iterations` required, `stuck_after` optional, `on_max_iter`), gate types (`human_approval`, `human_review`, `pr_approval`, `pr_checks`), agent frontmatter schema, context threading (`{{prior_context}}`, `{{prior_contexts}}`), `CONDUCTOR_OUTPUT` block format, composition via `call workflow`, agent/snippet resolution order.

The skill instructs Claude to read `docs/workflow/engine.md` at the start for the canonical DSL reference.

### `.claude/skills/conductor-workflow-update/SKILL.md`

Applies a plain-English change to an existing workflow:

1. Identify which workflow to update: list `.conductor/workflows/*.wf` and ask the user if not obvious from context
2. Read the existing `.wf` file
3. Introspect existing agents and prompts to ensure suggested additions actually exist
4. Apply the change — explain what DSL construct is being modified and why
5. Rewrite the `.wf` file (prefer minimal diffs — only change what's needed)
6. Create any new agent stubs if the change requires a new `call` target
7. Run `conductor workflow validate <name>`
8. Summarize what changed and why, referencing DSL semantics (e.g., "Changed `while` to `do {} while` so the review step always runs at least once")

**Example changes to handle:**
- "Add a human review gate before merging" → insert `gate human_review { ... }` at correct position
- "Increase the review loop iteration cap" → update `max_iterations` on the loop
- "Add a performance reviewer to the parallel block" → add `call review-performance` inside `parallel { ... }`, warn if `review-performance.md` doesn't exist

### `.claude/skills/conductor-workflow-validate/SKILL.md`

Interprets `conductor workflow validate` output and surfaces actionable fixes:

1. Determine which workflow(s) to validate: validate all if no name given, or specific workflow if named
2. Run `conductor workflow validate <name>` (or for all: iterate over `.conductor/workflows/*.wf`)
3. Parse output and categorize each error:
   - **Missing agent `.md` files** — show the resolution order that was searched, suggest the correct file path, offer to create a stub
   - **Unresolved `with` snippets** — show the search paths, suggest `.conductor/prompts/<name>.md`, offer to create a stub
   - **Cycle errors** — explain the full reference chain (e.g., `a → b → c → a`), explain which workflow to refactor to break the cycle
   - **Missing `targets` declaration** — explain what `targets` means, show valid values (`worktree`, `repo`)
   - **`stuck_after` / `max_iterations` misconfiguration** — explain that `max_iterations` is required on `while`/`do {} while`, `stuck_after` must be less than `max_iterations`
4. For each error, offer to apply the fix directly
5. Re-run validation after fixes and confirm clean

---

## Design Decisions

### Three separate skills vs one multi-command skill

The ticket proposes a single `conductor-workflow` skill with three sub-commands. Claude Code's `/` menu works best with discrete skills, so this plan uses three separate skills (`conductor-workflow-create`, `conductor-workflow-update`, `conductor-workflow-validate`). Each maps 1:1 to a command, is independently readable, and avoids branching logic in a single large SKILL.md.

### DSL knowledge in skills vs referencing docs

Skills instruct Claude to **read `docs/workflow/engine.md` at the start of each session**. This keeps the SKILL.md files focused on procedure, not DSL documentation, and ensures they stay accurate as the DSL evolves. Key DSL constructs are also summarized inline as a quick reference for the most common patterns.

### Introspection approach

Skills use shell commands (`ls`, `cat`, `conductor workflow validate`) to inspect actual repo state before suggesting anything. This is what the ticket calls "contextually correct suggestions" — the skill won't suggest `call review-security` if that agent doesn't exist.

### No Rust changes

This feature is entirely skill authoring. The `conductor workflow validate` command already exists and surfaces the right error messages. The skills layer intent and explanation on top of existing tooling.

---

## Risks and Unknowns

- **Skill naming convention**: Claude Code's skill menu currently shows `rebase-and-fix-review`. There's no established precedent for multi-word skill groups. Using `conductor-workflow-create` etc. is the safest bet.
- **`conductor` binary availability**: The validate skill runs `conductor workflow validate`. If the binary isn't built or on PATH, the skill should fall back to `cargo run --bin conductor -- workflow validate` or explain the issue.
- **Skill args support**: Claude Code's Skill tool accepts `args`, so `/conductor-workflow-create` with no args works, but users may try `/conductor-workflow create` (space-separated). Document the correct invocation in each SKILL.md.

---

## Task List

### task-1: Create conductor-workflow-create skill
**Files:** `.claude/skills/conductor-workflow-create/SKILL.md`

Full step-by-step skill for guided workflow creation: gather intent, introspect repo, draft DSL, write files, validate.

### task-2: Create conductor-workflow-update skill
**Files:** `.claude/skills/conductor-workflow-update/SKILL.md`

Step-by-step skill for applying plain-English changes to an existing workflow: read current state, apply change, rewrite, validate.

### task-3: Create conductor-workflow-validate skill
**Files:** `.claude/skills/conductor-workflow-validate/SKILL.md`

Step-by-step skill for interpreting validate output: categorize errors, explain causes, offer fixes, re-validate.
