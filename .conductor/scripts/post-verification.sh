#!/usr/bin/env bash
# post-verification.sh — posts verify-and-merge's report on the PR (#3407), and stops
# the run when verification failed.
# Env vars: PRIOR_OUTPUT (verify-acceptance's structured output), PR_NUMBER, HEAD_SHA
# (from check-pr-ready), DRY_RUN (print the comment instead of posting it).
#
# Fails closed: an unreadable report, a head that moved, or a working tree the agent
# changed exits 1 without posting, so nothing the agent altered can be merged.
set -euo pipefail

# shellcheck source=lib-gh-retry.sh
source "$(dirname "$0")/lib-gh-retry.sh"

refuse() { echo "verify-and-merge: $*" >&2; exit 1; }

[[ "${PR_NUMBER:-}" =~ ^[0-9]+$ ]] || refuse "no valid PR number"
[[ "${HEAD_SHA:-}" =~ ^[0-9a-f]{7,40}$ ]] || refuse "no valid head SHA"
[[ "${DRY_RUN:-}" == *"{{"* ]] && DRY_RUN=""

echo "${PRIOR_OUTPUT:-}" | jq -e '(.verified | type) == "boolean" and (.criteria | type) == "array"' >/dev/null 2>&1 \
  || refuse "verification report unreadable (verify-acceptance gave no verified/criteria)"

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
  "Head: `\($sha[0:7])`. Issues: \((.issues // []) | map("#\(.)") | join(", ") | if . == "" then "none" else . end).",
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
  gh_retry gh pr comment "${PR_NUMBER}" --body-file "${BODY_FILE}" >/dev/null || refuse "could not post the report on PR #${PR_NUMBER}"
fi

if [ "${VERIFIED}" != "true" ]; then
  refuse "verification failed: $(echo "${PRIOR_OUTPUT}" | jq -r '.summary // "see the report"'); PR #${PR_NUMBER} left open"
fi

jq -n --arg n "${PR_NUMBER}" '{markers: ["verified"], context: "Verification report posted on PR #\($n)."}' \
  | { printf '<<<FLOW_OUTPUT>>>\n'; cat; printf '<<<END_FLOW_OUTPUT>>>\n'; }
