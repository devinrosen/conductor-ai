#!/usr/bin/env bash
set -uo pipefail

git fetch origin

if [ -z "$(git log HEAD..origin/main --oneline)" ]; then
  cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["is_up_to_date"], "context": "Branch is already up to date with origin/main"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
  exit 0
fi

rebase_exit=0
git rebase origin/main || rebase_exit=$?

if [ $rebase_exit -eq 0 ]; then
  cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": "Rebased onto origin/main"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
  exit 0
fi

conflict_files=$(git diff --name-only --diff-filter=U | tr '\n' ' ' | sed 's/ $//')
git rebase --abort

cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["has_conflicts"], "context": "Rebase conflicted on: $conflict_files"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
exit 0
