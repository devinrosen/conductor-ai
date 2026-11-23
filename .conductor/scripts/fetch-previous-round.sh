#!/usr/bin/env bash
# fetch-previous-round.sh — the previous conductor review's blocking findings, for the reviewers (#3465)
# Runs in review-pr before the reviewer swarm. Env vars: PR_NUMBER, TRUSTED_REVIEW_BOTS
# (comma-separated bot logins whose reviews are trusted; see lib-trusted-review.sh).
#
# Emits a FLOW_OUTPUT block with a `previous_round` string field, which becomes
# {{previous_round}} in the shared review-diff-scope prompt: the blocking and answered
# findings from the latest non-dismissed conductor review by a trusted author, read from
# the data payload submit-review.sh embeds. Without it, each incremental round reviews
# only the new commits and can flag the fix for its own previous finding (#3461), or turn
# a request it made conditional into a blocking one (#3464).
#
# Always emits the field, since exported variables persist across loop iterations
# (iterate-pr calls review-pr in a loop). Never fails the review: on any error the field
# says the previous round is unavailable.
set -euo pipefail

# shellcheck source=lib-trusted-review.sh
source "$(dirname "$0")/lib-trusted-review.sh"

MAX_FINDINGS=30
MAX_MESSAGE_CHARS=600

emit() {
  # $1 = context, $2 = previous_round text
  jq -cn --arg context "$1" --arg prev "$2" '{markers: [], context: $context, previous_round: $prev}' \
    | { printf '<<<FLOW_OUTPUT>>>\n'; cat; printf '<<<END_FLOW_OUTPUT>>>\n'; }
}
NONE="None: this is the first conductor review of this PR."

# pr_number is injected only when the branch has a PR; not a declared input.
if [ -z "${PR_NUMBER:-}" ] || [[ "${PR_NUMBER}" == *"{{"* ]]; then
  PR_NUMBER=$(gh pr view --json number -q .number 2>/dev/null || true)
fi
if ! [[ "${PR_NUMBER:-}" =~ ^[0-9]+$ ]]; then
  emit "No PR number; no previous round." "${NONE}"
  exit 0
fi

OWNER_REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || true)
if [ -z "${OWNER_REPO}" ]; then
  emit "Could not resolve the repository." "Unavailable: could not resolve the repository."
  exit 0
fi
if ! REVIEWS=$(gh api --paginate "repos/${OWNER_REPO}/pulls/${PR_NUMBER}/reviews" --jq '.[]' 2>/dev/null | jq -s '.'); then
  emit "Could not fetch reviews for PR #${PR_NUMBER}." "Unavailable: could not fetch the PR's reviews."
  exit 0
fi

# Latest conductor review that isn't dismissed and comes from a trusted author: anyone
# can post a review body with the marker on a public PR, and this text goes into every
# reviewer's prompt.
LAST=$(echo "${REVIEWS}" | jq -c --arg bots "${TRUSTED_REVIEW_BOTS:-}" "$(trusted_author_jq)"'
  [.[] | select((.body // "") | contains("<!-- conductor-review -->"))
       | select((.state // "") != "DISMISSED")
       | select(trusted_author($bots))]
  | sort_by(.submitted_at // "") | last // empty')
if [ -z "${LAST}" ]; then
  emit "No earlier trusted conductor review on PR #${PR_NUMBER}." "${NONE}"
  exit 0
fi

PAYLOAD=$(echo "${LAST}" | jq -c '
  (.body // "") | capture("<!-- conductor-review-data:v1 (?<d>[A-Za-z0-9+/=]+) -->").d
  | @base64d | fromjson
  | select((.blocking_findings | type) == "array")' 2>/dev/null || true)
if [ -z "${PAYLOAD}" ]; then
  emit "The latest conductor review has no readable review data." "Unavailable: the latest conductor review has no readable review data."
  exit 0
fi

TEXT=$(jq -rn --argjson review "${LAST}" --argjson p "${PAYLOAD}" \
  --argjson max "${MAX_FINDINGS}" --argjson maxchars "${MAX_MESSAGE_CHARS}" '
  def trim: if length > $maxchars then .[0:$maxchars] + " [truncated]" else . end;
  def line: "- [\(.severity // "warning")] \(.reviewer // "reviewer") — \(.file // "?"):\(.line // "?") — \((.message // "") | gsub("\\s+"; " ") | trim)";
  ($p.blocking_findings // []) as $b
  | ($p.answered_findings // []) as $a
  | "Conductor review on \(($review.commit_id // "")[0:8]) (scope \($p.scope // "full")): "
    + (if ($b | length) == 0 then "All Clear, no blocking findings." else "\($b | length) blocking finding(s)." end)
    + (if ($b | length) > 0 then "\n" + ([$b[:$max][] | line] | join("\n")) else "" end)
    + (if ($b | length) > $max then "\n- … \(($b | length) - $max) more not shown" else "" end)
    + (if ($a | length) > 0 then "\nAnswered by a reply in that round:\n" + ([$a[:$max][] | line] | join("\n")) else "" end)')

COUNT=$(echo "${PAYLOAD}" | jq '.blocking_findings | length')
emit "Previous conductor review on PR #${PR_NUMBER}: ${COUNT} blocking finding(s)." "${TEXT}"
