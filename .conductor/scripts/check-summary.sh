#!/usr/bin/env bash
# check-summary.sh — decide whether verify-and-merge must refresh the PR's ## Summary
# before it squashes (#3471).
# Env vars: PR_NUMBER, HEAD_SHA, BASE_BRANCH, DRY_RUN (unused; the check only reads).
#
# push-and-pr.sh records the head a PR opened with as a trailing
# `<!-- conductor-summary-head: SHA -->` comment in the body; update-pr-summary.sh moves
# it after a refresh. Markers: summary_current (squash as today, no model call) or
# summary_stale (refresh-summary runs).
#   - no `## Summary` section: current (squash-merge's first-paragraph fallback stays).
#   - marker == HEAD_SHA: current. Marker an ancestor of HEAD: stale only when a non-merge
#     commit came after it, so merging the base alone doesn't count.
#   - marker not an ancestor or not in the checkout (rebased, force-pushed): stale.
#   - no marker (hand-opened, --fill, older PR): stale when a non-merge commit on
#     origin/BASE..HEAD is committed after the PR's createdAt.
# A stale result puts the current Summary and the commits in its context, and in the structured output as
# summary_opened_head / summary_commits (summary_ prefix: keys become workflow variables).
# Fails closed: a gh or git error exits 1.
set -euo pipefail

# shellcheck source=lib-gh-retry.sh
source "$(dirname "$0")/lib-gh-retry.sh"

refuse() { echo "verify-and-merge: $*" >&2; exit 1; }

[[ "${PR_NUMBER:-}" =~ ^[0-9]+$ ]] || refuse "no valid PR number"
[[ "${HEAD_SHA:-}" =~ ^[0-9a-f]{7,40}$ ]] || refuse "no valid head SHA"
case "${BASE_BRANCH:-}" in *"{{"*) BASE_BRANCH='' ;; esac
[ -n "${BASE_BRANCH:-}" ] || refuse "no base branch"

emit() { # MARKER CONTEXT [JSON-EXTRA]
  local extra="${3:-}"
  [ -n "${extra}" ] || extra='{}'
  jq -cn --arg m "$1" --arg c "$2" --argjson x "${extra}" '{markers: [$m], context: $c} + $x' \
    | { printf '<<<FLOW_OUTPUT>>>\n'; cat; printf '<<<END_FLOW_OUTPUT>>>\n'; }
}

OWNER_REPO=$(gh_retry gh repo view --json nameWithOwner -q .nameWithOwner) \
  || refuse "could not resolve the repository"
PR=$(gh_retry gh pr view "${PR_NUMBER}" --repo "${OWNER_REPO}" --json body,createdAt) \
  || refuse "could not read PR #${PR_NUMBER}"
BODY=$(echo "${PR}" | jq -r '.body // ""' | tr -d '\r')
CREATED=$(echo "${PR}" | jq -r '.createdAt // ""')

SUMMARY=$(printf '%s\n' "${BODY}" | awk '
  /^## Summary[[:space:]]*$/ { s = 1; next }
  s && /^## / { exit }
  s { print }' | sed -e '/./,$!d')
if [ -z "${SUMMARY}" ]; then
  emit summary_current "No ## Summary section; squash-merge uses the first paragraph as before."
  exit 0
fi

MARKER=$(printf '%s\n' "${BODY}" \
  | sed -n 's/^.*<!-- conductor-summary-head: \([0-9a-f]\{7,40\}\) -->.*$/\1/p' | tail -n 1)

git fetch origin "${BASE_BRANCH}" --quiet 2>/dev/null || true
git rev-parse --verify --quiet "origin/${BASE_BRANCH}" >/dev/null \
  || refuse "origin/${BASE_BRANCH} is not in this checkout"

COMMITS=""   # "<short sha> <subject>" lines, oldest first
if [ -n "${MARKER}" ]; then
  if git rev-parse --verify --quiet "${MARKER}^{commit}" >/dev/null \
     && git merge-base --is-ancestor "${MARKER}" HEAD 2>/dev/null; then
    COMMITS=$(git log --reverse --no-merges --format='%h %s' "${MARKER}..HEAD" --not "origin/${BASE_BRANCH}") \
      || refuse "could not list the commits since ${MARKER:0:7}"
    CURRENT_WHY="the head is still the commit the Summary describes (${MARKER:0:7})"
  else
    COMMITS=$(git log --reverse --no-merges --format='%h %s' "origin/${BASE_BRANCH}..HEAD") \
      || refuse "could not list the commits on the branch"
    [ -n "${COMMITS}" ] || COMMITS="(the branch was rebased; no commits to list)"
    CURRENT_WHY=""
  fi
else
  [ -n "${CREATED}" ] || refuse "PR #${PR_NUMBER} has no createdAt"
  # Not --since: git stops at the first older commit. Compare in epoch seconds.
  CREATED_EPOCH=$(jq -rn --arg t "${CREATED}" '$t | fromdateiso8601') \
    || refuse "unreadable createdAt: ${CREATED}"
  LOG=$(git log --reverse --no-merges --format='%ct %h %s' "origin/${BASE_BRANCH}..HEAD") \
    || refuse "could not list the commits on the branch"
  COMMITS=$(printf '%s\n' "${LOG}" | awk -v c="${CREATED_EPOCH}" 'NF && $1 > c { $1 = ""; sub(/^ /, ""); print }')
  CURRENT_WHY="no commit came after the PR opened"
fi

if [ -z "${COMMITS}" ]; then
  emit summary_current "Summary is current: ${CURRENT_WHY}."
  exit 0
fi

CONTEXT=$(printf 'The head moved after the Summary was written; refresh it.\n\nCurrent Summary:\n%s\n\nCommits since the PR opened (oldest first):\n%s' "${SUMMARY}" "${COMMITS}")
emit summary_stale "${CONTEXT}" \
  "$(jq -cn --arg c "${COMMITS}" --arg h "${MARKER}" '{summary_opened_head: $h, summary_commits: $c}')"
