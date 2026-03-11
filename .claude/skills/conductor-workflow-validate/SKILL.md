---
name: conductor-workflow-validate
description: Run conductor workflow validate, interpret each error, and offer actionable fixes.
---

# conductor-workflow-validate

Run `conductor workflow validate`, interpret each error type, and offer actionable fixes.

## Steps

### 0. Resolve target directory and run init preflight

**Determine the target directory:**
- If the user provided a directory path as an argument, use it as `<target_dir>`
- Otherwise default to CWD: `$(pwd)`

**Validate it is a git repo root:**
```bash
git -C <target_dir> rev-parse --show-toplevel
```
- If the command fails, stop and report: "`<target_dir>` is not inside a git repository."
- If the returned path does not equal `<target_dir>`, stop and report: "`<target_dir>` is a subdirectory — please pass the repo root."

**Auto-init preflight (silent):**
```bash
[ -d <target_dir>/.conductor/agents ] && [ -d <target_dir>/.conductor/workflows ]
```
If either is missing, silently run the init steps (do not ask the user — mention it in the final summary):
```bash
mkdir -p <target_dir>/.conductor/agents
mkdir -p <target_dir>/.conductor/workflows
mkdir -p <target_dir>/.conductor/prompts
mkdir -p <target_dir>/.conductor/schemas
mkdir -p <target_dir>/.conductor/reviewers
for dir in agents workflows prompts schemas reviewers; do
  t="<target_dir>/.conductor/$dir"
  [ -z "$(ls -A "$t" 2>/dev/null)" ] && touch "$t/.gitkeep"
done
```

**All file paths in subsequent steps use `<target_dir>` as the prefix.**

### 1. Determine which workflow(s) to validate

If the user named a specific workflow, validate that one. Otherwise, validate all:

```bash
ls <target_dir>/.conductor/workflows/
```

### 2. Run validation

For a specific workflow:
```bash
conductor workflow validate --path <target_dir> <name>
```

For all workflows (iterate over each `.wf` file):
```bash
for f in <target_dir>/.conductor/workflows/*.wf; do
  name="${f%.wf}"; name="${name##*/}"
  echo "=== $name ==="
  conductor workflow validate --path <target_dir> "$name"
done
```

If `conductor` is not on PATH, use:
```bash
cargo run --bin conductor -- workflow validate --path <target_dir> <name>
```

### 3. Interpret each error

Parse the output and categorize every error. For each one, explain the root cause and suggest a concrete fix.

---

#### Missing agent `.md` file

**Symptom:** `agent not found: <name>` or similar, listing paths that were searched.

**Cause:** A `call <name>` statement references an agent that has no `.md` file in the resolution order:
1. `.conductor/workflows/<workflow>/agents/<name>.md` (workflow-local)
2. `.conductor/agents/<name>.md` (shared)

(Each path is checked in the worktree first, then the repo root.)

**Fix options:**
- Create the agent file at `.conductor/agents/<name>.md` (most common)
- Create a workflow-local override at `.conductor/workflows/<workflow>/agents/<name>.md`
- Rename the `call` statement to match an existing agent

Offer to create a stub:
```markdown
---
role: reviewer
can_commit: false
---

You are a ... agent. Your task is: ...

Prior step context: {{prior_context}}

[TODO: flesh out this prompt]
```

---

#### Unresolved `with` snippet

**Symptom:** `prompt snippet not found: <name>` or similar, listing paths searched.

**Cause:** A `with = ["<name>"]` reference has no matching `.md` file in the resolution order:
1. `.conductor/workflows/<workflow>/prompts/<name>.md` (workflow-local)
2. `.conductor/prompts/<name>.md` (shared)

**Fix options:**
- Create the snippet at `.conductor/prompts/<name>.md`
- Create a workflow-local snippet at `.conductor/workflows/<workflow>/prompts/<name>.md`
- Remove the `with` reference if it's no longer needed
- Correct the snippet name to match an existing file

Offer to create a stub prompt file with placeholder content.

---

#### Circular workflow reference

**Symptom:** `Circular workflow reference: a -> b -> c -> a` or similar.

**Cause:** `call workflow` statements create a reference cycle. The engine detects this statically (it does not depend on runtime conditions — even an `if`-guarded reference counts).

**Fix:** Break the cycle by refactoring. Options:
- Extract the shared steps into a third workflow that both can call without creating a cycle
- Inline the steps from one workflow into the other instead of using `call workflow`
- Remove one of the `call workflow` references if it was unintentional

Show the full cycle chain (e.g., `a → b → c → a`) and identify which link is the easiest to break.

---

#### Missing `targets` declaration

**Symptom:** `missing required field: targets` or similar.

**Cause:** The `meta { }` block does not include a `targets` field.

**Fix:** Add `targets` to the `meta` block:
```
meta {
  description = "..."
  trigger     = "manual"
  targets     = ["worktree"]
}
```

Valid values:
- `["worktree"]` — workflow operates on a git worktree (branch-scoped work)
- `["repo"]` — workflow operates at the repository level
- `["worktree", "repo"]` — workflow can run in either context

---

#### Loop configuration error (`max_iterations` / `stuck_after`)

**Symptom:** `max_iterations is required` or `stuck_after must be less than max_iterations`.

**Cause A — missing `max_iterations`:** Every `while` and `do {} while` loop requires `max_iterations = N`. This is mandatory to prevent infinite loops.

**Fix:** Add `max_iterations = N` inside the loop body. Choose a value that allows enough iterations for the task but prevents runaway loops (e.g., 5–10 for review loops).

```
while review.has_issues {
  max_iterations = 5
  on_max_iter    = fail
  call address-reviews
  call review
}
```

**Cause B — `stuck_after` ≥ `max_iterations`:** If `stuck_after` is set, it must be strictly less than `max_iterations`.

**Fix:** Either decrease `stuck_after` or increase `max_iterations`.

---

#### Unknown gate type

**Symptom:** `unknown gate type: <value>`.

**Cause:** `gate` only accepts: `human_approval`, `human_review`, `pr_approval`, `pr_checks`.

**Fix:** Correct the gate type to one of the valid values.

| Gate type | Use case |
|---|---|
| `human_approval` | Block until a human explicitly approves (no feedback text) |
| `human_review` | Block for human review; feedback injected as `{{gate_feedback}}` |
| `pr_approval` | Block until the PR has the required number of GitHub approvals |
| `pr_checks` | Block until all PR CI checks pass |

---

#### Other / unknown errors

For any error not covered above:
1. Quote the exact error message
2. Identify the line in the `.wf` file it refers to (read the file if needed)
3. Explain what the parser expected vs. what it found
4. Offer a concrete fix

### 4. Apply fixes

For each error, offer to apply the fix directly. Make the smallest correct change. After applying all fixes, re-run validation.

### 5. Confirm clean

Re-run `conductor workflow validate --path <target_dir> <name>` after all fixes. Report the final result:
- If clean: "Validation passed — no errors found."
- If still failing: iterate until clean or ask the user for clarification.

## Notes

- Validation is always safe to run — it reads files and checks structure, it does not execute any agents or modify state.
- If `conductor` is not built yet, run `cargo build --bin conductor` first.
- `conductor workflow validate` checks agents, snippets, and cycles. It does not check that agent prompts make semantic sense — that requires reading the individual `.md` files.
- After validation passes, suggest a dry run to verify runtime behavior: `conductor workflow run <name> --dry-run`. Note: `--dry-run` does not yet support `--path`; if the workflow lives outside the current repo, `cd` to `<target_dir>` first.
