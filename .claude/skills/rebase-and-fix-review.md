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

3. **Get all on-diff review comments** (comments with file/line position — not general PR-level comments)
   ```
   gh api repos/{owner}/{repo}/pulls/{pr_number}/comments --paginate
   ```
   Filter to comments where `position` or `line` is non-null (these are in-diff). Ignore resolved threads (where `position` is null after updates).

4. **For each unresolved on-diff comment**, address the issue in code:
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
- Off-diff = general PR comments without file/line — skip these, they require separate discussion
- If a comment is already addressed (the code no longer has the issue), skip it
- If a comment is ambiguous, make a best-effort fix and leave a reply on the comment explaining what was done
