#!/usr/bin/env bash
# verify-progress.sh — did the implement retry make progress? (#3486)
#
# ticket-to-pr runs this after its second verify still fails. It reads the last
# two `verify` entries of PRIOR_CONTEXTS (JSON) and emits `failures_shrank` only
# when both carry a failure signature (`failures` array and numeric
# `failure_count`, from verify.sh), the signatures differ, and the count went
# down. The workflow then allows one more fix attempt (fix-verify) and a third
# verify. Anything else stops at fail-verify as before: fewer than two verifies,
# a missing signature (a run resumed from before this change), malformed input,
# an unchanged signature, or a count that stayed the same or grew. A fault here
# never fails the run in a new way: the script always exits 0.
#
# The context is a one-line decision followed by the last verify's context, so
# fix-verify and fail-verify, which read {{prior_context}}, still see the failures.
#
# Env: PRIOR_CONTEXTS (JSON array of {step, context, structured_output, ...}).
set -uo pipefail

emit() {
  # $1 = markers JSON, $2 = context
  echo "<<<FLOW_OUTPUT>>>"
  jq -cn --argjson markers "$1" --arg ctx "$2" '{markers: $markers, context: $ctx}'
  echo "<<<END_FLOW_OUTPUT>>>"
}

result=$(printf '%s' "${PRIOR_CONTEXTS:-}" | jq -c '
  def sig: (.structured_output | if type == "string" then (try fromjson catch null) else . end)
    | if type == "object" and (.failures | type) == "array" and (.failure_count | type) == "number"
      then {failures, count: .failure_count} else null end;
  [.[]? | select(type == "object" and .step == "verify")] as $v
  | ($v | last) as $last
  | ($v | if length >= 2 then .[length - 2] else null end) as $prev
  | ($prev | if . == null then null else sig end) as $ps
  | ($last | if . == null then null else sig end) as $ls
  | {ctx: ($last.context // ""),
     prev: ($ps.count // null), last: ($ls.count // null),
     shrank: ($ps != null and $ls != null and $ps.failures != $ls.failures and $ls.count < $ps.count)}
' 2>/dev/null) || result=""

if [ -z "${result}" ]; then
  emit '[]' "Could not compare the last two verify results; stopping."
  exit 0
fi

shrank=$(jq -r '.shrank' <<< "${result}")
prev=$(jq -r '.prev // "?"' <<< "${result}")
last=$(jq -r '.last // "?"' <<< "${result}")
ctx=$(jq -r '.ctx' <<< "${result}")

if [ "${shrank}" = "true" ]; then
  emit '["failures_shrank"]' "Failures shrank: ${prev} → ${last}; one more fix attempt.
${ctx}"
else
  emit '[]' "Failures unchanged or grew, or no comparison was possible (${prev} → ${last}); stopping.
${ctx}"
fi
exit 0
