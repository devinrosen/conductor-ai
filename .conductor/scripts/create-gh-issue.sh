#!/usr/bin/env bash
set -euo pipefail

PRIOR_OUTPUT="${PRIOR_OUTPUT:-}"
REPO="${REPO:-}"

emit_output() {
  local markers="$1"
  local context="$2"
  local output
  output=$(jq -n --argjson markers "$markers" --arg context "$context" \
    '{"markers": $markers, "context": $context}')
  printf '<<<CONDUCTOR_OUTPUT>>>\n%s\n<<<END_CONDUCTOR_OUTPUT>>>\n' "$output"
}

# Parse title from "Draft issue: <title>" line
TITLE=$(echo "$PRIOR_OUTPUT" | grep -m1 '^Draft issue:' | sed 's/^Draft issue: //')

if [ -z "$TITLE" ]; then
  emit_output '[]' "Failed to parse title from prior output"
  exit 1
fi

# Parse labels from "Labels: <comma-separated or empty>" line
LABELS_LINE=$(echo "$PRIOR_OUTPUT" | grep -m1 '^Labels:' | sed 's/^Labels: //')

# Parse body — everything after the "Body:" line, stripping trailing CONDUCTOR_OUTPUT block
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT

echo "$PRIOR_OUTPUT" \
  | sed -n '/^Body:$/,$ p' \
  | tail -n +2 \
  | sed '/^<<<CONDUCTOR_OUTPUT>>>/,$ d' \
  > "$tmp"

# Build label args
LABEL_ARGS=()
if [ -n "$LABELS_LINE" ]; then
  IFS=',' read -ra LABEL_LIST <<< "$LABELS_LINE"
  for label in "${LABEL_LIST[@]}"; do
    label="$(echo "$label" | xargs)"  # trim whitespace
    if [ -n "$label" ]; then
      LABEL_ARGS+=(--label "$label")
    fi
  done
fi

# Create the issue
ISSUE_URL=$(gh issue create \
  --repo "$REPO" \
  --title "$TITLE" \
  --body-file "$tmp" \
  "${LABEL_ARGS[@]+"${LABEL_ARGS[@]}"}")

if [ -z "$ISSUE_URL" ]; then
  emit_output '[]' "gh issue create returned no URL"
  exit 1
fi

# Extract issue number from URL
ISSUE_NUMBER=$(echo "$ISSUE_URL" | sed -E 's|.*/issues/([0-9]+)$|\1|')

emit_output '["issue_created"]' "Created issue #${ISSUE_NUMBER}: ${TITLE} — ${ISSUE_URL}"
