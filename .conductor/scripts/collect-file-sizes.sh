#!/usr/bin/env bash
set -euo pipefail

FOCUS="${FOCUS:-}"
THRESHOLD_LINES="${THRESHOLD_LINES:-1500}"

# Enumerate files via git ls-files, fall back to find
if git ls-files &>/dev/null; then
  all_files=$(git ls-files)
else
  all_files=$(find . -type f | sed 's|^\./||')
fi

# Apply exclusions
filtered=$(echo "$all_files" | grep -v -E \
  '(^|/)target/|(^|/)node_modules/|(^|/)\.git/|(^|/)dist/|(^|/)build/|(^|/)vendor/|(^|/)\.conductor/' | \
  grep -v -E '\.generated\.|\.min\.js$|\.min\.css$|\.lock$|\.snap$|^package-lock\.json$' || true)

# Filter by FOCUS extensions if set
if [ -n "$FOCUS" ]; then
  ext_pattern=$(echo "$FOCUS" | tr ',' '|' | sed 's/\./\\./g')
  filtered=$(echo "$filtered" | grep -E "\.(${ext_pattern})$" || true)
fi

emit_empty() {
  local focus="$1"
  local output
  output=$(jq -n --arg focus "$focus" \
    '{"markers": [], "context": ("No source files found after applying exclusions and focus filter \"" + $focus + "\".")}')
  printf '<<<CONDUCTOR_OUTPUT>>>\n%s\n<<<END_CONDUCTOR_OUTPUT>>>\n' "$output"
}

if [ -z "$filtered" ]; then
  emit_empty "$FOCUS"
  exit 0
fi

# Count lines per file using wc -l, sort descending, cap at 200
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT

while IFS= read -r file; do
  [ -f "$file" ] || continue
  lines=$(wc -l < "$file" 2>/dev/null || echo 0)
  printf '%d\t%s\n' "$lines" "$file"
done <<< "$filtered" | sort -rn | head -200 > "$tmp"

row_count=$(wc -l < "$tmp" | tr -d ' ')

if [ "$row_count" -eq 0 ]; then
  emit_empty "$FOCUS"
  exit 0
fi

table_rows=$(awk -F'\t' '{printf "| %s | %s |\n", $2, $1}' "$tmp")

context=$(printf 'Found %s source files. Threshold: %s lines. Focus filter: "%s" (empty = all).\n\n| File | Lines |\n|------|-------|\n%s' \
  "$row_count" "$THRESHOLD_LINES" "$FOCUS" "$table_rows")

output=$(jq -n --arg context "$context" '{"markers": ["has_files"], "context": $context}')

printf '<<<CONDUCTOR_OUTPUT>>>\n%s\n<<<END_CONDUCTOR_OUTPUT>>>\n' "$output"
