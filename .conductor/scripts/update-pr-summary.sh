#!/usr/bin/env bash
# update-pr-summary.sh — write refresh-summary's result into the PR body (#3471).
# Env vars: PRIOR_OUTPUT (refresh-summary's structured output), PR_NUMBER, HEAD_SHA,
# DRY_RUN (print the new body instead of editing the PR).
#
# When `changed` is true the `## Summary` section is replaced by the rewrite. Either way the
# trailing `<!-- conductor-summary-head: SHA -->` marker is set to HEAD_SHA, so a re-run at
# this head skips the agent. squash-merge re-reads the body, so the commit and the PR
# agree. Fails closed: unreadable output, a moved head or a failed edit exits 1, because
# merging then would squash a message the PR body doesn't show.
set -euo pipefail

# shellcheck source=lib-gh-retry.sh
source "$(dirname "$0")/lib-gh-retry.sh"

refuse() { echo "verify-and-merge: $*" >&2; exit 1; }

[[ "${PR_NUMBER:-}" =~ ^[0-9]+$ ]] || refuse "no valid PR number"
[[ "${HEAD_SHA:-}" =~ ^[0-9a-f]{7,40}$ ]] || refuse "no valid head SHA"
DRY_RUN="${DRY_RUN:-}"
case "${DRY_RUN}" in *"{{"*) DRY_RUN='' ;; esac

echo "${PRIOR_OUTPUT:-}" | jq -e '(.changed | type) == "boolean"' >/dev/null 2>&1 \
  || refuse "refresh-summary output unreadable (no boolean changed)"
CHANGED=$(echo "${PRIOR_OUTPUT}" | jq -r '.changed')
NEW_SUMMARY=$(echo "${PRIOR_OUTPUT}" | jq -r '.summary // ""' | tr -d '\r')
if [ "${CHANGED}" = "true" ]; then
  printf '%s' "${NEW_SUMMARY}" | grep -q '[^[:space:]]' || refuse "refresh-summary rewrote the Summary to nothing"
  # A heading or comment in the text would break the section or the marker.
  NEW_SUMMARY=$(printf '%s\n' "${NEW_SUMMARY}" | sed -e '/^## Summary[[:space:]]*$/d' -e 's/<!--.*-->//g' | sed -e '/./,$!d')
fi
REASON=$(echo "${PRIOR_OUTPUT}" | jq -r '.reason // ""' | tr '\n' ' ')

[ "$(git rev-parse HEAD)" = "${HEAD_SHA}" ] || refuse "the branch head moved; re-run verify-and-merge"

OWNER_REPO=$(gh_retry gh repo view --json nameWithOwner -q .nameWithOwner) \
  || refuse "could not resolve the repository"
PR=$(gh_retry gh pr view "${PR_NUMBER}" --repo "${OWNER_REPO}" --json body) \
  || refuse "could not read PR #${PR_NUMBER}"
BODY=$(echo "${PR}" | jq -r '.body // ""' | tr -d '\r')

if [ "${CHANGED}" = "true" ]; then
  BODY=$(SUMMARY_TEXT="${NEW_SUMMARY}" awk '
    BEGIN { new = ENVIRON["SUMMARY_TEXT"] }
    /^## Summary[[:space:]]*$/ { print; print ""; print new; print ""; skip = 1; found = 1; next }
    skip && /^## / { skip = 0 }
    !skip { print }
    END { if (!found) exit 3 }' <<< "${BODY}") \
    || refuse "PR #${PR_NUMBER} has no ## Summary section to rewrite"
fi

# Drop every existing marker line, then append the one for this head.
BODY=$(printf '%s\n' "${BODY}" | grep -v '^<!-- conductor-summary-head: [0-9a-f]* -->[[:space:]]*$' || true)
BODY=$(printf '%s\n' "${BODY}" | cat -s)
BODY=$(printf '%s\n' "${BODY}" | awk 'NF { last = NR } { l[NR] = $0 } END { for (i = 1; i <= last; i++) print l[i] }')
NEW_BODY=$(printf '%s\n\n<!-- conductor-summary-head: %s -->' "${BODY}" "${HEAD_SHA}")

emit() {
  jq -cn --arg context "$1" '{markers: [], context: $context}' \
    | { printf '<<<FLOW_OUTPUT>>>\n'; cat; printf '<<<END_FLOW_OUTPUT>>>\n'; }
}

if [ "${CHANGED}" = "true" ]; then
  MSG="Summary rewritten: ${REASON}"
else
  MSG="Summary still matches the diff."
fi

if [ "${DRY_RUN}" = "true" ]; then
  echo "[dry run] would set the body of PR #${PR_NUMBER} to:"
  printf '%s\n' "${NEW_BODY}"
  emit "[dry run] ${MSG}"
  exit 0
fi

BODY_FILE=$(mktemp "${TMPDIR:-/tmp}/pr_summary_body.XXXXXX")
trap 'rm -f "${BODY_FILE}"' EXIT
printf '%s\n' "${NEW_BODY}" > "${BODY_FILE}"
gh_retry gh pr edit "${PR_NUMBER}" --repo "${OWNER_REPO}" --body-file "${BODY_FILE}" >/dev/null \
  || refuse "could not update the body of PR #${PR_NUMBER}; nothing was merged"
emit "${MSG}"
