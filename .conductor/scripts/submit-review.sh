#!/usr/bin/env bash
# submit-review.sh — dismiss stale conductor review, file off-diff issues, submit formal review
# Called as a script step after review-aggregator. Env vars: PRIOR_OUTPUT, PR_NUMBER, DRY_RUN
set -euo pipefail

# ---------------------------------------------------------------------------
# 1. Resolve PR_NUMBER (fall back to gh pr view if not injected)
# ---------------------------------------------------------------------------
if [ -z "${PR_NUMBER:-}" ] || [[ "${PR_NUMBER}" == *"{{"* ]]; then
  PR_NUMBER=$(gh pr view --json number -q .number 2>/dev/null || true)
fi

if [ -z "${PR_NUMBER}" ]; then
  echo "PR_NUMBER is unset and no open PR found — skipping review submission."
  exit 0
fi

if ! [[ "${PR_NUMBER}" =~ ^[0-9]+$ ]]; then
  echo "PR_NUMBER is not a valid number: '${PR_NUMBER}' — aborting."
  exit 1
fi

# ---------------------------------------------------------------------------
# 2. No-op on dry run
# ---------------------------------------------------------------------------
if [ "${DRY_RUN:-false}" = "true" ]; then
  echo "DRY_RUN=true — would submit formal GitHub review for PR #${PR_NUMBER}."
  echo "reviewed_by:"
  echo "${PRIOR_OUTPUT}" | jq '.reviewed_by // []'
  echo "blocking_findings:"
  echo "${PRIOR_OUTPUT}" | jq '.blocking_findings // []'
  echo "off_diff_findings:"
  echo "${PRIOR_OUTPUT}" | jq '.off_diff_findings // []'
  exit 0
fi

# ---------------------------------------------------------------------------
# 3. Dismiss any existing conductor review on this PR
# ---------------------------------------------------------------------------
OWNER_REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner)

REVIEW_IDS=$(gh api "repos/${OWNER_REPO}/pulls/${PR_NUMBER}/reviews" \
  --jq '[.[] | select(.body | contains("<!-- conductor-review -->")) | .id] | .[]' \
  2>/dev/null || true)

if [ -n "${REVIEW_IDS}" ]; then
  while IFS= read -r review_id; do
    echo "Dismissing stale conductor review ${review_id}…"
    gh api --method PUT \
      "repos/${OWNER_REPO}/pulls/${PR_NUMBER}/reviews/${review_id}/dismissals" \
      -f message="Superseded by new conductor review run." \
      2>/dev/null || true
  done <<< "${REVIEW_IDS}"
fi

# ---------------------------------------------------------------------------
# 4. File off-diff issues
# ---------------------------------------------------------------------------
OFF_DIFF_FINDINGS=$(echo "${PRIOR_OUTPUT}" | jq -c '.off_diff_findings // []')
FINDING_COUNT=$(echo "${OFF_DIFF_FINDINGS}" | jq 'length')

FILED_ISSUES=""

if [ "${FINDING_COUNT}" -gt 0 ]; then
  # Ensure label exists
  gh label create conductor-off-diff \
    --color "0075ca" \
    --description "Finding in unchanged/removed code, not blocking the PR" \
    2>/dev/null || true

  # Fetch existing open off-diff issues for dedup
  EXISTING_ISSUES=$(gh issue list \
    --label conductor-off-diff \
    --state open \
    --json title,url \
    2>/dev/null || echo "[]")

  # File each finding not already tracked
  while IFS= read -r finding; do
    FILE=$(echo "${finding}" | jq -r '.file')
    LINE=$(echo "${finding}" | jq -r '.line')
    SEVERITY=$(echo "${finding}" | jq -r '.severity')
    TITLE=$(echo "${finding}" | jq -r '.title')
    MESSAGE=$(echo "${finding}" | jq -r '.message')
    REVIEWER=$(echo "${finding}" | jq -r '.reviewer')

    # Skip suggestion-severity findings — they appear in PR review body but are not filed as tracked issues
    if [ "${SEVERITY}" = "suggestion" ]; then
      echo "Skipping suggestion-severity off-diff finding: ${FILE}:${LINE} (not filed as issue)"
      continue
    fi

    FILE_LINE_REF="${FILE}:${LINE}"

    # Skip if already tracked
    ALREADY_EXISTS=$(echo "${EXISTING_ISSUES}" | jq -r \
      --arg ref "${FILE_LINE_REF}" \
      '[.[] | select(.title | contains($ref))] | length')

    if [ "${ALREADY_EXISTS}" -gt 0 ]; then
      echo "Skipping already-tracked off-diff finding: ${FILE_LINE_REF}"
      continue
    fi

    ISSUE_BODY="**Severity:** ${SEVERITY}
