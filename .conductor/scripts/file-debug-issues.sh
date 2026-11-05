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
THIS_VISIBILITY=$(gh repo view --json visibility -q .visibility)
OTHER_ISSUE_REPOS="${OTHER_ISSUE_REPOS:-runkon/runkon}"

# target_repo REQUESTED — prints the repo to file in; reasons go to stderr.
# Only an allowlisted owner/name is accepted, and a finding from a non-public
# repo never goes to a public one: diagnoses quote run logs.
target_repo() {
  local requested="$1" allowed visibility
  if [ -z "${requested}" ] || [ "${requested}" = "${THIS_REPO}" ]; then
    echo "${THIS_REPO}"
    return
  fi
  if ! [[ "${requested}" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
    echo "Warning: ignoring an invalid repo in the diagnosis; filing in ${THIS_REPO}" >&2
    echo "${THIS_REPO}"
    return
  fi
  for allowed in ${OTHER_ISSUE_REPOS}; do
    [ "${requested}" = "${allowed}" ] || continue
    if [ "${THIS_VISIBILITY}" != "PUBLIC" ]; then
      if ! visibility=$(gh repo view "${allowed}" --json visibility -q .visibility); then
        echo "Warning: could not check ${allowed}'s visibility; filing in ${THIS_REPO}" >&2
        echo "${THIS_REPO}"
        return
      fi
      if [ "${visibility}" = "PUBLIC" ]; then
        echo "Warning: ${THIS_REPO} is not public, so its findings aren't filed in public ${allowed}; filing in ${THIS_REPO}" >&2
        echo "${THIS_REPO}"
        return
      fi
    fi
    echo "${allowed}"
    return
  done
  echo "Warning: the diagnosis asked for ${requested}, which isn't in OTHER_ISSUE_REPOS; filing in ${THIS_REPO}" >&2
  echo "${THIS_REPO}"
}

# ---------------------------------------------------------------------------
# 2. Per-repo helpers: label, dedup, create
# ---------------------------------------------------------------------------
# Best effort: only "already exists" is expected; anything else is logged, and
# the create below then reports its own error.
ensure_debug_label() {
  local err
  if err=$(gh label create workflow-debug --repo "$1" \
      --color "d93f0b" \
      --description "Workflow failure diagnosis — auto-filed by debug-failed-run" 2>&1); then
    return 0
  fi
  case "${err}" in
    *"already exists"*) ;;
    *) echo "Warning: could not create the workflow-debug label in $1: ${err}" >&2 ;;
  esac
  return 0
}

# is_duplicate REPO TITLE — 0: an open workflow-debug issue with that title
# exists; 1: none; 2: the list failed (error logged).
is_duplicate() {
  local list count
  if ! list=$(gh issue list --repo "$1" --label workflow-debug --state open --limit 500 --json title); then
    echo "Warning: could not list open workflow-debug issues in $1 to check for duplicates" >&2
    return 2
  fi
  count=$(printf '%s' "${list}" | jq --arg title "$2" '[.[] | select(.title | contains($title))] | length')
  [ "${count}" -gt 0 ]
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

  ISSUE_BODY="**Severity:** ${SEVERITY}
**Category:** ${CATEGORY}
**Failed step:** ${FAILED_STEP}
**Workflow run:** \`${RUN_ID}\`

---

${DESCRIPTION}

---

**Root cause:** ${ROOT_CAUSE}

**Summary:** ${SUMMARY}"

  ISSUE_URL=""
  fallback_note=""
  if [ "${REPO}" != "${THIS_REPO}" ]; then
    ensure_debug_label "${REPO}"
    dup=0
    is_duplicate "${REPO}" "${TITLE}" || dup=$?
    if [ "${dup}" -eq 0 ]; then
      echo "Skipping duplicate issue in ${REPO}: ${TITLE}"
      skipped=$((skipped + 1))
      continue
    fi
    # The identity this step runs as may not reach the other repo (for
    # example a GitHub App not installed there). Then file here instead, with
    # a note, rather than lose the finding.
    if [ "${dup}" -eq 2 ] || ! ISSUE_URL=$(gh issue create --repo "${REPO}" \
        --title "${TITLE}" --label "workflow-debug" --body "${ISSUE_BODY}"); then
      echo "Warning: could not file '${TITLE}' in ${REPO}; filing in ${THIS_REPO} instead" >&2
      ISSUE_URL=""
      fallback_note="

_This belongs in ${REPO}, but filing there failed; recreate it there by hand._"
    fi
  fi

  if [ -z "${ISSUE_URL}" ]; then
    ensure_debug_label "${THIS_REPO}"
    dup=0
    is_duplicate "${THIS_REPO}" "${TITLE}" || dup=$?
    if [ "${dup}" -eq 0 ]; then
      echo "Skipping duplicate issue in ${THIS_REPO}: ${TITLE}"
      skipped=$((skipped + 1))
      continue
    fi
    # dup=2: couldn't check. Filing a possible duplicate beats losing the finding.
    if ! ISSUE_URL=$(gh issue create --repo "${THIS_REPO}" \
        --title "${TITLE}" --label "workflow-debug" --body "${ISSUE_BODY}${fallback_note}"); then
      echo "ERROR: could not file '${TITLE}' in ${THIS_REPO}" >&2
      exit 1
    fi
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
