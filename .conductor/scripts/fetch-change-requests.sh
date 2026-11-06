#!/usr/bin/env bash
# fetch-change-requests.sh — collect change requests trusted authors posted on
# the PR since the last conductor review (#3308).
#
# Runs at the top of each iterate-pr cycle, before review-pr posts a new review
# (which would empty the window). Reuses fetch-pr-replies.sh for the window, the
# reviewer-identity exclusion and truncation, then keeps only candidates that
# may steer the pipeline:
#   - author_association OWNER, MEMBER or COLLABORATOR (the #3266/#3278 trust rule);
#   - not a Bot: acting on a comment changes code, so bots never qualify;
#   - not triage's own [Pushback on / [Clarify on comments, which are posted as
#     the developer identity and may be the OWNER account;
#   - not already triaged earlier in this run (comment_id of a change-request
#     entry in an earlier triage-reviews output in PRIOR_CONTEXTS).
# Triage decides which candidates actually ask for a change.
#
# Env: PR_NUMBER, PRIOR_CONTEXTS. Failures are non-fatal: emits an empty list.
set -uo pipefail

emit() {
  # $1 = context string, $2 = change requests JSON array
  jq -cn --arg context "$1" --argjson crs "$2" \
    '{markers: (if ($crs | length) > 0 then ["has_change_requests"] else [] end), context: $context, change_requests: $crs}' \
    | { printf '<<<FLOW_OUTPUT>>>\n'; cat; printf '<<<END_FLOW_OUTPUT>>>\n'; }
}

PRIOR_CONTEXTS="${PRIOR_CONTEXTS:-}"
if ! jq -e 'type == "array"' >/dev/null 2>&1 <<< "${PRIOR_CONTEXTS}"; then
  PRIOR_CONTEXTS="[]"
fi

OUT=$("$(dirname "$0")/fetch-pr-replies.sh" 2>/dev/null) || OUT=""
REPLIES=$(printf '%s\n' "${OUT}" | sed -n '/^<<<FLOW_OUTPUT>>>$/,/^<<<END_FLOW_OUTPUT>>>$/p' \
  | sed '1d;$d' | jq -c '.replies // []' 2>/dev/null) || REPLIES=""
if [ -z "${REPLIES}" ]; then
  emit "Could not read PR replies; no change requests." "[]"
  exit 0
fi

if ! CRS=$(jq -cn --argjson replies "${REPLIES}" --argjson prior "${PRIOR_CONTEXTS}" '
  def json_obj: strings | try fromjson catch empty | objects;
  ([ $prior[] | select(.step == "triage-reviews")
     | ((.structured_output // empty) | json_obj) | (.triaged // [])[]?
     | select(.source == "change-request") | .comment_id ]) as $seen
  | [ $replies[]
      | select((.author_association // "") | IN("OWNER", "MEMBER", "COLLABORATOR"))
      | select((.author_type // "") != "Bot")
      | select((.body // "") | (startswith("[Pushback on") or startswith("[Clarify on")) | not)
      | select(.id != null and ((.id as $i | $seen | index($i)) == null)) ]' 2>/dev/null); then
  emit "Could not filter PR replies; no change requests." "[]"
  exit 0
fi

COUNT=$(jq 'length' <<< "${CRS}")
emit "${COUNT} change request candidates from trusted authors since the last conductor review." "${CRS}"
