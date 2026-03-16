#!/usr/bin/env bash
set -euo pipefail

# Resolve PR number from current branch
PR_NUMBER=$(gh pr view --json number -q .number)

# Merge via auto-merge (merge queue); fall back to direct squash if unsupported
if ! gh pr merge --auto --squash --delete-branch 2>/dev/null; then
  gh pr merge --squash --delete-branch
fi

echo "Merged PR #${PR_NUMBER}"

# Close linked issue if TICKET_NUMBER was provided and is a valid number
if [ -n "${TICKET_NUMBER:-}" ] && [[ "${TICKET_NUMBER}" =~ ^#?[0-9]+$ ]]; then
  ISSUE_NUMBER="${TICKET_NUMBER#\#}"
  gh issue close "${ISSUE_NUMBER}"
  gh issue comment "${ISSUE_NUMBER}" --body "Closed by #${PR_NUMBER} (merged)."
  echo "Closed issue #${ISSUE_NUMBER}"
else
  echo "TICKET_NUMBER not set or invalid — skipping issue close."
fi
