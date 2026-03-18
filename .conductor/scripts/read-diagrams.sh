#!/usr/bin/env bash
set -euo pipefail

diagram_dir="docs/diagrams"
shopt -s nullglob
mmd_files=("$diagram_dir"/*.mmd)
shopt -u nullglob

if [ ${#mmd_files[@]} -eq 0 ]; then
  echo "❌ No diagram files found in $diagram_dir/." >&2
  echo "Run \`generate-diagrams\` first to create the diagram files." >&2
  exit 1
fi

# Sort alphabetically
IFS=$'\n' sorted=($(sort <<<"${mmd_files[*]}")); unset IFS

# Concatenate all diagrams, labeled by filename
content=""
for f in "${sorted[@]}"; do
  basename=$(basename "$f")
  content+="--- ${basename} ---"$'\n'
  content+=$(cat "$f")
  content+=$'\n\n'
done

# Escape content for JSON
json_content=$(printf '%s' "$content" | python3 -c 'import sys,json; print(json.dumps(sys.stdin.read()))')

cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": ${json_content}}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