**Location:** ${FILE_LINE_REF}
**Found by:** ${REVIEWER}

${MESSAGE}"

    ISSUE_URL=$(gh issue create \
      --title "${TITLE} (${FILE_LINE_REF})" \
      --label "conductor-off-diff" \
      --body "${ISSUE_BODY}" \
      2>/dev/null)

    ISSUE_NUMBER=$(echo "${ISSUE_URL}" | grep -o '[0-9]*$')
    echo "Filed off-diff issue: ${ISSUE_URL}"
    FILED_ISSUES="${FILED_ISSUES}- [#${ISSUE_NUMBER} — ${TITLE}](${ISSUE_URL}) — \`${FILE_LINE_REF}\` (${SEVERITY})
"
  done < <(echo "${OFF_DIFF_FINDINGS}" | jq -c '.[]')
fi

# ---------------------------------------------------------------------------
# 5. Build complete review body programmatically
# ---------------------------------------------------------------------------
OVERALL_APPROVED=$(echo "${PRIOR_OUTPUT}" | jq -r 'if .overall_approved == false then "false" else "true" end')

# Safety net: if blocking findings exist, override to not approved regardless of model output
HAS_BLOCKING_CHECK=$(echo "${PRIOR_OUTPUT}" | jq -r 'if (.blocking_findings // [] | length) > 0 then "true" else "false" end')
if [ "${HAS_BLOCKING_CHECK}" = "true" ]; then
  OVERALL_APPROVED="false"
fi

if [ "${OVERALL_APPROVED}" = "true" ]; then
  HEADING="## Conductor Review Swarm — All Clear"
else
  HEADING="## Conductor Review Swarm — Changes Requested"
fi

# Build compact reviewed-by line
REVIEWED_BY=$(echo "${PRIOR_OUTPUT}" | jq -r '(.reviewed_by // []) | join(", ")')

REVIEW_BODY="${HEADING}

**Reviewed by:** ${REVIEWED_BY}"

# Append blocking findings section if any
if [ "${HAS_BLOCKING_CHECK}" = "true" ]; then
  BLOCKING_SECTION=$(echo "${PRIOR_OUTPUT}" | jq -r '
    "\n### Blocking findings\n",
    (
      [(.blocking_findings // []) | group_by(.reviewer)[] |
        . as $group |
        "<details>\n<summary><b>\($group[0].reviewer)</b> — \($group | length) \(if ($group | length) == 1 then "issue" else "issues" end)</summary>\n",
        ($group[] | "- **\(.severity)** `\(.file):\(.line)` — \(.message)"),
        "</details>"
      ] | .[]
    )
  ')
  REVIEW_BODY="${REVIEW_BODY}
${BLOCKING_SECTION}"
fi

REVIEW_BODY="${REVIEW_BODY}

<!-- conductor-review -->"

if [ -n "${FILED_ISSUES}" ]; then
  REVIEW_BODY="${REVIEW_BODY}

### Off-diff findings (filed as issues, not blocking this PR)
${FILED_ISSUES}"
fi

REVIEW_BODY_FILE=$(mktemp "${TMPDIR:-/tmp}/conductor_review_body.XXXXXXXXXX.md")
trap 'rm -f "${REVIEW_BODY_FILE}"' EXIT
echo "${REVIEW_BODY}" > "${REVIEW_BODY_FILE}"

# ---------------------------------------------------------------------------
# 6. Submit formal review
# ---------------------------------------------------------------------------
if [ "${OVERALL_APPROVED}" = "true" ]; then
  echo "Submitting APPROVE review for PR #${PR_NUMBER}…"
  gh pr review "${PR_NUMBER}" --approve --body-file "${REVIEW_BODY_FILE}"
else
  echo "Submitting REQUEST CHANGES review for PR #${PR_NUMBER}…"
  gh pr review "${PR_NUMBER}" --request-changes --body-file "${REVIEW_BODY_FILE}"
fi

echo "Review submitted successfully."
