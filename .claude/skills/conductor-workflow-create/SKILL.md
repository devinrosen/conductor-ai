---
name: conductor-workflow-create
description: Create a new conductor workflow (.wf file) from a plain-English description.
---

# conductor-workflow-create

Guide the user through creating a new conductor workflow `.wf` file from scratch.

## Steps

### 0. Read the canonical DSL reference

Read `docs/workflow/engine.md` for the full grammar, all constructs, and design rationale. This is the authoritative source — use it throughout this session.

### 1. Gather intent

Ask the user:
- What should this workflow do? (plain-English description)
- What does it operate on? (`worktree` for branch-level work, `repo` for repo-level work, or both)
- What should the workflow be named? (suggest a hyphenated slug from their description if they don't provide one)
- Are there any required inputs the workflow needs from the user at run time? (e.g., `ticket_id`, `pr_url`)

If the user already provided this information as arguments to the skill, skip to step 2.

### 2. Introspect existing repo state

Run these commands to understand what's available:

```bash
ls .conductor/agents/
```
```bash
ls .conductor/workflows/
```
```bash
ls .conductor/prompts/ 2>/dev/null || echo "(no prompts directory)"
```

Note the exact filenames (without `.md`/`.wf` extension) — these are the identifiers you can use in `call` statements. Do not suggest `call` targets that don't exist without noting they need to be created.

### 3. Draft the workflow structure

Design the `.wf` file using the correct DSL constructs. Key rules:

**Structure:**
```
workflow <name> {
  meta {
    description = "..."
    trigger     = "manual"
    targets     = ["worktree"]   # or ["repo"] or ["worktree", "repo"]
  }

  inputs {
    my_input required
    optional_input default = "value"
  }

  # steps go here
}
```

**Choosing constructs:**
- Sequential agent call: `call <agent-name>`
- Conditional: `if <step>.<marker> { ... }` or `unless <step>.<marker> { ... }`
- Loop (check before first run): `while <step>.<marker> { max_iterations = N ... }`
- Loop (always run at least once): `do { ... } while <step>.<marker>`
- Parallel agents: `parallel { call a; call b; call c }`
- Human gate: `gate human_review { prompt = "..." timeout = "48h" on_timeout = fail }`
- Automated gate: `gate pr_checks { timeout = "2h" on_timeout = fail }`
- Always run (cleanup/notify): `always { call notify-result }`
- Compose another workflow: `call workflow <name>`

**Loop options** (required/optional):
- `max_iterations = N` — **required** on `while` and `do {} while`
- `stuck_after = N` — optional, must be < `max_iterations`
- `on_max_iter = fail` — optional, defaults to `fail`

**Context threading:**
- `{{prior_context}}` — context string from the immediately preceding step
- `{{prior_contexts}}` — JSON array of all prior step contexts
- `{{gate_feedback}}` — feedback text from a `human_review` gate

**Prompt snippets** (reusable `.md` files from `.conductor/prompts/`):
```
call review { with = ["review-diff-scope"] }
parallel { with = ["review-diff-scope"]; call review-security }
```

**Workflow composition:**
```
call workflow lint-fix
call workflow test-coverage { inputs { pr_url = "{{pr_url}}" } }
```
Avoid creating a cycle — check existing `.wf` files to ensure the workflow you're creating doesn't reference a workflow that would transitively reference it back.

### 4. Identify missing agents

Compare the `call` statements in your draft against the existing agents listed in step 2. For any agent that doesn't exist yet, note that you will need to create a stub `.md` file at `.conductor/agents/<name>.md`.

Stub format:
```markdown
---
role: actor
can_commit: false
---

You are a ... agent. Your task is: ...

Prior step context: {{prior_context}}

[TODO: flesh out this prompt]
```

Use `role: reviewer` and `can_commit: false` for read-only agents (reviewers, validators).
Use `role: actor` and `can_commit: true` for agents that write code or commit changes.

### 5. Present the plan and confirm

Show the user:
1. The proposed `.wf` file contents
2. Any new agent stub files that will be created
3. A brief explanation of why each construct was chosen

Ask for confirmation before writing any files. If the user wants changes, revise and show again.

### 6. Write the files

Write the `.wf` file:
```
.conductor/workflows/<name>.wf
```

Write any new agent stub files:
```
.conductor/agents/<name>.md
```

### 7. Validate

Run validation using one of these commands (try `conductor` first, fall back to `cargo run`):

```bash
conductor workflow validate <name>
```

If that fails with "command not found":
```bash
cargo run --bin conductor -- workflow validate <name>
```

### 8. Report results

If validation passes:
- Confirm what was created
- List any agent stub files that still need their prompts fleshed out
- Suggest a test run: `conductor workflow run <name> --dry-run`

If validation fails:
- Interpret each error (see `/conductor-workflow-validate` for error guidance)
- Fix the issues and re-validate before declaring done

## Notes

- The `targets` field in `meta` is required. Use `["worktree"]` for branch-scoped work, `["repo"]` for repository-level work.
- Agent names in `call` statements must match the filename without `.md` (e.g., `call review-security` resolves to `.conductor/agents/review-security.md`).
- `while` and `do {} while` conditions reference `<step-name>.<marker>` where `<step-name>` is the name of a prior `call` statement and `<marker>` is a string the agent emits in its `CONDUCTOR_OUTPUT` markers array.
- A workflow that only calls read-only agents can still be useful — dry-run mode is safe for all agents with `can_commit: false`.
