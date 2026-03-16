#!/usr/bin/env bash
set -euo pipefail

git push -u origin HEAD

pr_create_err=$(mktemp)
if pr_url=$(gh pr create --fill 2>"$pr_create_err"); then
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
