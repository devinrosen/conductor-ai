#!/usr/bin/env bash
set -euo pipefail

if [ -f docs/diagrams/personas.md ]; then
  cat <<'EOF'
<<<FLOW_OUTPUT>>>
{"markers": ["personas_exist"], "context": "personas.md exists"}
<<<END_FLOW_OUTPUT>>>
EOF
else
  cat <<'EOF'
<<<FLOW_OUTPUT>>>
{"markers": [], "context": "personas.md does not exist"}
<<<END_FLOW_OUTPUT>>>
EOF
fi
