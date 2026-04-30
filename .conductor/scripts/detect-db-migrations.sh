#!/usr/bin/env bash
set -euo pipefail

# BASE_BRANCH is injected by the workflow from the resolve-pr-base step.
# We refuse to silently fall back to a guess — a wrong base produces a
# fabricated diff that contaminates the downstream review. See #2736.
if [ -z "${BASE_BRANCH:-}" ]; then
  echo "ERROR: BASE_BRANCH env var not set — workflow must run resolve-pr-base first." >&2
  exit 1
fi

# Get changed files relative to the PR base branch
changed_files=$(git diff "origin/${BASE_BRANCH}...HEAD" --name-only 2>/dev/null || true)

# Filter for migration files
migration_files=()
while IFS= read -r file; do
  [[ -z "$file" ]] && continue
  [[ "$file" == conductor-core/src/db/migrations/* ]] && migration_files+=("$file")
done <<< "$changed_files"

count=${#migration_files[@]}

if [ "$count" -gt 0 ]; then
  file_list=$(IFS=", "; echo "${migration_files[*]}")
  cat <<EOF
<<<FLOW_OUTPUT>>>
{"markers": ["has_db_migrations"], "context": "Found ${count} migration file(s) in diff: ${file_list}"}
<<<END_FLOW_OUTPUT>>>
EOF
else
  cat <<'EOF'
<<<FLOW_OUTPUT>>>
{"markers": [], "context": "No migration files in diff"}
<<<END_FLOW_OUTPUT>>>
EOF
fi
