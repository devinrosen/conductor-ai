#!/usr/bin/env bash
set -euo pipefail

PRIOR_OUTPUT="${PRIOR_OUTPUT:-}"
TICKET_URL="${TICKET_URL:-}"
TICKET_SOURCE_ID="${TICKET_SOURCE_ID:-}"

# Parse owner, repo, and issue number from TICKET_URL
# Expected format: https://github.com/<owner>/<repo>/issues/<number>
OWNER=$(echo "$TICKET_URL" | sed -E 's|https://github.com/([^/]+)/([^/]+)/issues/([0-9]+)|\1|')
REPO=$(echo "$TICKET_URL" | sed -E 's|https://github.com/([^/]+)/([^/]+)/issues/([0-9]+)|\2|')
ISSUE_NUMBER=$(echo "$TICKET_URL" | sed -E 's|https://github.com/([^/]+)/([^/]+)/issues/([0-9]+)|\3|')

# Validate that URL parsing succeeded — sed returns the original string if pattern doesn't match
if [ -z "$OWNER" ] || [ -z "$REPO" ] || [ -z "$ISSUE_NUMBER" ] || \
   [ "$OWNER" = "$TICKET_URL" ] || ! echo "$ISSUE_NUMBER" | grep -qE '^[0-9]+$'; then
  echo "ERROR: Could not parse OWNER/REPO/ISSUE_NUMBER from TICKET_URL: ${TICKET_URL}" >&2
  exit 1
fi

emit_output() {
  local markers="$1"
  local context="$2"
  local output
  output=$(jq -n --argjson markers "$markers" --arg context "$context" \
    '{"markers": $markers, "context": $context}')
  printf '<<<CONDUCTOR_OUTPUT>>>\n%s\n<<<END_CONDUCTOR_OUTPUT>>>\n' "$output"
}

# Detect verdict — check SHOULD CLOSE before NOT READY before READY to avoid substring collisions
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT

if echo "$PRIOR_OUTPUT" | grep -q "SHOULD CLOSE"; then
  VERDICT="SHOULD_CLOSE"
  LABEL="pending-close"
  LABEL_COLOR="d93f0b"
  LABEL_DESC="Ticket is invalid, resolved, or no longer actionable"
  MARKERS='["should_close"]'
  printf '## ⚠️ Pending Close\n\n%s\n' "$PRIOR_OUTPUT" > "$tmp"
elif echo "$PRIOR_OUTPUT" | grep -q "NOT READY"; then
  VERDICT="NOT_READY"
  LABEL="needs-work"
  LABEL_COLOR="e4e669"
  LABEL_DESC="Ticket requires clarification before implementation"
  MARKERS='["has_open_questions"]'
  printf '## ❓ Open Questions\n\nThe following questions or issues must be resolved before this ticket can be handed off to an autonomous agent:\n\n%s\n' "$PRIOR_OUTPUT" > "$tmp"
elif echo "$PRIOR_OUTPUT" | grep -q "READY"; then
  VERDICT="READY"
  LABEL="qualified"
  LABEL_COLOR="0075ca"
  LABEL_DESC="Ticket is ready for autonomous implementation"
  MARKERS='["ticket_ready"]'
  printf '## ✅ Ready for Implementation\n\n%s\n' "$PRIOR_OUTPUT" > "$tmp"
else
  emit_output '[]' "Could not determine verdict from prior output — no SHOULD CLOSE, NOT READY, or READY found"
  exit 0
fi

# Post comment using --body-file to safely handle multi-line content
gh issue comment "$ISSUE_NUMBER" --repo "$OWNER/$REPO" --body-file "$tmp"

# shellcheck source=lib-labels.sh
source "$(dirname "$0")/lib-labels.sh"
apply_exclusive_label "$OWNER" "$REPO" "$ISSUE_NUMBER" "$LABEL" "$LABEL_COLOR" "$LABEL_DESC"

if [ "$VERDICT" = "READY" ]; then
  CONTEXT_MSG="Posted READY comment and applied 'qualified' label to ticket #${ISSUE_NUMBER}"
elif [ "$VERDICT" = "NOT_READY" ]; then
  CONTEXT_MSG="Posted NOT READY comment and applied 'needs-work' label to ticket #${ISSUE_NUMBER}"
else
  CONTEXT_MSG="Posted SHOULD CLOSE comment and applied 'pending-close' label to ticket #${ISSUE_NUMBER}"
fi

emit_output "$MARKERS" "$CONTEXT_MSG"
