#!/usr/bin/env bash
set -euo pipefail

personas_file="docs/diagrams/personas.md"

if [ ! -f "$personas_file" ]; then
  echo "❌ $personas_file not found." >&2
  echo "Run \`generate-diagrams\` first to bootstrap the personas file." >&2
  exit 1
fi

content=$(cat "$personas_file")

# If a personas filter is set, prefix it
prefix=""
if [ -n "${PERSONAS:-}" ]; then
  prefix="Active persona filter: ${PERSONAS}\n\n"
fi

# Escape content for JSON: backslashes, double quotes, newlines, tabs
json_content=$(printf '%s' "${prefix}${content}" | python3 -c 'import sys,json; print(json.dumps(sys.stdin.read()))')

cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": ${json_content}}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
