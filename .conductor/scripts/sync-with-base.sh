#!/usr/bin/env bash
set -uo pipefail

BASE="${FEATURE_BASE_BRANCH:-main}"

git fetch origin

if [ -z "$(git log HEAD..origin/${BASE} --oneline)" ]; then
  cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["is_up_to_date"], "context": "Branch is already up to date with origin/${BASE}"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
  exit 0
fi

rebase_exit=0
git rebase origin/${BASE} || rebase_exit=$?

if [ $rebase_exit -eq 0 ]; then
  cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": "Rebased onto origin/${BASE}"}
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
