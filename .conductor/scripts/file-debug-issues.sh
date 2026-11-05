#!/usr/bin/env bash
set -euo pipefail

# File GitHub issues for diagnosed workflow failures.
# Reads the structured diagnosis from PRIOR_OUTPUT (debug-diagnosis schema).
# Env: PRIOR_OUTPUT — JSON string with issues array from diagnose-failure agent.
#      RUN_ID — workflow run ID for linking in issue bodies.
#      OTHER_ISSUE_REPOS — space-separated owner/name repos an issue may be
#        routed to with its `repo` field (default: runkon/runkon). Anything
#        else is filed here with a note, so a diagnosis can't file elsewhere.

ISSUES=$(echo "${PRIOR_OUTPUT}" | jq -c '.issues // []')
ISSUE_COUNT=$(echo "${ISSUES}" | jq 'length')
ROOT_CAUSE=$(echo "${PRIOR_OUTPUT}" | jq -r '.root_cause // "unknown"')
SUMMARY=$(echo "${PRIOR_OUTPUT}" | jq -r '.summary // ""')

if [ "${ISSUE_COUNT}" -eq 0 ]; then
  echo "No issues to file."
  cat <<EOF
<<<FLOW_OUTPUT>>>
{"markers": [], "context": "No issues to file — diagnosis found no actionable fixes"}
<<<END_FLOW_OUTPUT>>>
EOF
  exit 0
fi

# ---------------------------------------------------------------------------
# 1. Resolve which repo each issue goes to (#3150)
# ---------------------------------------------------------------------------
# Findings in the runkon engine belong in runkon/runkon. GitHub can't transfer
# issues between owners, so filing them here meant recreating them by hand.
THIS_REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner)
OTHER_ISSUE_REPOS="${OTHER_ISSUE_REPOS:-runkon/runkon}"

# target_repo REQUESTED — prints the repo to file in.
target_repo() {
  local requested="$1" allowed
  if [ -z "${requested}" ] || [ "${requested}" = "null" ] || [ "${requested}" = "${THIS_REPO}" ]; then
    echo "${THIS_REPO}"
    return
  fi
  for allowed in ${OTHER_ISSUE_REPOS}; do
    if [ "${requested}" = "${allowed}" ]; then
      echo "${allowed}"
      return
    fi
  done
  echo "${THIS_REPO}"
}

# ---------------------------------------------------------------------------
# 2. Per repo: ensure the workflow-debug label and list open issues for dedup
# ---------------------------------------------------------------------------
ensure_debug_label() {
  gh label create workflow-debug --repo "$1" \
    --color "d93f0b" \
    --description "Workflow failure diagnosis — auto-filed by debug-failed-run" \
    2>/dev/null || true
}

existing_debug_issues() {
  gh issue list --repo "$1" \
    --label workflow-debug \
    --state open \
    --json title,url \
    2>/dev/null || echo "[]"
}

# ---------------------------------------------------------------------------
# 3. File each issue
# ---------------------------------------------------------------------------
filed=0
skipped=0
filed_urls=""

while IFS= read -r issue; do
  TITLE=$(echo "${issue}" | jq -r '.title')
  DESCRIPTION=$(echo "${issue}" | jq -r '.description')
  CATEGORY=$(echo "${issue}" | jq -r '.category')
  FAILED_STEP=$(echo "${issue}" | jq -r '.failed_step')
  SEVERITY=$(echo "${issue}" | jq -r '.severity')
  REQUESTED_REPO=$(echo "${issue}" | jq -r '.repo // ""')
  REPO=$(target_repo "${REQUESTED_REPO}")
  ROUTING_NOTE=""
  if [ -n "${REQUESTED_REPO}" ] && [ "${REQUESTED_REPO}" != "${REPO}" ]; then
    echo "Warning: diagnosis asked to file '${TITLE}' in ${REQUESTED_REPO}, which isn't allowed; filing in ${REPO}" >&2
    ROUTING_NOTE="

_The diagnosis suggested filing this in ${REQUESTED_REPO}._"
  fi

  ensure_debug_label "${REPO}"
  EXISTING_ISSUES=$(existing_debug_issues "${REPO}")

  # Dedup: skip if an open issue with the same title substring exists
  ALREADY_EXISTS=$(echo "${EXISTING_ISSUES}" | jq -r \
    --arg title "${TITLE}" \
    '[.[] | select(.title | contains($title))] | length')

  if [ "${ALREADY_EXISTS}" -gt 0 ]; then
    echo "Skipping duplicate issue: ${TITLE}"
    skipped=$((skipped + 1))
    continue
  fi

  ISSUE_BODY="**Severity:** ${SEVERITY}
**Category:** ${CATEGORY}
**Failed step:** ${FAILED_STEP}
**Workflow run:** \`${RUN_ID}\` (${THIS_REPO})

---

${DESCRIPTION}

---

**Root cause:** ${ROOT_CAUSE}

**Summary:** ${SUMMARY}${ROUTING_NOTE}"

  # The identity this step runs as may not be able to file in the other repo
  # (for example a GitHub App not installed there). Then file here instead,
  # with a note, rather than lose the finding.
  if ! ISSUE_URL=$(gh issue create \
      --repo "${REPO}" \
      --title "${TITLE}" \
      --label "workflow-debug" \
      --body "${ISSUE_BODY}"); then
    if [ "${REPO}" = "${THIS_REPO}" ]; then
      echo "ERROR: could not file '${TITLE}' in ${REPO}" >&2
      exit 1
    fi
    echo "Warning: could not file '${TITLE}' in ${REPO}; filing in ${THIS_REPO} instead" >&2
    ensure_debug_label "${THIS_REPO}"
    ISSUE_URL=$(gh issue create \
      --repo "${THIS_REPO}" \
      --title "${TITLE}" \
      --label "workflow-debug" \
      --body "${ISSUE_BODY}

_This belongs in ${REPO}, but filing there failed; recreate it there by hand._")
  fi

  echo "Filed issue: ${ISSUE_URL}"
  filed=$((filed + 1))
  filed_urls="${filed_urls}${ISSUE_URL} "
done < <(echo "${ISSUES}" | jq -c '.[]')

cat <<EOF
<<<FLOW_OUTPUT>>>
{"markers": [], "context": "Filed ${filed} issue(s), skipped ${skipped} duplicate(s). ${filed_urls}"}
<<<END_FLOW_OUTPUT>>>
EOF
