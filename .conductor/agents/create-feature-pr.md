# Create Feature PR

Create a pull request for the current branch against the feature/release base branch.

## Steps

1. Run `SKIP_E2E=1 git push -u origin HEAD` to ensure the branch is pushed
2. Run `gh pr create --fill --base "{{feature_base_branch}}"` to open the PR
3. If a PR already exists for this branch, retrieve and report its URL with `gh pr view --json url -q .url`
4. Output the PR URL in the summary

## Context

- Base branch: `{{feature_base_branch}}`
- This step runs after all child worktrees have been merged into the feature branch
