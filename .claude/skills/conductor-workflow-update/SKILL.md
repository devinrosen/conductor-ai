---
name: conductor-workflow-update
description: Apply a plain-English change request to an existing conductor workflow (.wf file).
---

# conductor-workflow-update

Apply a plain-English change to an existing conductor workflow `.wf` file.

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

### 0a. Read the canonical DSL reference

Read `docs/workflow/engine.md` for the full grammar, all constructs, and design rationale. This is the authoritative source — use it throughout this session.

### 1. Identify the target workflow

If the user specified a workflow name in their request, use it. Otherwise:

```bash
ls <target_dir>/.conductor/workflows/
```

List the available workflows and ask the user which one to modify.

### 2. Read the current workflow

Read the `.wf` file:
```bash
cat <target_dir>/.conductor/workflows/<name>.wf
```

Also introspect available agents and prompts to ensure any additions you suggest actually exist:

```bash
ls <target_dir>/.conductor/agents/
ls <target_dir>/.conductor/prompts/ 2>/dev/null || echo "(no prompts directory)"
```

### 3. Understand the change request

Parse the user's plain-English request. Map it to the relevant DSL construct. Common patterns:

| Request | DSL change |
|---|---|
| "Add a human review gate before merging" | Insert `gate human_review { ... }` at the correct position |
| "Increase the review loop iteration cap" | Update `max_iterations` on the `while`/`do {} while` block |
| "Add a performance reviewer to the parallel block" | Add `call review-performance` inside `parallel { ... }` |
| "Run cleanup even if the workflow fails" | Wrap the cleanup call in `always { ... }` |
| "Skip the tests if tests are already passing" | Add `unless build.has_test_failures { call run-tests }` |
| "Make the review loop run at least once" | Change `while` to `do { ... } while` |
| "Add a prompt snippet for code conventions" | Add `with = ["<snippet-name>"]` to the relevant `call` or `parallel` |
| "Add an input parameter" | Add entry to the `inputs { }` block |

If the change involves adding a new `call <agent>`:
- Check if `.conductor/agents/<agent>.md` exists
- If not, note that you'll need to create a stub and warn the user

If the change involves adding a `call workflow <name>`:
- Check if `.conductor/workflows/<name>.wf` exists
- Check for potential cycles: verify the referenced workflow does not (directly or transitively) call back to the workflow being modified

### 4. Plan the edit

Before making changes, state explicitly:
1. Which line(s)/block(s) in the current `.wf` file will change
2. What DSL construct is being added/modified and why it's the right choice
3. Any new files that need to be created (agent stubs, prompt files)

Example explanation: "Changing `while` to `do {} while` so the review step always runs at least once — with `while`, if the prior step emits no markers the body never executes."

### 5. Apply the change

Make the minimal edit necessary. Do not refactor or reformat parts of the file that the user didn't ask to change. Preserve existing comments and spacing.

Key rules to maintain:
- `while` and `do {} while` require `max_iterations = N` (required)
- `stuck_after` must be strictly less than `max_iterations` if both are set
- `targets` must remain in `meta { }` (required field)
- All `call <agent>` targets must resolve to existing `.md` files (or new stubs you create)
- All `with = [...]` snippet references must resolve to existing `.md` files in `.conductor/prompts/`

### 6. Create any new agent stubs

For each new `call` target that doesn't have an existing agent file:

```markdown
---
role: actor
can_commit: false
---

You are a ... agent. Your task is: ...

Prior step context: {{prior_context}}

[TODO: flesh out this prompt]
```

Write to `<target_dir>/.conductor/agents/<name>.md`.

### 7. Write the updated workflow

Rewrite `<target_dir>/.conductor/workflows/<name>.wf` with the applied changes.

### 8. Validate

```bash
conductor workflow validate --path <target_dir> <name>
```

If `conductor` is not on PATH:
```bash
cargo run --bin conductor -- workflow validate --path <target_dir> <name>
```

### 9. Summarize

Report:
- What changed (line-level summary)
- Why the chosen DSL construct is correct for this use case
- Any new files created
- Whether `.conductor/` was auto-initialized (if the preflight ran)
- Any TODOs remaining (e.g., "stub agent at `<target_dir>/.conductor/agents/review-performance.md` still needs its prompt fleshed out")
- If validation passed

If validation failed, interpret and fix each error before declaring done.

## Notes

- Prefer minimal diffs. Only change what the user asked to change.
- If the user's request is ambiguous (e.g., "make the loop better"), ask a clarifying question before editing.
- When adding a gate, consider the most natural position: human approval gates typically come before irreversible steps (push, merge, deploy); human review gates come after agent-generated findings.
- `gate human_review` accepts written feedback via `{{gate_feedback}}` in the next step — mention this if the user is adding a review gate so they know to thread it into the downstream agent prompt.
- Dry-run the updated workflow to verify it is structurally sound: `conductor workflow run <name> --dry-run`. Note: `--dry-run` does not yet support `--path`; if the workflow lives outside the current repo, `cd` to `<target_dir>` first.
