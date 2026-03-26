#!/usr/bin/env bash
set -euo pipefail

echo "=== Building conductor-web frontend ==="
cd conductor-web/frontend && bun install && bun run build
cd ../..

echo "=== Building conductor-web binary ==="
cargo build --bin conductor-web

cat <<'EOF'
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": "Built conductor-web frontend and binary successfully"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
