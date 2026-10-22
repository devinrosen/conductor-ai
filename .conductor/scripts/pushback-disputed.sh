#!/usr/bin/env bash
# pushback-disputed.sh — report that a re-review after triage pushback still blocks
# Called as a script step in iterate-pr before the human_review gate. Env vars: PR_NUMBER
#
# Emits the `needs_human` marker and a context naming the disputed findings
# (the `[Pushback on ...]` comments posted since the previous conductor review).
# Every failure is non-fatal: the gate must always be reached, so a failed
# `gh` call only omits that part of the context.
set -euo pipefail

emit() {
  jq -cn --arg context "$1" '{markers: ["needs_human"], context: $context}' \
    | { printf '<<<FLOW_OUTPUT>>>\n'; cat; printf '<<<END_FLOW_OUTPUT>>>\n'; }
}

if [ -z "${PR_NUMBER:-}" ] || [[ "${PR_NUMBER}" == *"{{"* ]]; then
  PR_NUMBER=$(gh pr view --json number -q .number 2>/dev/null || true)
fi
if ! [[ "${PR_NUMBER:-}" =~ ^[0-9]+$ ]]; then
  emit "Re-review after pushback still requests changes. A human needs to dismiss the review or address the findings."
  exit 0
fi

OWNER_REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || true)
REVIEWS='[]'
COMMENTS='[]'
if [ -n "${OWNER_REPO}" ]; then
  REVIEWS=$(gh api --paginate "repos/${OWNER_REPO}/pulls/${PR_NUMBER}/reviews" --jq '.[]' 2>/dev/null | jq -s '.' 2>/dev/null) || REVIEWS='[]'
  COMMENTS=$(gh api --paginate "repos/${OWNER_REPO}/issues/${PR_NUMBER}/comments" --jq '.[]' 2>/dev/null | jq -s '.' 2>/dev/null) || COMMENTS='[]'
fi

# Conductor reviews, oldest first. The re-review is the last; pushback was
# posted after the one before it.
CONDUCTOR=$(echo "${REVIEWS}" | jq -c '
  [.[] | select((.body // "") | contains("<!-- conductor-review -->"))]
  | sort_by(.submitted_at)' 2>/dev/null) || CONDUCTOR='[]'
REVIEW_URL=$(echo "${CONDUCTOR}" | jq -r 'last | .html_url // empty' 2>/dev/null || true)
PREV_AT=$(echo "${CONDUCTOR}" | jq -r 'if length > 1 then .[-2].submitted_at else "" end' 2>/dev/null || true)

DISPUTED=$(echo "${COMMENTS}" | jq -r --arg since "${PREV_AT}" '
  [.[] | select((.body // "") | startswith("[Pushback on "))
       | select($since == "" or .created_at > $since)
       | (.body | split("\n") | .[0] | split("]:") | .[0] | ltrimstr("[Pushback on "))]
  | join("; ")' 2>/dev/null || true)

CTX="Re-review after pushback still requests changes on PR #${PR_NUMBER}"
[ -n "${REVIEW_URL}" ] && CTX="${CTX} (${REVIEW_URL})"
CTX="${CTX}."
[ -n "${DISPUTED}" ] && CTX="${CTX} Disputed: ${DISPUTED}."
CTX="${CTX} A human needs to dismiss the review or address the findings."
emit "${CTX}"
