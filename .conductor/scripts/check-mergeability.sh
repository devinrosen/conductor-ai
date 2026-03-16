#!/usr/bin/env bash
set -euo pipefail

max_attempts=3
attempt=0

while [ $attempt -lt $max_attempts ]; do
  mergeable=$(gh pr view --json mergeable -q .mergeable 2>/dev/null || echo "UNKNOWN")

  if [ "$mergeable" != "UNKNOWN" ]; then
    break
  fi

  attempt=$((attempt + 1))
  if [ $attempt -lt $max_attempts ]; then
    sleep 5
  fi
done

if [ "$mergeable" = "CONFLICTING" ]; then
  cat <<'EOF'
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["has_conflicts"], "context": "PR is CONFLICTING — rebase needed"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
else
  cat <<'EOF'
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": "PR is mergeable — no rebase needed"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
fi

exit 0
