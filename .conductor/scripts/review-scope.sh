#!/usr/bin/env bash
# review-scope.sh — choose what the next review-pr run in iterate-pr covers
# Called as a script step before each review-pr call. Env vars: PR_NUMBER,
# FORCE_REVIEW (only the exact string "true" forces a re-review of an unchanged HEAD)
#
# Emits `review_scope` ("full" or "incremental") and `review_since` (a commit
# SHA, or "") as FLOW_OUTPUT fields, exposed to later steps as
# {{review_scope}} / {{review_since}}, and `reuse_review_id` ({{reuse_review_id}},
# empty unless reusing; every branch emits it because output variables persist
# across loop iterations).
#
# - incremental, since <sha>: the most recent conductor review on this PR was
#   submitted on <sha>, and <sha> is still an ancestor of HEAD. The next review
#   covers <sha>..HEAD, i.e. every commit pushed since the last review, however
#   many there are (address-reviews, fmt and lint commits).
# - reuse (marker `reuse_review`): HEAD is the commit the latest conductor review
#   was submitted on, so re-reviewing identical code only produces new,
#   non-deterministic findings (#3307). The review is reused when it isn't
#   DISMISSED, its author is OWNER/MEMBER/COLLABORATOR and its body carries a
#   readable conductor-review-data payload (see submit-review.sh). The scope
#   fields then carry that review's own scope, for a pushback-only re-review.
#   FORCE_REVIEW=true, or any failed check, gives a full review instead.
# - full: no earlier conductor review, its commit is no longer in HEAD's
#   history (rebase, force-push), or anything here fails. The first review of a
#   PR always covers the whole PR, whatever the commit layout.
set -euo pipefail

emit() {
  # $1 = scope, $2 = since, $3 = context, $4 = reused review id (optional)
  jq -cn --arg scope "$1" --arg since "$2" --arg context "$3" --arg reuse "${4:-}" \
    '{markers: (if $reuse != "" then ["reuse_review"] else [] end), context: $context,
      review_scope: $scope, review_since: $since, reuse_review_id: $reuse}' \
    | { printf '<<<FLOW_OUTPUT>>>\n'; cat; printf '<<<END_FLOW_OUTPUT>>>\n'; }
}

if [ -z "${PR_NUMBER:-}" ] || [[ "${PR_NUMBER}" == *"{{"* ]]; then
  PR_NUMBER=$(gh pr view --json number -q .number 2>/dev/null || true)
fi
if ! [[ "${PR_NUMBER:-}" =~ ^[0-9]+$ ]]; then
  emit full "" "No PR number; reviewing the full PR."
  exit 0
fi

OWNER_REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || true)
if [ -z "${OWNER_REPO}" ] \
  || ! REVIEWS=$(gh api --paginate "repos/${OWNER_REPO}/pulls/${PR_NUMBER}/reviews" --jq '.[]' 2>/dev/null | jq -s '.'); then
  emit full "" "Could not read earlier reviews; reviewing the full PR."
  exit 0
fi

LATEST=$(echo "${REVIEWS}" | jq -c '
  [.[] | select((.body // "") | contains("<!-- conductor-review -->"))]
  | sort_by(.submitted_at) | last // {}')
SINCE=$(echo "${LATEST}" | jq -r '.commit_id // empty')

if [ -z "${SINCE}" ]; then
  emit full "" "No earlier conductor review on PR #${PR_NUMBER}; reviewing the full PR."
  exit 0
fi
if ! git merge-base --is-ancestor "${SINCE}" HEAD 2>/dev/null; then
  emit full "" "Last reviewed commit ${SINCE:0:8} is not in HEAD's history (rebase or force-push); reviewing the full PR."
  exit 0
fi
if [ "$(git rev-parse HEAD)" = "$(git rev-parse "${SINCE}")" ]; then
  if [ "${FORCE_REVIEW:-}" = "true" ]; then
    emit full "" "No commits since the last review (${SINCE:0:8}) but force_review is set; reviewing the full PR."
    exit 0
  fi
  REVIEW_ID=$(echo "${LATEST}" | jq -r '.id // empty')
  if [ "$(echo "${LATEST}" | jq -r '.state // ""')" = "DISMISSED" ]; then
    emit full "" "The last conductor review (${SINCE:0:8}) was dismissed; reviewing the full PR."
    exit 0
  fi
  if ! echo "${LATEST}" | jq -e '.author_association as $a | ["OWNER","MEMBER","COLLABORATOR"] | index($a)' >/dev/null; then
    emit full "" "The last conductor review (${SINCE:0:8}) is not from a trusted author; reviewing the full PR."
    exit 0
  fi
  PAYLOAD=$(echo "${LATEST}" | jq -c '
    (.body // "") | capture("<!-- conductor-review-data:v1 (?<d>[A-Za-z0-9+/=]+) -->").d
    | @base64d | fromjson
    | select((.blocking_findings | type) == "array" and (.overall_approved | type) == "boolean")' 2>/dev/null || true)
  if [ -z "${REVIEW_ID}" ] || [ -z "${PAYLOAD}" ]; then
    emit full "" "The last conductor review (${SINCE:0:8}) has no reusable review data; reviewing the full PR."
    exit 0
  fi
  R_SCOPE=$(echo "${PAYLOAD}" | jq -r '.scope // "full"')
  R_SINCE=$(echo "${PAYLOAD}" | jq -r '.since // ""')
  if [ "${R_SCOPE}" != "incremental" ] || [ -z "${R_SINCE}" ] \
    || ! git merge-base --is-ancestor "${R_SINCE}" HEAD 2>/dev/null; then
    R_SCOPE="full"
    R_SINCE=""
  fi
  emit "${R_SCOPE}" "${R_SINCE}" "HEAD is the commit the last conductor review covered (${SINCE:0:8}); reusing review ${REVIEW_ID} instead of re-reviewing (#3307)." "${REVIEW_ID}"
  exit 0
fi

COUNT=$(git rev-list --count "${SINCE}..HEAD")
emit incremental "${SINCE}" "Reviewing ${COUNT} commit(s) since the last review (${SINCE:0:8})."
