# RFC 002: Workflow Targets Beyond Worktrees

**Status:** Implemented
**Created:** 2026-03-10

## Background

Workflows currently require a worktree context to run. The worktree provides a working directory for the agent and a branch to commit changes to. This works well for code-change workflows but excludes use cases where the natural target is a ticket or a repo rather than a specific branch.

This RFC captures early thinking on expanding workflow targets. No decisions have been made.

## Current model

```
WorkflowDef
  └── runs on → Worktree (provides: file path, branch, repo context)
```

A `WorkflowDef` has an `inputs` field for user-supplied values and a `targets` field, but execution always resolves to a worktree. Attempting to run a workflow from the global Workflows view (no worktree selected) currently fails silently with "No worktree selected".

## Proposed target types

### Ticket-scoped workflows

Tickets are metadata — no file system access needed. The agent operates via `gh` CLI.

**Use cases:**
- Cleanup / normalize ticket text (grammar, formatting, acceptance criteria)
- Triage (assess complexity, suggest labels, estimate effort)
- Detect duplicate tickets against other open issues
- Generate a draft spec from a ticket description
- Auto-create a worktree from the ticket + run initial analysis in one step (the "ticket → worktree + agent" pipeline)

**Risk level:** Low. No file system writes. Worst case is a bad GitHub API call, which is reversible.

### Repo-scoped workflows

The repo has a `local_path` so an agent can operate there, but this means the main branch — risky for writes.

**Use cases (read-only / safe):**
- Dependency audit / security scan
- Stale branch and PR cleanup report
- Generate changelog from merged PRs
- Repo health dashboard (test coverage, open issues by age, CI flakiness)
- Ticket sync — replace the manual `s` key with a schedulable repo-scoped workflow

**Use cases (write — requires care):**
- Should probably always require creating a worktree first
- Could be a workflow step: `create_worktree → do work → open PR`

**Risk level:** Medium for read-only, High for writes. Write operations on a repo-scoped workflow should either be blocked or require explicit confirmation.

## Possible architecture

Rather than separate workflow "types", treat it as a **target discriminator** on the workflow definition:

```yaml
target: worktree | ticket | repo | none
```

- The TUI shows only workflows compatible with the current context
- Running from a ticket context passes `ticket_id` as an implicit input
- Running from a repo context passes `repo_path` as an implicit input
- `none` workflows are always runnable (no context required) — useful for cross-cutting automation

This maps naturally onto the existing `inputs` field — the target type just determines which implicit inputs are pre-filled vs. user-supplied.

### TUI implications

- **Workflows view (global mode):** show all `repo`-targeted and `none`-targeted workflows; filter out `worktree` and `ticket` workflows with a note
- **Ticket pane:** `r` on a selected ticket opens workflows filtered to `target: ticket`
- **Repo detail view:** `r` opens workflows filtered to `target: repo`
- **WorktreeDetail:** unchanged — `r` runs a `target: worktree` workflow

## Open questions

- Should `target` be a hard constraint or a hint? (Can a `worktree` workflow fall back to repo path if no worktree is selected?)
- What does the agent's working directory look like for a ticket workflow? Probably a temp dir or the repo root in read-only mode.
- For the "ticket → create worktree" pipeline, does that live as a built-in workflow step type (`create_worktree`) or is it composed from existing primitives?
- How does this interact with scheduled/cron workflows? A repo health check could be a natural daily cron.
- Should repo-scoped write workflows be blocked entirely, or just require a confirmation gate?

## Related

- Issue #515 — TUI keybinding cleanup (removes `s` → SyncTickets, which could become a repo workflow)
- `docs/workflow/engine.md` — workflow engine design
- `docs/ROADMAP.md` — current priorities
