---
name: roundtable-testing
description: Run the testing roundtable review on the current PR.
---

# roundtable-testing

Run the testing roundtable review, which evaluates the current PR through three specialist lenses: test coverage, integration testing, and regression risk.

## Steps

### 0. Pre-flight checks

Verify the current directory is inside a conductor-registered worktree:

```bash
conductor worktree find .
```

If this fails, try:
```bash
cargo run --bin conductor -- worktree find .
```

If neither works, tell the user: "This directory is not inside a registered conductor worktree. Please run from within a worktree."

Extract the repo slug and worktree slug from the output.

### 1. Verify workflow exists

Check that the roundtable-testing workflow is available:

```bash
ls .conductor/workflows/roundtable-testing.wf
```

If missing, tell the user: "The roundtable-testing workflow is not found. Ensure `.conductor/workflows/roundtable-testing.wf` exists."

### 2. Validate the workflow

```bash
conductor workflow validate <repo-slug> <worktree-slug> roundtable-testing
```

Or: `cargo run --bin conductor -- workflow validate <repo-slug> <worktree-slug> roundtable-testing`

If validation fails, explain each error and offer fixes before proceeding.

### 3. Run the roundtable

```bash
conductor workflow execute <repo-slug> <worktree-slug> roundtable-testing
```

Or: `cargo run --bin conductor -- workflow execute <repo-slug> <worktree-slug> roundtable-testing`

### 4. Report results

After execution completes, summarize the verdict:
- **Verdict**: pass/fail
- **Confidence**: score out of 100
- **Consensus mode**: consensus or discussion
- **Findings**: list each finding with reviewer, severity, file, line, and message
- **Recommendation**: whether the PR has adequate test coverage or needs additional tests

If the verdict is "fail" or confidence is below 70, list the specific findings that need to be addressed.
