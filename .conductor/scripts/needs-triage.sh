#!/usr/bin/env bash
# needs-triage.sh — decide whether iterate-pr runs triage this cycle (#3308).
#
# The workflow DSL has no `or`, so this combines the two triggers: the latest
# review-aggregator has blocking findings, or the latest fetch-change-requests
# found change request candidates. Emits the needs_triage marker when either holds.
#
# Env: PRIOR_CONTEXTS — the engine's prior_contexts JSON array.
set -uo pipefail

PRIOR_CONTEXTS="${PRIOR_CONTEXTS:-}"
if ! jq -e 'type == "array"' >/dev/null 2>&1 <<< "${PRIOR_CONTEXTS}"; then
  PRIOR_CONTEXTS="[]"
fi

jq -c '
  def json_obj: strings | try fromjson catch empty | objects;
  ([ .[] | select(.step == "review-aggregator") ] | last // {}) as $agg
  | ([ .[] | select(.step == "fetch-change-requests") ] | last // {}) as $fcr
  | (($agg.markers // []) | index("has_blocking_findings") != null) as $blocking
  | ([ ($fcr.structured_output // empty) | json_obj | (.change_requests // [])[] ] | length) as $crs
  | ($blocking or $crs > 0) as $need
  | {markers: (if $need then ["needs_triage"] else [] end),
     context: (if $need
       then "Triage needed: " + (if $blocking then "blocking findings" else "no blocking findings" end)
            + ", " + ($crs | tostring) + " change request candidates."
       else "No blocking findings or change requests; skipping triage." end)}
' <<< "${PRIOR_CONTEXTS}" | { printf '<<<FLOW_OUTPUT>>>\n'; cat; printf '<<<END_FLOW_OUTPUT>>>\n'; }
