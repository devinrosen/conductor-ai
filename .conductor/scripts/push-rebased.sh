#!/usr/bin/env bash
set -euo pipefail

git push --force-with-lease origin HEAD
branch=$(git rev-parse --abbrev-ref HEAD)

cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["pulled_new_commits"], "context": "Pushed rebased branch: $branch"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
