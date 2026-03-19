#!/usr/bin/env bash
set -euo pipefail

# File GitHub issues for diagnosed workflow failures.
# Reads the structured diagnosis from PRIOR_OUTPUT (debug-diagnosis schema).
# Env: PRIOR_OUTPUT — JSON string with issues array from diagnose-failure agent.
#      RUN_ID — workflow run ID for linking in issue bodies.

ISSUES=$(echo "${PRIOR_OUTPUT}" | jq -c '.issues // []')
ISSUE_COUNT=$(echo "${ISSUES}" | jq 'length')
ROOT_CAUSE=$(echo "${PRIOR_OUTPUT}" | jq -r '.root_cause // "unknown"')
SUMMARY=$(echo "${PRIOR_OUTPUT}" | jq -r '.summary // ""')

if [ "${ISSUE_COUNT}" -eq 0 ]; then
  echo "No issues to file."
  cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": "No issues to file — diagnosis found no actionable fixes"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
  exit 0
fi

# ---------------------------------------------------------------------------
# 1. Ensure workflow-debug label exists
# ---------------------------------------------------------------------------
gh label create workflow-debug \
  --color "d93f0b" \
  --description "Workflow failure diagnosis — auto-filed by debug-failed-run" \
  2>/dev/null || true

# ---------------------------------------------------------------------------
# 2. Fetch existing open workflow-debug issues for dedup
# ---------------------------------------------------------------------------
EXISTING_ISSUES=$(gh issue list \
  --label workflow-debug \
  --state open \
  --json title,url \
  2>/dev/null || echo "[]")

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
**Workflow run:** \`${RUN_ID}\`

---

${DESCRIPTION}

---

**Root cause:** ${ROOT_CAUSE}

**Summary:** ${SUMMARY}"

  ISSUE_URL=$(gh issue create \
    --title "${TITLE}" \
    --label "workflow-debug" \
    --body "${ISSUE_BODY}" \
    2>/dev/null)

  echo "Filed issue: ${ISSUE_URL}"
  filed=$((filed + 1))
  filed_urls="${filed_urls}${ISSUE_URL} "
done < <(echo "${ISSUES}" | jq -c '.[]')

cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": "Filed ${filed} issue(s), skipped ${skipped} duplicate(s). ${filed_urls}"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
