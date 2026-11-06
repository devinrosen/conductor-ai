#!/usr/bin/env bash
# reuse-review.sh — hand triage an earlier conductor review instead of re-reviewing (#3307)
# Runs as the parent-level `review-aggregator` step of iterate-pr / quick-fix-pr when
# review-scope.sh found that HEAD is the commit the latest conductor review covers.
# Env vars: PR_NUMBER, REVIEW_ID (from review-scope's reuse_review_id).
#
# Reads the payload submit-review.sh embeds in the review body and emits it in the
# shape aggregate-reviews.sh emits, so `if review-aggregator.has_blocking_findings`
# and triage-reviews work unchanged. Re-checks everything review-scope checked: not
# dismissed, trusted author, commit_id == HEAD, payload readable. Any failure exits
# non-zero; it never invents findings.
set -euo pipefail

# shellcheck source=lib-gh-retry.sh
source "$(dirname "$0")/lib-gh-retry.sh"

fail() { echo "reuse-review: $*" >&2; exit 1; }

if [ -z "${PR_NUMBER:-}" ] || [[ "${PR_NUMBER}" == *"{{"* ]]; then
  PR_NUMBER=$(gh pr view --json number -q .number 2>/dev/null || true)
fi
[[ "${PR_NUMBER:-}" =~ ^[0-9]+$ ]] || fail "no valid PR number"
[[ "${REVIEW_ID:-}" =~ ^[0-9]+$ ]] || fail "no valid review id '${REVIEW_ID:-}'"

OWNER_REPO=$(gh_retry gh repo view --json nameWithOwner -q .nameWithOwner) || fail "could not resolve the repository"
REVIEW=$(gh_retry gh api "repos/${OWNER_REPO}/pulls/${PR_NUMBER}/reviews/${REVIEW_ID}") || fail "could not fetch review ${REVIEW_ID}"

[ "$(echo "${REVIEW}" | jq -r '.state // ""')" != "DISMISSED" ] || fail "review ${REVIEW_ID} was dismissed"
echo "${REVIEW}" | jq -e '.author_association as $a | ["OWNER","MEMBER","COLLABORATOR"] | index($a)' >/dev/null \
  || fail "review ${REVIEW_ID} is not from a trusted author"
HEAD_SHA=$(git rev-parse HEAD)
[ "$(echo "${REVIEW}" | jq -r '.commit_id // ""')" = "${HEAD_SHA}" ] || fail "review ${REVIEW_ID} is not on HEAD (${HEAD_SHA:0:8})"

PAYLOAD=$(echo "${REVIEW}" | jq -c '
  (.body // "") | capture("<!-- conductor-review-data:v1 (?<d>[A-Za-z0-9+/=]+) -->").d
  | @base64d | fromjson
  | select((.blocking_findings | type) == "array" and (.overall_approved | type) == "boolean")' 2>/dev/null || true)
[ -n "${PAYLOAD}" ] || fail "review ${REVIEW_ID} has no readable review data"

echo "${PAYLOAD}" | jq -c --arg id "${REVIEW_ID}" --arg sha "${HEAD_SHA:0:8}" '
  (.blocking_findings) as $blocking
  | (($blocking | length) == 0 and .overall_approved) as $approved
  | {
      markers: (
        (if $approved then ["approved"] else ["has_review_issues"] end)
        + (if ($blocking | length) > 0 then ["has_blocking_findings"] else [] end)
        + (if any($blocking[]; .severity == "warning") then ["has_warnings"] else [] end)),
      context: ("Reused conductor review \($id) on \($sha) (no new review-pr run). " + (.summary // "")),
      reviewed_by: (.reviewed_by // ""),
      blocking_findings: $blocking,
      answered_findings: (.answered_findings // []),
      off_diff_findings: [],
      overall_approved: $approved,
      summary: (.summary // "")
    }' | { printf '<<<FLOW_OUTPUT>>>\n'; cat; printf '<<<END_FLOW_OUTPUT>>>\n'; }
