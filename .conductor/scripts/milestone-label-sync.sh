#!/usr/bin/env bash
set -euo pipefail

# Usage: milestone-label-sync.sh <milestone-number> <label>
# Assigns <label> to all open issues in the given milestone so they can be
# targeted by: foreach { over = tickets scope = { label = "<label>" } }

MILESTONE="${1:?Usage: $0 <milestone-number> <label>}"
LABEL="${2:?Usage: $0 <milestone-number> <label>}"

gh issue list --milestone "$MILESTONE" --state open --json number --jq '.[].number' \
  | xargs -I{} gh issue edit {} --add-label "$LABEL"

echo "Labeled all open milestone $MILESTONE issues with '$LABEL'"
