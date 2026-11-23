#!/usr/bin/env bash
# post-verification.sh — posts verify-and-merge's report on the PR (#3407), and stops
# the run when verification failed.
# Env vars: PRIOR_OUTPUT (verify-acceptance's structured output), PR_NUMBER, HEAD_SHA
# (from check-pr-ready), DRY_RUN (print the comment instead of posting it).
#
# Fails closed: an unreadable report, a verified report with a failed criterion, a head that moved, or a working tree the agent
# changed exits 1 without posting, so nothing the agent altered can be merged.
# TRUSTED_REVIEW_BOTS: bot logins whose earlier report counts as already posted.
set -euo pipefail

# shellcheck source=lib-gh-retry.sh
source "$(dirname "$0")/lib-gh-retry.sh"
# shellcheck source=lib-trusted-review.sh
source "$(dirname "$0")/lib-trusted-review.sh"

refuse() { echo "verify-and-merge: $*" >&2; exit 1; }

[[ "${PR_NUMBER:-}" =~ ^[0-9]+$ ]] || refuse "no valid PR number"
[[ "${HEAD_SHA:-}" =~ ^[0-9a-f]{7,40}$ ]] || refuse "no valid head SHA"
[[ "${DRY_RUN:-}" == *"{{"* ]] && DRY_RUN=""

echo "${PRIOR_OUTPUT:-}" | jq -e '(.verified | type) == "boolean" and (.criteria | type) == "array" and all(.criteria[]; type == "object")' >/dev/null 2>&1 \
  || refuse "verification report unreadable (verify-acceptance gave no verified/criteria)"

# A report claiming verified while a criterion failed must never reach the merge.
echo "${PRIOR_OUTPUT}" | jq -e '.verified == true and ([.criteria[] | select(.status == "fail")] | length) > 0' >/dev/null 2>&1 \
  && refuse "report says verified but a criterion has status fail; nothing was merged"

[ "$(git rev-parse HEAD)" = "${HEAD_SHA}" ] || refuse "the branch head moved during verification; re-run verify-and-merge"
[ -z "$(git status --porcelain)" ] || refuse "verify-acceptance left changes in the worktree; nothing was merged"

VERIFIED=$(echo "${PRIOR_OUTPUT}" | jq -r '.verified')
BODY_FILE=$(mktemp "${TMPDIR:-/tmp}/verify_report.XXXXXX")
trap 'rm -f "${BODY_FILE}"' EXIT

# Evidence is agent text: keep table cells to one line and escape the separator.
echo "${PRIOR_OUTPUT}" | jq -r --arg sha "${HEAD_SHA}" '
  def cell: tostring | gsub("\r?\n"; " ") | gsub("\\|"; "\\|");
  "<!-- conductor-verification -->",
  "## verify-and-merge — \(if .verified then "Verified" else "Verification failed" end)",
  "",
  "Head: `\($sha[0:7])`. Verdict: `\(if .verified then "pass" else "fail" end)`. Issues: \((.issues // []) | map("#\(.)") | join(", ") | if . == "" then "none" else . end).",
  "",
  (.summary // ""),
  "",
  "### Acceptance criteria",
  "",
  "| Criterion | Status | Evidence |",
  "| --- | --- | --- |",
  (.criteria[] | "| \(.criterion | cell) | \(.status | cell) | \(.evidence | cell) |"),
  "",
  (if ((.checks // []) | length) > 0 then
     "### Checks run", "",
     ((.checks)[] | "<details><summary>\(.result | ascii_upcase): <code>\(.command | cell)</code></summary>\n\n```\n\(.excerpt // "")\n```\n\n</details>")
   else empty end),
  (if ((.run_ids // []) | length) > 0 then "", "Dry-run smoke runs: " + ((.run_ids) | map("`\(.)`") | join(", ")) else empty end)
' > "${BODY_FILE}"

if [ "${DRY_RUN:-}" = "true" ]; then
  echo "[dry run] would comment on PR #${PR_NUMBER}:"
  cat "${BODY_FILE}"
else
  # A resume or re-run must not post a second report for the same head and verdict;
  # a pass after an earlier fail at the same head still posts.
  VERDICT=pass; [ "${VERIFIED}" = "true" ] || VERDICT=fail
  OWNER_REPO=$(gh_retry gh repo view --json nameWithOwner -q .nameWithOwner) || refuse "could not resolve the repository"
  # Only a report from a trusted author counts (the same rule as check-pr-ready's review
  # check): anyone else's look-alike comment must not suppress the real report.
  EXISTING=$(gh_retry gh api --paginate "repos/${OWNER_REPO}/issues/${PR_NUMBER}/comments" | jq -s --arg sha "${HEAD_SHA:0:7}" --arg verdict "${VERDICT}" --arg bots "${TRUSTED_REVIEW_BOTS:-}" "$(trusted_author_jq)"'
    [ add // [] | .[] | select((.body // "") | contains("<!-- conductor-verification -->"))
      | select(trusted_author($bots))
      | select((.body // "") | contains("Head: `" + $sha + "`. Verdict: `" + $verdict + "`")) ] | length') \
    || refuse "could not list the comments of PR #${PR_NUMBER}"
  if [ "${EXISTING}" -gt 0 ]; then
    echo "A verification report for ${HEAD_SHA:0:7} is already on PR #${PR_NUMBER}; not posting another." >&2
  else
    gh_retry gh pr comment "${PR_NUMBER}" --body-file "${BODY_FILE}" >/dev/null || refuse "could not post the report on PR #${PR_NUMBER}"
  fi
fi

if [ "${VERIFIED}" != "true" ]; then
  refuse "verification failed: $(echo "${PRIOR_OUTPUT}" | jq -r '.summary // "see the report"'); PR #${PR_NUMBER} left open"
fi

jq -n --arg n "${PR_NUMBER}" '{markers: ["verified"], context: "Verification report posted on PR #\($n)."}' \
  | { printf '<<<FLOW_OUTPUT>>>\n'; cat; printf '<<<END_FLOW_OUTPUT>>>\n'; }
