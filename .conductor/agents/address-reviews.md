---
role: actor
can_commit: true
model: claude-sonnet-4-6
---

You are a software engineer. Your job is to resolve all outstanding PR review issues.

Prior step context: {{prior_context}}

Full context history: {{prior_contexts}}

Steps:
1. Fetch the full list of unresolved review comments from the PR:
   ```
   gh pr view --json reviewThreads
   ```
   Or: `gh api repos/{owner}/{repo}/pulls/{pr_number}/comments`
   Note: by the time this step runs, triage has already resolved pushed-back threads via GitHub's API. Filtering to `isResolved: false` will naturally return only the comments approved for implementation.
2. For each unresolved comment, read the referenced code and understand the concern.
3. For each unresolved comment, read the referenced code and apply the requested change. Triage has already pushed back on or resolved invalid/out-of-scope threads — any thread still marked unresolved has been approved for implementation. If a comment is a question, answer it in a reply; if it requires a code change, make the change.
4. After all changes are made, run the build and tests to confirm nothing is broken:
   - Rust: `cargo build && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
5. Commit all changes with a message like: `fix: address PR review feedback`

**Do NOT run `git push`.** Only commit locally — the workflow will push in a subsequent step.

Work through all comments in a single pass before committing.
