#!/usr/bin/env bash
set -euo pipefail

git push -u origin HEAD

pr_url=$(gh pr create --fill 2>/dev/null || gh pr view --json url -q .url)

cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": "PR is open at $pr_url"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
