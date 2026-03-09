# Agent Path Resolution in Workflows

## Overview

Workflows reference agents via the `call` statement. Today, agents are resolved
by **short name** from a fixed set of directories under `.conductor/`. This
proposal extends `call` to also accept **explicit relative paths**, giving
workflow authors full control over where agent definitions live.

---

## Syntax

### Short name (existing behavior)

```
call plan
call implement { retries = 2 }
```

The engine resolves the name using the search order described below. This is
the default and recommended form for most workflows.

### Explicit path (new)

```
call "./custom/agents/my-lint.md"
call ".claude/agents/code-review.md"
call "lib/agents/shared-reviewer.md"
```

When the argument to `call` is a **quoted string**, the engine treats it as a
path relative to the **repository root**. The file must exist and must be a
valid agent `.md` file (optional YAML frontmatter + markdown body). No search
order is applied — the path is used as-is.

### How the parser distinguishes them

| Token after `call` | Interpretation |
|---|---|
| Bare identifier (`plan`, `review-code`) | Short name — resolved via search order |
| Quoted string (`"path/to/agent.md"`) | Explicit path — relative to repo root |

The lexer already supports both token types. Bare identifiers allow
`[a-zA-Z0-9_-]`. Paths require `/` and `.` which are not valid in identifiers,
so the quoting requirement is a natural consequence of the grammar — not an
arbitrary rule.

---

## Short-name resolution order

When `call` receives a bare identifier (e.g., `call plan`), the engine resolves
it by checking the following locations in order. The first match wins.

| Priority | Path | Scope |
|---|---|---|
| 1 | `.conductor/workflows/<workflow>/agents/<name>.md` | Workflow-local override (worktree, then repo) |
| 2 | `.conductor/agents/<name>.md` | Shared conductor agents (worktree, then repo) |

Each priority level is checked first in the **worktree path**, then in the
**repo path** (the registered repository root). This allows worktree-local
overrides of shared agents.

### Proposed additions to the search order

| Priority | Path | Scope |
|---|---|---|
| 1 | `.conductor/workflows/<workflow>/agents/<name>.md` | Workflow-local override (worktree, then repo) |
| 2 | `.conductor/agents/<name>.md` | Shared conductor agents (worktree, then repo) |
| 3 | `.claude/agents/<name>.md` | Claude Code agents (worktree, then repo) |

Priority 3 enables reuse of agents defined for Claude Code's own agent
framework without duplicating files into `.conductor/agents/`. Conductor-specific
agents still take precedence.

---

## Explicit path rules

1. **Paths are relative to the repository root.** Absolute paths are rejected.
2. **The `.md` extension is required** in the path (unlike short names where it
   is appended automatically).
3. **The file must exist** at resolution time. A missing file is an error, not a
   fallback trigger.
4. **Frontmatter is parsed identically** to short-name agents — same `role`,
   `can_commit`, `model` fields.
5. **The agent name** is derived from the file stem (e.g.,
   `"lib/agents/my-lint.md"` produces agent name `my-lint`).
6. **Path traversal** (`../`) is allowed but constrained — the resolved path
   must remain within the repository root. Escaping the repo is an error.

---

## Examples

### Mixed workflow

```
workflow ticket-to-pr {
  meta {
    description = "Plan, implement, and review"
    trigger     = "manual"
  }

  inputs {
    ticket_id required
  }

  # Resolved via short-name search order (.conductor/agents/plan.md)
  call plan

  # Resolved via short-name search order (.conductor/agents/implement.md)
  call implement { retries = 2 }

  call push-and-pr

  # Explicit path to a Claude Code agent
  call ".claude/agents/code-review.md"

  while code-review.has_review_issues {
    max_iterations = 5
    call address-reviews
    call ".claude/agents/code-review.md"
  }
}
```

### Team-shared agents in a subdirectory

```
workflow lint-fix {
  meta {
    description = "Lint and auto-fix"
    trigger     = "manual"
  }

  # Agent definitions live alongside the workflow for portability
  call ".conductor/workflows/lint-fix/agents/analyze-lint.md"
  call ".conductor/workflows/lint-fix/agents/fix-lint.md"
}
```

Note: the second example already works today via the workflow-local override
(priority 1 in the search order), so the short-name form `call analyze-lint`
would resolve identically. The explicit path form is useful when you want to
be unambiguous or when agents live outside the conventional directories.

---

## Validation

`conductor workflow validate <name>` currently checks that all referenced agents
exist. This extends naturally:

- **Short names:** checked against the search order (no change).
- **Explicit paths:** checked for file existence relative to repo root.

The error message for a missing explicit-path agent should show the exact path
that was tried, since there is no search order to enumerate.

---

## `on_fail` agent references

The `on_fail` option in a `call` block also references an agent:

