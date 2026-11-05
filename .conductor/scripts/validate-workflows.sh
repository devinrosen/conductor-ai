#!/usr/bin/env bash
# validate-workflows.sh — deterministic workflow and template validation for
# review-pr (#3290). Runs when detect-workflow-changes reports pipeline changes.
#
# Runs `conductor workflow validate --all --templates` with the conductor built
# from this checkout (cargo run), so a PR that changes the validator is checked
# by its own code. Emits a reviewer-shaped FLOW_OUTPUT (review-findings schema:
# approved, summary, findings) so aggregate-reviews.sh counts a failure as a
# blocking finding from "Workflow Validation", the same way an LLM reviewer's
# finding blocks. The Workflow Semantics reviewer can't miss what this catches.
#
# Each finding names a file in the PR diff (the failing workflow or template if
# the PR touches it, else the first changed pipeline file), so submit-review's
# off-diff filter keeps it.
#
# Env: BASE_BRANCH (from resolve-pr-base).
set -uo pipefail

emit() {
  # $1 = approved (true/false), $2 = summary, $3 = findings JSON
  local markers='[]'
  [ "$1" = "false" ] && markers='["has_review_issues"]'
  echo "<<<FLOW_OUTPUT>>>"
  jq -cn --argjson approved "$1" --arg summary "$2" --argjson findings "$3" --argjson markers "${markers}" \
    '{markers: $markers, context: $summary, approved: $approved, summary: $summary, findings: $findings, off_diff_findings: []}'
  echo "<<<END_FLOW_OUTPUT>>>"
}

log=$(mktemp)
trap 'rm -f "${log}"' EXIT

status=0
cargo run -q --bin conductor -- workflow validate --all --templates --path . > "${log}" 2>&1 || status=$?
if [ "${status}" -eq 0 ]; then
  emit true "Workflow validation passed: $(tail -n 1 "${log}")" '[]'
  exit 0
fi

pr_files=""
if [ -n "${BASE_BRANCH:-}" ] && [[ "${BASE_BRANCH}" != *"{{"* ]]; then
  pr_files=$(git diff --name-only "origin/${BASE_BRANCH}...HEAD" 2>/dev/null || true)
fi
fallback_file=$(printf '%s\n' "${pr_files}" | grep -E '^(\.conductor/|conductor-core/templates/)' | head -n 1)

# FAIL lines name a workflow ("FAIL  ticket-to-pr") or a template
# ("FAIL  template:review-pr"); the ✗ lines after each are its errors.
findings=$(awk '
  /^FAIL[ \t]+/ { name = $2; next }
  /^[ \t]*✗/    { sub(/^[ \t]*✗[ \t]*/, ""); if (name != "") print name "\t" $0 }
' "${log}" | while IFS=$'\t' read -r name msg; do
  case "${name}" in
    (template:*) file="conductor-core/templates/${name#template:}.wft" ;;
    (*)          file=".conductor/workflows/${name}.wf" ;;
  esac
  if ! printf '%s\n' "${pr_files}" | grep -qxF "${file}"; then
    msg="${msg} (in ${file})"
    file="${fallback_file:-${file}}"
  fi
  jq -cn --arg file "${file}" --arg msg "${msg}" \
    '{file: $file, line: 1, severity: "critical", message: ("conductor workflow validate: " + $msg)}'
done | jq -s '.')

if [ "$(jq 'length' <<< "${findings}")" -eq 0 ]; then
  # The validator failed without a parseable error (it didn't build, or crashed).
  # Fail closed with its output.
  findings=$(jq -cn --arg file "${fallback_file:-.conductor/workflows}" --arg out "$(tail -n 20 "${log}")" --arg st "${status}" \
    '[{file: $file, line: 1, severity: "critical", message: ("conductor workflow validate exited " + $st + " without a parseable error: " + $out)}]')
fi

emit false "Workflow validation failed: $(grep -cE '^FAIL' "${log}") workflow(s) or template(s) have errors." "${findings}"
