#!/usr/bin/env bash
set -euo pipefail

# BASE_BRANCH is injected by the workflow from the resolve-pr-base step.
# We refuse to silently fall back to a guess — a wrong base produces a
# fabricated diff that contaminates the downstream review. See #2736.
if [ -z "${BASE_BRANCH:-}" ]; then
  echo "ERROR: BASE_BRANCH env var not set — workflow must run resolve-pr-base first." >&2
  exit 1
fi

# Get changed files relative to the PR base branch
changed_files=$(git diff "origin/${BASE_BRANCH}...HEAD" --name-only 2>/dev/null || true)

# Filter for pipeline files: workflows, scripts, schemas, agent and prompt
# briefs, and the workflow templates shipped by conductor-core.
workflow_files=()
while IFS= read -r file; do
  [[ -z "$file" ]] && continue
  case "$file" in
    .conductor/workflows/*|.conductor/scripts/*|.conductor/schemas/*|.conductor/agents/*|.conductor/prompts/*|conductor-core/templates/*)
      workflow_files+=("$file")
      ;;
  esac
done <<< "$changed_files"

count=${#workflow_files[@]}

if [ "$count" -gt 0 ]; then
  file_list=$(IFS=", "; echo "${workflow_files[*]}")
  cat <<EOF2
<<<FLOW_OUTPUT>>>
{"markers": ["has_workflow_changes"], "context": "Found ${count} workflow/pipeline file(s) in diff: ${file_list}"}
<<<END_FLOW_OUTPUT>>>
EOF2
else
  cat <<'EOF2'
<<<FLOW_OUTPUT>>>
{"markers": [], "context": "No workflow/pipeline files in diff"}
<<<END_FLOW_OUTPUT>>>
EOF2
fi
