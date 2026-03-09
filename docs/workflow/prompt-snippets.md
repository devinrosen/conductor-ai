# Prompt Snippets

## Overview

Prompt snippets are reusable `.md` instruction blocks that are appended to an
agent's prompt at execution time via the `with` keyword. They let you extract
common context — coding conventions, diff-scope instructions, project
background — into shared files instead of duplicating text across agent
definitions.

Snippets are fully separate from agent definitions. An agent's `.md` file
describes what the agent does; snippets provide context it needs to do it
well. Any snippet can be reused across multiple agents and workflows.

---

## Syntax

### Single snippet

```
call implement { with = "rust-conventions" }
```

### Array of snippets

```
call review {
  with = ["review-diff-scope", "rust-conventions"]
}
```

### Block-level snippets in `parallel`

`with` on a `parallel` block applies to every call in the block:

```
parallel {
  with      = ["review-diff-scope"]
  fail_fast = false
  call review-security
  call review-style
  call review-tests
}
```

### Per-call additions inside `parallel`

Individual calls can add their own snippets on top of the block-level ones:

```
parallel {
  with = ["review-diff-scope"]
  call review-security
  call review-migrations { with = ["migration-rules"] }
}
```

`review-migrations` receives both `review-diff-scope` (from the block) and
`migration-rules` (its own addition), in that order.

---

## Prompt composition order

When the engine builds an agent's final prompt, it assembles:

1. Agent `.md` body (with `{{variable}}` substitution)
2. `with` snippets, each trimmed and joined with `\n\n` (also variable-substituted)
3. Schema output instructions / `CONDUCTOR_OUTPUT` block

Snippets go through the same `{{variable}}` substitution as the agent body,
so they can reference workflow inputs and prior context:

```markdown
<!-- .conductor/prompts/ticket-context.md -->
You are working on ticket {{ticket_id}}.

Background from the planning step:
{{prior_context}}
```

---

## Resolution order

### Short names

A value without `/` or `\` is a **short name**. The engine searches for
`<name>.md` in these locations, in order. The first match wins.

| Priority | Path | Scope |
|---|---|---|
| 1 | `.conductor/workflows/<workflow-name>/prompts/<name>.md` | Workflow-local override |
| 2 | `.conductor/prompts/<name>.md` | Shared conductor prompts |

Each priority level is checked first in the **worktree path**, then the
**repo path** (the registered repository root). This allows worktree-local
overrides of shared snippets without modifying shared files.

**Example layout:**

```
.conductor/
├── prompts/
│   ├── rust-conventions.md       # shared across all workflows
│   └── review-diff-scope.md
└── workflows/
    └── pr-review/
        └── prompts/
            └── review-diff-scope.md   # overrides shared version for pr-review
```

In the `pr-review` workflow, `with = "review-diff-scope"` resolves to the
workflow-local file. All other workflows use the shared version.

### Explicit paths

A value containing `/` or `\` is treated as a **path relative to the
repository root**:

```
call implement {
  with = [".conductor/prompts/rust-conventions.md", "docs/prompts/api-rules.md"]
}
```

Explicit paths skip the search order entirely — the file must exist at exactly
that location.

---

## Path safety

The following are rejected with a clear error:

- **Absolute paths** — e.g., `/home/user/snippet.md`
- **Paths that escape the repository root** — e.g., `../../etc/passwd`
- **Short names with path separators or `..`** — e.g., `../escape` or `a/b`
- **Null bytes** in names

Path traversal in explicit paths is caught by canonicalizing the resolved path
and verifying it starts with the repository root.

---

## Validation

`conductor workflow validate <name>` checks all `with` references before
execution begins. Missing snippets are reported with the paths that were
searched:

```
MISSING prompt snippets (1/2):
  - missing-snippet

Searched:
  .conductor/workflows/pr-review/prompts/missing-snippet.md
  .conductor/prompts/missing-snippet.md
```

`conductor workflow run` also validates snippets at startup and fails
immediately if any are missing, before any agent has been launched.

---

## File format

Snippet files are plain Markdown. No frontmatter. Content is trimmed of
leading and trailing whitespace before being appended.

```markdown
<!-- .conductor/prompts/rust-conventions.md -->
## Rust conventions

- Use `thiserror` for library errors, `anyhow` for binary errors.
- Prefer `?` over `unwrap()` except in tests.
- All new public functions require a doc comment.
- Run `cargo clippy -- -D warnings` before committing.
```

---

## Examples

### Reviewer workflow with shared diff-scope instructions

```
workflow pr-review {
  meta {
    description = "Run parallel code reviewers against a PR"
    trigger     = "manual"
  }

  inputs {
    pr_url required
  }

  parallel {
    with      = ["review-diff-scope"]
    fail_fast = false
    call review-architecture
    call review-security
    call review-tests
    call review-style
    call review-db-migrations { with = ["migration-rules"] }
  }
}
```

All five reviewers receive the `review-diff-scope` instructions. The
`review-db-migrations` reviewer additionally receives `migration-rules`.

### Adding ticket context to an implementation step

```
workflow ticket-to-pr {
  meta { trigger = "manual" }
  inputs { ticket_id required }

  call plan   { with = "ticket-context" }
  call implement {
    retries = 2
    with    = ["ticket-context", "rust-conventions"]
  }
  call push-and-pr
}
```

The `ticket-context` snippet might contain:

```markdown
## Ticket context

You are working on: {{ticket_id}}

Planning output: {{prior_context}}
```

---

## Implementation notes

The resolution logic lives in `conductor-core/src/prompt_config.rs`:

- `PromptSnippetRef` — enum distinguishing `Name` vs `Path` variants
- `load_prompt_snippet()` — load a single snippet by reference
- `load_and_concat_snippets()` — load multiple snippets and join with `\n\n`
- `snippet_exists()` — check existence without loading content (used by validation)
- `find_missing_snippets()` — return list of unresolvable references

The DSL parser (`workflow_dsl.rs`) stores `with` references as
`Vec<String>` on `CallNode` and `ParallelNode`. The execution engine
(`workflow.rs`) resolves and loads them just before building each agent prompt,
after variable substitution context is available.

---

## Design tradeoffs

**Why snippets instead of longer agent files?**

Agent files should describe what an agent does, not contain boilerplate that
applies to many agents. When the same instructions (e.g., "here is how to
read the diff") appear in a dozen agent files, updating them is error-prone.
Snippets make the shared part explicit and single-sourced.

**Why append instead of prepend?**

The agent body establishes the agent's role and primary task. Snippets provide
supporting context. Appending ensures the agent's core instructions are read
first and snippets don't overshadow them. Schema output instructions always
come last since they must appear at the end of the prompt.

**Why not inline snippets in the workflow file?**

Inline content in `.wf` files would mix workflow orchestration logic with
instruction text, making both harder to read and maintain. Separate files
allow version control diffs to clearly show what changed: structure vs.
content.

**Why not template includes in agent files?**

Agent files intentionally have no include/import mechanism — they are static
documents. Moving the composition to the workflow level keeps agent files
simple and independently readable, and gives the workflow author explicit
control over what context each step receives.
