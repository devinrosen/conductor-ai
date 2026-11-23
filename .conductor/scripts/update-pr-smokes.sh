#!/usr/bin/env bash
# update-pr-smokes.sh — put the cycle's workflow smokes in the PR description (#3453).
#
# iterate-pr runs this after push. verify.sh ran the dry-run smokes for a .wf/.wft change
# in this cycle and recorded them in its structured output; pr-body.sh (PR_BODY_SECTION=smokes)
# renders them as the `## Smokes` section. This replaces that section in the PR body, or
# inserts it before `## Risks and open questions`, else before a trailing `Closes #N` line,
# else at the end. A verify entry with no smokes leaves the body alone.
#
# Env: PR_NUMBER, PRIOR_CONTEXTS. Never fails the run: a gh error warns on stderr and
# exits 0 (the smokes are in verify's step output either way).
set -uo pipefail
# shellcheck source=lib-gh-retry.sh
source "$(dirname "$0")/lib-gh-retry.sh"

emit() {
  jq -cn --arg context "$1" '{markers: [], context: $context}' \
    | { printf '<<<FLOW_OUTPUT>>>\n'; cat; printf '<<<END_FLOW_OUTPUT>>>\n'; }
}

section=$(PR_BODY_SECTION=smokes "$(dirname "$0")/pr-body.sh" 2>/dev/null) || section=""
if [ -z "${section}" ]; then
  emit "No workflow smokes in this cycle; PR body unchanged."
  exit 0
fi

pr="${PR_NUMBER:-}"
if [ -z "${pr}" ] || [ "${pr#\{\{}" != "${pr}" ]; then
  echo "WARNING: no PR_NUMBER; not updating the PR body" >&2
  emit "No PR number; PR body not updated."
  exit 0
fi

if ! body=$(gh_retry gh pr view "${pr}" --json body --jq .body); then
  echo "WARNING: could not read the body of PR #${pr}; not updating it" >&2
  emit "Could not read PR #${pr}; PR body not updated."
  exit 0
fi

# Drop an existing ## Smokes section (heading to the next ## heading or the end).
rest=$(printf '%s\n' "${body}" | awk '
  /^## Smokes[[:space:]]*$/ { skip = 1; next }
  skip && /^## / { skip = 0 }
  !skip { print }')
# awk leaves the blank line that preceded the removed section; squeeze runs of them.
rest=$(printf '%s\n' "${rest}" | cat -s)

new_body=$(SECTION="${section}" awk '
  BEGIN { sec = ENVIRON["SECTION"]; n = 0 }
  { line[n++] = $0 }
  END {
    at = -1
    for (i = 0; i < n; i++) if (line[i] ~ /^## Risks and open questions/) { at = i; break }
    if (at < 0) {
      last = n - 1
      while (last >= 0 && line[last] ~ /^[[:space:]]*$/) last--
      if (last >= 0 && line[last] ~ /^(Closes|Fixes|Resolves) #[0-9]+/) at = last
    }
    if (at < 0) {
      for (i = 0; i < n; i++) print line[i]
      print ""; print sec
    } else {
      for (i = 0; i < at; i++) print line[i]
      print sec; print ""
      for (i = at; i < n; i++) print line[i]
    }
  }' <<< "${rest}")

body_file=$(mktemp "${TMPDIR:-/tmp}/pr-smokes-body.XXXXXX")
trap 'rm -f "${body_file}"' EXIT
printf '%s\n' "${new_body}" > "${body_file}"
if gh_retry gh pr edit "${pr}" --body-file "${body_file}" >/dev/null; then
  emit "Updated the ## Smokes section of PR #${pr}."
else
  echo "WARNING: could not update the body of PR #${pr}" >&2
  emit "Could not update PR #${pr}; PR body not updated."
fi
exit 0
