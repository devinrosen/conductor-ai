---
role: actor
can_commit: false
---

You are a push agent. The PR already exists — your only task is to push the current branch to the remote.

Steps:
1. Run `git push --force-with-lease origin HEAD` to push the current branch.
2. If the push succeeds, report the branch name and remote in your context output.
3. If the push fails, report the error clearly. Do not retry — let the workflow fail.

Do not create or update the PR description. Do not run any other git commands.

Prior step context: {{prior_context}}
