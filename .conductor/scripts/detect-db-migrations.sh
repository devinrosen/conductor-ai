#!/usr/bin/env bash
set -euo pipefail

# BASE_BRANCH, SCOPE and SINCE are injected by the workflow. BASE_BRANCH comes
# from the resolve-pr-base step; there is no silent fallback to a guessed base
# (#2736). An incremental review decides from the commits since the last review.
# shellcheck source=lib-diff-range.sh
source "$(dirname "$0")/lib-diff-range.sh"
resolve_diff_range

# -z gives NUL-separated, unquoted names.
# Command substitution would drop the NULs, so go through a temp file.
diff_tmp=$(mktemp)
trap 'rm -f "$diff_tmp"' EXIT
changed_files_to "$diff_tmp"

# Filter for migration files
migration_files=()
while IFS= read -r -d '' file; do
  [[ -z "$file" ]] && continue
  [[ "$file" == conductor-core/src/db/migrations/* ]] && migration_files+=("$file")
done < "$diff_tmp"

count=${#migration_files[@]}

# jq escapes every character in the file names; a hand-built JSON string would
# be invalid for names with quotes or backslashes and the engine would drop the
# markers without any error.
emit() {
  echo "<<<FLOW_OUTPUT>>>"
  jq -cn --argjson markers "$1" --arg ctx "$2" '{markers: $markers, context: $ctx}'
  echo "<<<END_FLOW_OUTPUT>>>"
}

if [ "$count" -gt 0 ]; then
  file_list=$(IFS=", "; echo "${migration_files[*]}")
  emit '["has_db_migrations"]' "Found ${count} migration file(s) in ${DIFF_DESC}: ${file_list}"
else
  emit '[]' "No migration files in ${DIFF_DESC}"
fi
