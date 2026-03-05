---
name: rebase-and-fix-review
description: Rebase the current branch onto main and address all on-diff PR review comments.
---

# rebase-and-fix-review

Rebase the current branch onto main, then address all on-diff PR review comments.

## Steps

0. **Guard: refuse if on main**
   ```
   git branch --show-current
   ```
   If the current branch is `main`, stop immediately and tell the user to check out the feature branch first. Do not proceed.

1. **Rebase onto main**
   ```
   git fetch origin
   git rebase origin/main
   ```
   Resolve any conflicts that arise, preserving the intent of both sides.

2. **Find the PR for this branch**
   ```
   gh pr view --json number,url,headRefName
   ```

3. **Get all review feedback** from two sources:

   **a) On-diff review comments** (line-level comments with file/line position):
   ```
   gh api repos/{owner}/{repo}/pulls/{pr_number}/comments --paginate
   ```
   Filter to comments where `position` or `line` is non-null (these are in-diff).

   **b) PR issue comments** (conductor review results posted as PR comments):
   ```
   gh api repos/{owner}/{repo}/issues/{pr_number}/comments --paginate
   ```
   Look for comments that contain structured review findings — these are posted by the conductor review system. Parse findings that have **Severity: warning** or **Severity: critical** as actionable items. Each finding will include a **Location** (file:line reference) and a **Fix** recommendation.

   Ignore findings with severity `suggestion` — they are non-blocking.

4. **For each actionable finding** (from either source), address the issue in code:
   - Read the referenced file and line range for context
   - Make the smallest correct fix that satisfies the reviewer's concern
   - Do not refactor surrounding code unless the comment specifically asks for it
   - Do not add comments/docs unless explicitly requested

5. **Verify the build still passes**
   ```
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```

6. **Stage and commit the fixes**
   Group related fixes into logical commits. Use `fix:` prefix. Reference the PR review where helpful.

7. **Push the branch**
   ```
   git push --force-with-lease
   ```
   Force push is required after a rebase since history is rewritten. `--force-with-lease` is preferred over `--force` — it refuses if the remote has commits you haven't fetched, preventing accidental overwrites.

## Notes
- On-diff = comments tied to a specific file + line (have `path` + `position`/`line` fields in the GitHub API response)
- Conductor reviews = structured findings posted as issue comments, with **Issue**, **Severity**, **Location**, and **Fix** fields
- Off-diff = general PR comments without file/line or structured findings — skip these, they require separate discussion
- If a comment is already addressed (the code no longer has the issue), skip it
- If a comment is ambiguous, make a best-effort fix and leave a reply on the comment explaining what was done
- Only address findings with severity **critical** or **warning** — suggestions are informational only
