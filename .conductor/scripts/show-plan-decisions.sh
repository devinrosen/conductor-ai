#!/usr/bin/env bash
# show-plan-decisions.sh — list the plan's open decisions before the
# human_plan_decisions gate (#3128). A gate's prompt is fixed text, so the
# questions go in this step's context, where whoever answers the gate reads them.
# Env: PRIOR_OUTPUT — the plan step's structured output (task-plan schema).
set -euo pipefail

emit() {
  jq -cn --arg ctx "$1" '{markers: [], context: $ctx}' \
    | { printf '<<<FLOW_OUTPUT>>>\n'; cat; printf '<<<END_FLOW_OUTPUT>>>\n'; }
}

if ! decisions=$(jq -r '
    (.decisions // []) | to_entries
    | map("\(.key + 1). \(.value.question // "(no question)")\n   Default: \(.value.default // "(none given)")")
    | join("\n")' <<< "${PRIOR_OUTPUT:-}" 2>/dev/null) || [ -z "${decisions}" ]; then
  # The gate still runs; point at PLAN.md rather than failing the run.
  emit "The plan flagged decisions for you, but they couldn't be read from its output. See \"Decisions for the user\" in the worktree's PLAN.md (\$(git rev-parse --git-dir)/PLAN.md)."
  exit 0
fi

emit "The plan needs your decision before implementing:

${decisions}

Approve the gate with your answers as feedback, or approve without feedback to keep the defaults. Reject to stop the run."
