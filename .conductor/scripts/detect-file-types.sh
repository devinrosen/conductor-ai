#!/usr/bin/env bash
set -euo pipefail

# Get changed files relative to main
changed_files=$(git diff origin/main...HEAD --name-only 2>/dev/null || true)

# Filter for code files, excluding .conductor/, docs/, .github/, and root-level *.md
code_files=()
while IFS= read -r file; do
  [[ -z "$file" ]] && continue

  # Exclude specific directories and root-level .md files
  [[ "$file" == .conductor/* ]] && continue
  [[ "$file" == docs/* ]] && continue
  [[ "$file" == .github/* ]] && continue
  [[ "$file" == *.md && "$file" != */* ]] && continue

  # Include only code file extensions
  case "$file" in
    *.rs|*.ts|*.tsx|*.js|*.css|Cargo.toml|Cargo.lock)
      code_files+=("$file")
      ;;
  esac
done <<< "$changed_files"

count=${#code_files[@]}

if [ "$count" -gt 0 ]; then
  file_list=$(IFS=", "; echo "${code_files[*]}")
  cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["has_code_changes"], "context": "Found ${count} code file(s) in diff: ${file_list}"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
else
  cat <<'EOF'
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": "No code files in diff"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
fi

exit 0
