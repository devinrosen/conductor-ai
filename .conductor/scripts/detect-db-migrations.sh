#!/usr/bin/env bash
set -euo pipefail

# Get changed files relative to main
changed_files=$(git diff origin/main...HEAD --name-only 2>/dev/null || true)

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
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["has_db_migrations"], "context": "Found ${count} migration file(s) in diff: ${file_list}"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
else
  cat <<'EOF'
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": "No migration files in diff"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
fi
