#!/usr/bin/env bash
set -euo pipefail

if [ -f docs/diagrams/personas.md ]; then
  cat <<'EOF'
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["personas_exist"], "context": "personas.md exists"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
else
  cat <<'EOF'
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": "personas.md does not exist"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
fi
