#!/usr/bin/env bash
set -euo pipefail

base="${FEATURE_BASE_BRANCH:-main}"

# Fetch latest base ref for accurate comparison
git fetch origin "$base" --quiet

# Early exit if no commits ahead of base
ahead=$(git rev-list --count "origin/$base..HEAD")
if [ "$ahead" -eq 0 ]; then
  cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["no_changes"], "context": "No commits ahead of $base — nothing to push or PR"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
  exit 0
fi

SKIP_E2E=1 git push -u origin HEAD

pr_create_err=$(mktemp)
if pr_url=$(gh pr create --fill --base "$base" 2>"$pr_create_err"); then
  : # pr_url already set from stdout
else
  exit_code=$?
  if grep -qi "already exists" "$pr_create_err"; then
    pr_url=$(gh pr view --json url -q .url)
  else
    cat "$pr_create_err" >&2
    rm -f "$pr_create_err"
    exit $exit_code
  fi
fi
rm -f "$pr_create_err"

cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": "PR is open at $pr_url"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
