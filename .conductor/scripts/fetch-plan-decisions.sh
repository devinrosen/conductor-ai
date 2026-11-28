#!/usr/bin/env bash
# fetch-plan-decisions.sh — the decisions the user made at the plan gate, for the reviewers (#3484)
# Runs in review-pr before the reviewer swarm. Env vars: PLAN_DECISIONS (review-pr's
# plan_decisions input), PR_NUMBER, TRUSTED_REVIEW_BOTS (see lib-trusted-review.sh).
#
# Emits a FLOW_OUTPUT block with:
#   user_decisions    text for {{user_decisions}} in the shared review-diff-scope prompt
#   decision_count    how many numbered decisions it lists; aggregate-reviews.sh only sets a
#                     finding aside when its `decision` number is within 1..decision_count
#   decisions_source  input | pr_body | none
#
# Source: the PLAN_DECISIONS input when it is set (a ticket-to-pr run passes push-and-pr's
# export). Otherwise the PR body's `## Decisions` section, so verify-and-merge's re-review
# and a hand-run iterate-pr see them too, but only when the PR's author is trusted: the
# text goes into every reviewer's prompt and can stop findings from blocking.
#
# Always emits every field, since exported variables persist across loop iterations
# (iterate-pr calls review-pr in a loop). Never fails the review: on any error the
# decisions are None.
set -euo pipefail

# shellcheck source=lib-trusted-review.sh
source "$(dirname "$0")/lib-trusted-review.sh"

MAX_CHARS=4000
NONE="None: the user made no plan-gate decisions for this PR."

emit() {
  # $1 = context, $2 = text, $3 = count, $4 = source
  jq -cn --arg context "$1" --arg text "$2" --argjson count "$3" --arg src "$4" \
    '{markers: [], context: $context, user_decisions: $text, decision_count: $count, decisions_source: $src}' \
    | { printf '<<<FLOW_OUTPUT>>>\n'; cat; printf '<<<END_FLOW_OUTPUT>>>\n'; }
}

# emit_text TEXT SOURCE — truncate, count the numbered questions and emit.
emit_text() {
  local text="$1" src="$2" count
  count=$(printf '%s\n' "${text}" | grep -cE '^[0-9]+\. ' || true)
  if [ "${#text}" -gt "${MAX_CHARS}" ]; then
    text="${text:0:${MAX_CHARS}} [truncated]"
  fi
  emit "Plan decisions from ${src}: ${count} listed." "${text}" "${count:-0}" "${src}"
}

input="${PLAN_DECISIONS:-}"
trimmed="${input#"${input%%[![:space:]]*}"}"
trimmed="${trimmed%"${trimmed##*[![:space:]]}"}"
tmpl_re='^\{\{[[:alnum:]_.]+\}\}$'
if [ -n "${trimmed}" ] && ! [[ "${trimmed}" =~ ${tmpl_re} ]]; then
  emit_text "${trimmed}" "input"
  exit 0
fi

# pr_number is injected only when the branch has a PR; not a declared input.
if [ -z "${PR_NUMBER:-}" ] || [[ "${PR_NUMBER}" == *"{{"* ]]; then
  PR_NUMBER=$(gh pr view --json number -q .number 2>/dev/null || true)
fi
if ! [[ "${PR_NUMBER:-}" =~ ^[0-9]+$ ]]; then
  emit "No PR number; no plan decisions." "${NONE}" 0 "none"
  exit 0
fi

OWNER_REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || true)
if [ -z "${OWNER_REPO}" ]; then
  echo "fetch-plan-decisions: could not resolve the repository" >&2
  emit "Could not resolve the repository." "${NONE}" 0 "none"
  exit 0
fi
if ! PR=$(gh api "repos/${OWNER_REPO}/pulls/${PR_NUMBER}" 2>/dev/null) || ! jq -e 'type == "object"' >/dev/null 2>&1 <<< "${PR}"; then
  echo "fetch-plan-decisions: could not fetch PR #${PR_NUMBER}" >&2
  emit "Could not fetch PR #${PR_NUMBER}." "${NONE}" 0 "none"
  exit 0
fi

# The section body, up to the next `## ` heading, only from a trusted author.
SECTION=$(jq -r --arg bots "${TRUSTED_REVIEW_BOTS:-}" "$(trusted_author_jq)"'
  if trusted_author($bots) then
    ((.body // "") | split("\n")
      | reduce .[] as $l ({on: false, out: []};
          if ($l | test("^## ")) then .on = ($l | test("^## Decisions\\s*$"))
          elif .on then .out += [$l] else . end)
      | .out | join("\n") | gsub("^\\s+|\\s+$"; ""))
  else "" end' <<< "${PR}" 2>/dev/null || true)
if [ -z "${SECTION}" ]; then
  emit "No plan decisions on PR #${PR_NUMBER}." "${NONE}" 0 "none"
  exit 0
fi
emit_text "${SECTION}" "pr_body"
