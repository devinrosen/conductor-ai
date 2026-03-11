---
name: conductor-workflow-init
description: Bootstrap the .conductor/ directory structure in any git repo.
---

# conductor-workflow-init

Bootstrap the `.conductor/` directory tree in a git repository so it is ready for conductor workflows, agents, and prompts.

## Steps

### 0. Resolve and validate target directory

Determine the target directory:
- If the user provided a path argument, use it
- Otherwise default to CWD: `$(pwd)`

Validate it is a git repo root:
```bash
git -C <target_dir> rev-parse --show-toplevel
```

- If the command fails (non-zero exit), stop and report: "`<target_dir>` is not inside a git repository."
- If the returned path does not equal `<target_dir>`, stop and report: "`<target_dir>` is a subdirectory — please pass the repo root (returned: `<actual_root>`)."

### 1. Create the directory structure

Run `mkdir -p` for each required subdirectory (idempotent — safe to re-run):

```bash
mkdir -p <target_dir>/.conductor/agents
mkdir -p <target_dir>/.conductor/workflows
mkdir -p <target_dir>/.conductor/prompts
mkdir -p <target_dir>/.conductor/schemas
mkdir -p <target_dir>/.conductor/reviewers
```

### 2. Add `.gitkeep` files to empty directories

For each directory, add a `.gitkeep` only if the directory contains no other files (so existing content is never overwritten):

```bash
for dir in agents workflows prompts schemas reviewers; do
  target="<target_dir>/.conductor/$dir"
  if [ -z "$(ls -A "$target" 2>/dev/null)" ]; then
    touch "$target/.gitkeep"
  fi
done
```

### 3. Report results

For each of the five directories, report whether it was:
- **Created** — directory did not exist before this run
- **Already existed** — directory was already in place (idempotent)
- **`.gitkeep` added** — directory was new or empty, `.gitkeep` was placed
- **`.gitkeep` skipped** — directory already had content

Example output:
```
.conductor/agents/      created, .gitkeep added
.conductor/workflows/   created, .gitkeep added
.conductor/prompts/     created, .gitkeep added
.conductor/schemas/     created, .gitkeep added
.conductor/reviewers/   created, .gitkeep added
```

## Notes

- This skill is safe to run multiple times — all operations are idempotent.
- The `conductor-workflow-create`, `conductor-workflow-update`, and `conductor-workflow-validate` skills call this init automatically as a preflight when `.conductor/agents/` or `.conductor/workflows/` is missing — you do not need to run it manually first.
- After init, commit the scaffold: `git add .conductor && git commit -m "chore: bootstrap .conductor/ scaffold"`
