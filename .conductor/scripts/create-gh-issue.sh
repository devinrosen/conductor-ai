#!/usr/bin/env bash
set -euo pipefail

# Create the GitHub issue drafted by draft-gh-issue.
# Env: PRIOR_OUTPUT — draft-gh-issue's structured output (gh-issue-draft schema):
#        {title, labels[], assignee?, body, context}
#      DRY_RUN — "true" prints what would be created instead of creating it.
# The issue is created in the repo of the current checkout (owner/name from
# `gh repo view`), not the conductor repo slug, which `gh --repo` can't use.

PRIOR_OUTPUT="${PRIOR_OUTPUT:-}"
DRY_RUN="${DRY_RUN:-false}"

emit_output() {
  local markers="$1"
  local context="$2"
  local output
  output=$(jq -n --argjson markers "$markers" --arg context "$context" \
    '{"markers": $markers, "context": $context}')
  printf '<<<FLOW_OUTPUT>>>\n%s\n<<<END_FLOW_OUTPUT>>>\n' "$output"
}

fail() {
  echo "ERROR: $1" >&2
  emit_output '[]' "$1"
  exit 1
}

if [ -z "$PRIOR_OUTPUT" ] || [[ "$PRIOR_OUTPUT" == *"{{"* ]] \
  || ! echo "$PRIOR_OUTPUT" | jq -e 'type == "object"' >/dev/null 2>&1; then
  fail "No structured draft from draft-gh-issue (PRIOR_OUTPUT is empty or not a JSON object)"
fi

TITLE=$(echo "$PRIOR_OUTPUT" | jq -r '.title // "" | strings')
[ -n "$TITLE" ] || fail "The draft has no title"

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
echo "$PRIOR_OUTPUT" | jq -r '.body // "" | strings' > "$tmp"
[ -s "$tmp" ] || fail "The draft has no body"

ASSIGNEE=$(echo "$PRIOR_OUTPUT" | jq -r '.assignee // "" | strings')

LABEL_ARGS=()
while IFS= read -r label; do
  [ -n "$label" ] && LABEL_ARGS+=(--label "$label")
done < <(echo "$PRIOR_OUTPUT" | jq -r '(.labels // [])[] | strings')

ASSIGNEE_ARGS=()
if [ -n "$ASSIGNEE" ] && [ "$ASSIGNEE" != "unassigned" ]; then
  ASSIGNEE_ARGS=(--assignee "$ASSIGNEE")
fi

OWNER_REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner) \
  || fail "Could not resolve the GitHub owner/name for this repo"

if [ "$DRY_RUN" = "true" ]; then
  echo "DRY RUN: would create in ${OWNER_REPO}: ${TITLE} ${LABEL_ARGS[*]+"${LABEL_ARGS[*]}"} ${ASSIGNEE_ARGS[*]+"${ASSIGNEE_ARGS[*]}"}" >&2
  cat "$tmp" >&2
  emit_output '[]' "Dry run: would create issue in ${OWNER_REPO}: ${TITLE}"
  exit 0
fi

if ! ISSUE_URL=$(gh issue create \
  --repo "$OWNER_REPO" \
  --title "$TITLE" \
  --body-file "$tmp" \
  "${LABEL_ARGS[@]+"${LABEL_ARGS[@]}"}" \
  "${ASSIGNEE_ARGS[@]+"${ASSIGNEE_ARGS[@]}"}"); then
  fail "gh issue create failed in ${OWNER_REPO}"
fi
[ -n "$ISSUE_URL" ] || fail "gh issue create returned no URL"

ISSUE_NUMBER=$(echo "$ISSUE_URL" | sed -E 's|.*/issues/([0-9]+)$|\1|')

emit_output '["issue_created"]' "Created issue #${ISSUE_NUMBER} in ${OWNER_REPO}: ${TITLE} — ${ISSUE_URL}"
