#!/usr/bin/env bash
set -euo pipefail

git add docs/diagrams/

if git diff --cached --quiet; then
  cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": "No diagram changes to commit"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
else
  committed_files=$(git diff --cached --name-only)
  git commit -m "docs: update diagrams for ticket $TICKET"
  cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": "Committed diagram files: $committed_files"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
fi
