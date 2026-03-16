#!/usr/bin/env bash
# submit-review.sh — dismiss stale conductor review, file off-diff issues, submit formal review
# Called as a script step after review-aggregator. Env vars: PRIOR_OUTPUT, PR_NUMBER, DRY_RUN
set -euo pipefail

# ---------------------------------------------------------------------------
# 1. Guard: exit gracefully if no PR_NUMBER
# ---------------------------------------------------------------------------
if [ -z "${PR_NUMBER:-}" ]; then
  echo "PR_NUMBER is unset — skipping review submission (not running on a PR worktree)."
  exit 0
fi

# ---------------------------------------------------------------------------
# 2. No-op on dry run
# ---------------------------------------------------------------------------
if [ "${DRY_RUN:-false}" = "true" ]; then
  echo "DRY_RUN=true — would submit formal GitHub review for PR #${PR_NUMBER}."
  echo "review_body preview:"
  echo "${PRIOR_OUTPUT}" | jq -r '.review_body // "(no review_body in output)"'
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
# 5. Build complete review body
# ---------------------------------------------------------------------------
REVIEW_BODY=$(echo "${PRIOR_OUTPUT}" | jq -r '.review_body // ""')

if [ -n "${FILED_ISSUES}" ]; then
  REVIEW_BODY="${REVIEW_BODY}

### Off-diff findings (filed as issues, not blocking this PR)
${FILED_ISSUES}"
fi

echo "${REVIEW_BODY}" > /tmp/conductor_review_body.md

# ---------------------------------------------------------------------------
# 6. Submit formal review
# ---------------------------------------------------------------------------
OVERALL_APPROVED=$(echo "${PRIOR_OUTPUT}" | jq -r '.overall_approved // true')

if [ "${OVERALL_APPROVED}" = "true" ]; then
  echo "Submitting APPROVE review for PR #${PR_NUMBER}…"
  gh pr review "${PR_NUMBER}" --approve --body-file /tmp/conductor_review_body.md
else
  echo "Submitting REQUEST CHANGES review for PR #${PR_NUMBER}…"
  gh pr review "${PR_NUMBER}" --request-changes --body-file /tmp/conductor_review_body.md
fi

echo "Review submitted successfully."