```
call implement {
  retries = 2
  on_fail = diagnose
}
```

`on_fail` values follow the same rules: bare identifiers use the search order,
quoted strings are explicit paths:

```
call implement {
  retries  = 2
  on_fail  = "custom/agents/diagnose-build.md"
}
```

---

## `parallel` agent references

Agents inside `parallel` blocks follow the same rules:

```
parallel {
  call reviewer-security
  call ".claude/agents/code-review.md"
  call "team/agents/perf-reviewer.md"
}
```

---

## Implementation summary

### Parser changes (`workflow_dsl.rs`)

1. **`parse_call()`**: After consuming the `call` token, check if the next
   token is a `StringLit` (quoted path) or an `Ident` (short name). Store the
   result in `CallNode`.
2. **`CallNode` struct**: Add a field to distinguish name vs. path, or store
   a single enum:
   ```rust
   pub enum AgentRef {
       Name(String),       // bare identifier — use search order
       Path(String),       // quoted string — relative path from repo root
   }
   ```
3. **`collect_agent_names()`**: Update to collect both variants for validation.
4. **`on_fail` parsing**: Apply the same `Ident` vs `StringLit` logic.

### Agent resolution (`agent_config.rs`)

1. **`load_agent()`**: Accept `AgentRef` instead of `&str`. For `AgentRef::Path`,
   resolve relative to repo root and call `parse_agent_file()` directly.
2. **Add `.claude/agents/` fallback**: Insert as priority 3 in the existing
   search order for `AgentRef::Name`.
3. **Path safety**: Canonicalize the resolved path and verify it starts with
   the repo root.

### Workflow execution (`workflow.rs`)

1. **`execute_call()`** and **`execute_parallel()`**: Pass `AgentRef` through
   to `load_agent()` instead of a plain string.
2. **Validation phase**: Handle both variants when checking agent existence
   before execution begins.

### Error messages

- Short name not found: list all searched paths (as today, plus `.claude/agents/`).
- Explicit path not found: show the single resolved path.
- Path escapes repo root: specific error message.

---

## Considered alternatives

Several approaches were evaluated before settling on the hybrid (short name +
explicit path) design. They are documented here for context.

### Option A: `.claude/agents/` as a fixed fallback only

Add `.claude/agents/<name>.md` as priority 3 in the short-name search order
and make no other changes. No explicit path support.

**Why not chosen:** Solves the immediate `.claude/agents` reuse case but does
not help with agents that live in arbitrary project directories (e.g.,
`team/agents/`, `lib/prompts/`). Users would still have to symlink or copy
files into a blessed directory. The explicit path syntax covers this and all
other locations in one mechanism.

Note: We did adopt the `.claude/agents/` fallback *in addition to* explicit
paths, since it provides a convenient default for the common case.

### Option B: Configurable `agent_paths` in `config.toml`

Add an `agent_paths` array to `~/.conductor/config.toml` or a per-repo
`.conductor/config.toml`:

```toml
agent_paths = [
  ".conductor/agents",
  ".claude/agents",
  "my-team/agents",
]
```

The engine would search each directory in order when resolving short names.

**Why not chosen:** Adds configuration surface area and indirection. The search
order becomes non-obvious — you need to read the config to understand which
agent a short name resolves to. Debugging "why did it pick that agent?" gets
harder as the list grows. The explicit path syntax achieves the same
flexibility without hidden state: if you want an agent from `my-team/agents/`,
you write `call "my-team/agents/reviewer.md"` and there is no ambiguity.

Could be revisited if teams find themselves repeatedly writing long explicit
paths to the same directories.

### Option C: Per-workflow `agents_dir` in `meta`

Allow each workflow to declare a custom agent directory:

```
workflow lint-fix {
  meta {
    agents_dir = "lib/agents"
  }

  call analyze-lint    # resolves from lib/agents/analyze-lint.md
}
```

**Why not chosen:** Only shifts the search location — it does not support
mixing agents from multiple directories within a single workflow. A workflow
that needs one agent from `.conductor/agents/` and another from `lib/agents/`
would still need a second mechanism. Explicit paths handle this naturally since
each `call` independently specifies its source.

### Option D: Fully explicit paths only

Remove the short-name search order entirely. Every `call` must use a quoted
path:

```
call ".conductor/agents/plan.md"
call ".conductor/agents/implement.md"
call ".claude/agents/code-review.md"
```

**Why not chosen:** Too verbose for the common case. Most workflows use agents
from `.conductor/agents/`, and requiring the full path on every `call` adds
noise without adding information. The short-name form keeps simple workflows
readable while explicit paths are available when needed.

---

## Out of scope

- **Remote/URL agent references** (e.g., fetching from a registry). Deferred to
  a future proposal.
- **Glob patterns** in `call` (e.g., `call "agents/*.md"`). Agents are called
  individually.
