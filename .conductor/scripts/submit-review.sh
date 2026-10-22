#!/usr/bin/env bash
# submit-review.sh — dismiss stale conductor review, file off-diff issues, submit formal review
# Called as a script step after review-aggregator. Env vars: PRIOR_OUTPUT, PR_NUMBER, DRY_RUN,
# FILE_OFF_DIFF ("false" lists off-diff findings in the review body instead of filing issues)
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

# Only the exact string "false" turns filing off. An unset or unsubstituted
# value keeps the default behaviour (#3090).
FILE_OFF_DIFF_ENABLED="true"
[ "${FILE_OFF_DIFF:-true}" = "false" ] && FILE_OFF_DIFF_ENABLED="false"

# ---------------------------------------------------------------------------
# 1a. Security findings always block.
#
# The aggregator may set aside findings that a PR reply already answered
# (answered_findings). A reply never clears a Security finding, so move any
# that ended up there back into blocking_findings.
# ---------------------------------------------------------------------------
PRIOR_OUTPUT=$(echo "${PRIOR_OUTPUT}" | jq '
  ((.answered_findings // []) | map(select(.reviewer == "Security"))) as $sec
  | if ($sec | length) > 0 then
      .blocking_findings = ((.blocking_findings // []) + ($sec | map(del(.answered_by, .reply))))
      | .answered_findings = ((.answered_findings // []) | map(select(.reviewer != "Security")))
    else . end
')

# ---------------------------------------------------------------------------
# 1b. Filter hallucinated off-diff entries from blocking_findings.
#
# Reviewers occasionally emit findings that cite files outside the PR's diff
# (recognized pattern from training data, not actual code in this PR). The
# safety net: drop any blocking_findings whose `file` is not in the diff.
# off_diff_findings are intentionally off-diff and not filtered here.
# ---------------------------------------------------------------------------
BASE_BRANCH=$(gh pr view "${PR_NUMBER}" --json baseRefName -q .baseRefName 2>/dev/null)
if [ -z "${BASE_BRANCH}" ]; then
  echo "Warning: could not resolve PR base branch for PR #${PR_NUMBER} — skipping off-diff filter."
  echo "         Findings will pass through unfiltered (not silently filtered against the wrong base)."
  DIFF_FILE_COUNT=0
else
  DIFF_FILES_JSON=$(git diff --name-only "origin/${BASE_BRANCH}...HEAD" 2>/dev/null \
    | jq -R -s 'split("\n") | map(select(length > 0))' || echo "[]")
  DIFF_FILE_COUNT=$(echo "${DIFF_FILES_JSON}" | jq 'length')
fi

if [ "${DIFF_FILE_COUNT}" -eq 0 ]; then
  echo "Warning: git diff returned no files for origin/${BASE_BRANCH:-<unresolved>}...HEAD — skipping off-diff filter to avoid silently dropping legitimate findings."
else
  PRIOR_OUTPUT_FILTERED=$(echo "${PRIOR_OUTPUT}" | jq --argjson diff "${DIFF_FILES_JSON}" '
    ((.blocking_findings // []) | length) as $before
    | .blocking_findings = ((.blocking_findings // []) | map(select(.file as $f | $diff | index($f))))
    | .blocking_findings_dropped = ($before - (.blocking_findings | length))
  ')

  DROPPED_COUNT=$(echo "${PRIOR_OUTPUT_FILTERED}" | jq -r '.blocking_findings_dropped // 0')
  if [ "${DROPPED_COUNT}" -gt 0 ]; then
    echo "Dropped ${DROPPED_COUNT} hallucinated off-diff blocking finding(s) from review:"
    echo "${PRIOR_OUTPUT}" | jq -c --argjson diff "${DIFF_FILES_JSON}" '
      .blocking_findings // []
      | map(select(.file as $f | $diff | index($f) | not))
      | .[] | {file, line, severity, reviewer, message: (.message // "")}
    '
  fi

  # Use the filtered output for everything downstream.
  PRIOR_OUTPUT="${PRIOR_OUTPUT_FILTERED}"
fi

# ---------------------------------------------------------------------------
# 2. No-op on dry run
# ---------------------------------------------------------------------------
# Dry runs must never file issues: this exit has to stay before step 4 (#3090).
if [ "${DRY_RUN:-false}" = "true" ]; then
  echo "DRY_RUN=true — would submit formal GitHub review for PR #${PR_NUMBER}."
  echo "TICKET_SOURCE_ID: ${TICKET_SOURCE_ID:-<not set>}"
  echo "CONDUCTOR_REPO:   ${CONDUCTOR_REPO:-<not set>}"
  echo "file_off_diff:    ${FILE_OFF_DIFF_ENABLED}"
  echo "reviewed_by:"
  echo "${PRIOR_OUTPUT}" | jq -r '.reviewed_by // ""'
  echo "blocking_findings:"
  echo "${PRIOR_OUTPUT}" | jq '.blocking_findings // []'
  echo "answered_findings:"
  echo "${PRIOR_OUTPUT}" | jq '.answered_findings // []'
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
UNFILED_OFF_DIFF=""

# Create a label if it doesn't exist. Failures other than "already exists" are
# logged, not hidden (#3081) — labelling is best-effort and never fails the step.
ensure_label() {
  local name="$1" color="$2" description="${3:-}" err
  if err=$(gh label create "${name}" --color "${color}" --description "${description}" 2>&1); then
    return 0
  fi
  case "${err}" in
    *"already exists"*) ;;
    *) echo "Warning: could not create label ${name}: ${err}" ;;
  esac
  return 0
}

if [ "${FINDING_COUNT}" -gt 0 ] && [ "${FILE_OFF_DIFF_ENABLED}" = "true" ]; then
  ensure_label conductor-off-diff "0075ca" "Finding in unchanged/removed code, not blocking the PR"

  # Fetch ALL open issues for dedup, matching on the "(file:line)" title suffix.
  # Deliberately not filtered by label: the reviewer identity can create issues
  # that GitHub then drops labels from, so a label filter never finds them (#3081).
  if ! EXISTING_ISSUES=$(gh issue list --state open --limit 1000 --json title,url); then
    echo "Warning: could not list open issues for dedup — already-tracked findings may be filed again."
    EXISTING_ISSUES="[]"
  fi

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

    # Skip if already tracked. Match the parenthesised ref so foo.rs:3 doesn't match foo.rs:38.
    ALREADY_EXISTS=$(echo "${EXISTING_ISSUES}" | jq -r \
      --arg ref "(${FILE_LINE_REF})" \
      '[.[] | select(.title | contains($ref))] | length')

    if [ "${ALREADY_EXISTS}" -gt 0 ]; then
      echo "Skipping already-tracked off-diff finding: ${FILE_LINE_REF}"
      continue
    fi

    # Requested labels: conductor-off-diff plus the finding's own, each ensured to exist
    REQUESTED_LABELS=("conductor-off-diff")
    LABEL_ARGS=(--label "conductor-off-diff")
    while IFS= read -r label; do
      [ -z "${label}" ] && continue
      ensure_label "${label}" "ededed"
      REQUESTED_LABELS+=("${label}")
      LABEL_ARGS+=(--label "${label}")
    done < <(echo "${finding}" | jq -r '(.labels // []) | .[]')

    ISSUE_BODY="**Severity:** ${SEVERITY}
**Location:** ${FILE_LINE_REF}
**Found by:** ${REVIEWER}

${MESSAGE}"

    if ! ISSUE_URL=$(gh issue create \
      --title "${TITLE} (${FILE_LINE_REF})" \
      "${LABEL_ARGS[@]}" \
      --body "${ISSUE_BODY}"); then
      echo "Warning: could not file off-diff issue for ${FILE_LINE_REF} — skipping."
      continue
    fi
    ISSUE_URL=$(echo "${ISSUE_URL}" | tail -n 1)

    ISSUE_NUMBER=$(echo "${ISSUE_URL}" | grep -o '[0-9]*$' || true)
    echo "Filed off-diff issue: ${ISSUE_URL}"

    # Same-run dedup: later findings at this location must not be filed again
    EXISTING_ISSUES=$(echo "${EXISTING_ISSUES}" | jq -c \
      --arg title "${TITLE} (${FILE_LINE_REF})" --arg url "${ISSUE_URL}" \
      '. + [{title: $title, url: $url}]')

    # GitHub silently drops labels on create when the caller lacks push access.
    # Read them back, retry the missing ones, and warn if that fails too (#3081).
    if [ -n "${ISSUE_NUMBER}" ]; then
      APPLIED_LABELS=$(gh issue view "${ISSUE_NUMBER}" --json labels -q '[.labels[].name] | .[]' 2>/dev/null || true)
      MISSING_LABELS=()
      for label in "${REQUESTED_LABELS[@]}"; do
        if ! printf '%s\n' "${APPLIED_LABELS}" | grep -qxF -- "${label}"; then
          MISSING_LABELS+=("${label}")
        fi
      done
      if [ "${#MISSING_LABELS[@]}" -gt 0 ]; then
        MISSING_CSV=$(IFS=,; echo "${MISSING_LABELS[*]}")
        if LABEL_ERR=$(gh issue edit "${ISSUE_NUMBER}" --add-label "${MISSING_CSV}" 2>&1); then
          echo "Applied labels ${MISSING_CSV} to #${ISSUE_NUMBER} after create dropped them"
        else
          echo "Warning: issue #${ISSUE_NUMBER} was created without labels [${MISSING_CSV}]: ${LABEL_ERR}"
          echo "         The reviewer identity likely lacks permission to label issues."
        fi
      fi
    fi

    FILED_ISSUES="${FILED_ISSUES}- [#${ISSUE_NUMBER} — ${TITLE}](${ISSUE_URL}) — \`${FILE_LINE_REF}\` (${SEVERITY})
"

    # Upsert as a conductor ticket and link to source ticket (best-effort)
    if [ -n "${TICKET_SOURCE_ID:-}" ] && [ -n "${CONDUCTOR_REPO:-}" ] \
        && [[ "${TICKET_SOURCE_ID}" != *"{{"* ]] \
        && [[ "${CONDUCTOR_REPO}" != *"{{"* ]]; then
      UPSERT_LABELS=$(IFS=,; echo "${REQUESTED_LABELS[*]}")

      conductor tickets upsert "${CONDUCTOR_REPO}" \
        --source-type github \
        --source-id "${ISSUE_NUMBER}" \
        --title "${TITLE} (${FILE_LINE_REF})" \
        --state open \
        --body "${ISSUE_BODY}" \
        --url "${ISSUE_URL}" \
        --labels "${UPSERT_LABELS}" \
        --parent "${TICKET_SOURCE_ID}" \
        2>/dev/null || echo "Warning: conductor ticket upsert failed for issue #${ISSUE_NUMBER} (non-fatal)"
    fi
  done < <(echo "${OFF_DIFF_FINDINGS}" | jq -c '.[]')
elif [ "${FINDING_COUNT}" -gt 0 ]; then
  echo "file_off_diff=false — listing ${FINDING_COUNT} off-diff finding(s) in the review body without filing issues."
  UNFILED_OFF_DIFF=$(echo "${OFF_DIFF_FINDINGS}" | jq -r '
    .[] | "- **\(.severity)** `\(.file):\(.line)` (\(.reviewer)) — \(.title): \(.message)"
  ')
fi

# ---------------------------------------------------------------------------
# 5. Build complete review body programmatically
# ---------------------------------------------------------------------------
# Derive approval state purely from the post-filter blocking_findings count.
#
# The aggregator's `.overall_approved` is computed on pre-filter data, so it
# can be stale: if the off-diff filter (step 1b) drops every blocking finding,
# the aggregator may still report `overall_approved: false`. Re-evaluating
# here off the filtered count keeps the review body consistent — no
# "Changes Requested" without an accompanying findings list.
HAS_BLOCKING_CHECK=$(echo "${PRIOR_OUTPUT}" | jq -r 'if (.blocking_findings // [] | length) > 0 then "true" else "false" end')
if [ "${HAS_BLOCKING_CHECK}" = "true" ]; then
  OVERALL_APPROVED="false"
else
  OVERALL_APPROVED="true"
fi

if [ "${OVERALL_APPROVED}" = "true" ]; then
  HEADING="## Conductor Review Swarm — All Clear"
else
  HEADING="## Conductor Review Swarm — Changes Requested"
fi

# Build compact reviewed-by line
REVIEWED_BY=$(echo "${PRIOR_OUTPUT}" | jq -r '.reviewed_by // ""')

REVIEW_BODY="${HEADING}

**Reviewed by:** ${REVIEWED_BY}"

# Append blocking findings section if any
if [ "${HAS_BLOCKING_CHECK}" = "true" ]; then
  BLOCKING_SECTION=$(echo "${PRIOR_OUTPUT}" | jq -r '
    "\n### Blocking findings (\(.blocking_findings // [] | length))\n",
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

# Append findings set aside because a PR reply already answered them
if [ "$(echo "${PRIOR_OUTPUT}" | jq '.answered_findings // [] | length')" -gt 0 ]; then
  ANSWERED_SECTION=$(echo "${PRIOR_OUTPUT}" | jq -r '
    "\n### Previously answered (\(.answered_findings | length), not blocking)\n",
    ((.answered_findings)[] | "- **\(.severity)** `\(.file):\(.line)` (\(.reviewer)) — \(.message)\n  - Answered by \(.answered_by): \(.reply)")
  ')
  REVIEW_BODY="${REVIEW_BODY}
${ANSWERED_SECTION}"
fi

REVIEW_BODY="${REVIEW_BODY}

<!-- conductor-review -->"

if [ -n "${FILED_ISSUES}" ]; then
  REVIEW_BODY="${REVIEW_BODY}

### Off-diff findings (filed as issues, not blocking this PR)
${FILED_ISSUES}"
fi

if [ -n "${UNFILED_OFF_DIFF}" ]; then
  REVIEW_BODY="${REVIEW_BODY}

### Off-diff findings (not filed as issues, not blocking this PR)
${UNFILED_OFF_DIFF}"
fi

REVIEW_BODY_FILE=$(mktemp "${TMPDIR:-/tmp}/conductor_review_body.XXXXXXXXXX")
trap 'rm -f "${REVIEW_BODY_FILE}"' EXIT
echo "${REVIEW_BODY}" > "${REVIEW_BODY_FILE}"

# ---------------------------------------------------------------------------
# 6. Submit formal review
# ---------------------------------------------------------------------------

# GitHub disallows REQUEST_CHANGES on your own PR — detect and fall back to COMMENT.
PR_AUTHOR=$(gh pr view "${PR_NUMBER}" --json author -q .author.login 2>/dev/null || true)
CURRENT_USER=$(gh api user -q .login 2>/dev/null || true)
IS_OWN_PR="false"
if [ -n "${PR_AUTHOR}" ] && [ -n "${CURRENT_USER}" ] && [ "${PR_AUTHOR}" = "${CURRENT_USER}" ]; then
  IS_OWN_PR="true"
fi

if [ "${OVERALL_APPROVED}" = "true" ]; then
  echo "Submitting APPROVE review for PR #${PR_NUMBER}…"
  gh pr review "${PR_NUMBER}" --approve --body-file "${REVIEW_BODY_FILE}"
elif [ "${IS_OWN_PR}" = "true" ]; then
  echo "PR author matches current user — submitting COMMENT review (GitHub disallows REQUEST_CHANGES on own PRs)…"
  gh pr review "${PR_NUMBER}" --comment --body-file "${REVIEW_BODY_FILE}"
else
  echo "Submitting REQUEST CHANGES review for PR #${PR_NUMBER}…"
  gh pr review "${PR_NUMBER}" --request-changes --body-file "${REVIEW_BODY_FILE}"
fi

echo "Review submitted successfully."
