#!/usr/bin/env bash
set -euo pipefail

# Fetch all data for a completed workflow run and write it to a scratch file.
# Env: RUN_ID — the workflow run ID to fetch.

mkdir -p .conductor/postmortems

# Fetch the run overview
run_output=$(conductor workflow run-show "$RUN_ID")

# Extract workflow name from the output (line like "Workflow: <name>")
workflow_name=$(echo "$run_output" | sed -n 's/^Workflow:[[:space:]]*\([^[:space:]]*\).*/\1/p' | head -1)

# Read the .wf file if it exists
wf_contents=""
if [[ -n "$workflow_name" ]]; then
  wf_path=".conductor/workflows/${workflow_name}.wf"
  if [[ -f "$wf_path" ]]; then
    wf_contents=$(cat "$wf_path")
  fi
fi

# Extract summary fields for context
status=$(echo "$run_output" | sed -n 's/^Status:[[:space:]]*\([^[:space:]]*\).*/\1/p' | head -1)
status=${status:-unknown}
step_count=$(echo "$run_output" | grep -c '^\s*Step ' || echo "0")
started_at=$(echo "$run_output" | sed -n 's/^Started:[[:space:]]*//p' | head -1)
ended_at=$(echo "$run_output" | sed -n 's/^Ended:[[:space:]]*//p' | head -1)

# Compute elapsed time if both timestamps are available
elapsed=""
if [[ -n "$started_at" && -n "$ended_at" ]]; then
  start_epoch=$(date -jf "%Y-%m-%dT%H:%M:%S" "${started_at%%.*}" +%s 2>/dev/null || date -d "$started_at" +%s 2>/dev/null || true)
  end_epoch=$(date -jf "%Y-%m-%dT%H:%M:%S" "${ended_at%%.*}" +%s 2>/dev/null || date -d "$ended_at" +%s 2>/dev/null || true)
  if [[ -n "$start_epoch" && -n "$end_epoch" ]]; then
    diff_secs=$((end_epoch - start_epoch))
    elapsed="${diff_secs}s"
  fi
fi

# Write gathered data to scratch file
output_file=".conductor/postmortems/.fetch-${RUN_ID}.md"
{
  echo "# Workflow Run Data: ${RUN_ID}"
  echo ""
  echo "## Run Overview"
  echo ""
  echo '```'
  echo "$run_output"
  echo '```'
  if [[ -n "$wf_contents" ]]; then
    echo ""
    echo "## Workflow Definition (${workflow_name}.wf)"
    echo ""
    echo '```'
    echo "$wf_contents"
    echo '```'
  fi
} > "$output_file"

# Build context summary
context="workflow=${workflow_name:-unknown}, status=${status}, steps=${step_count}"
if [[ -n "$elapsed" ]]; then
  context="${context}, elapsed=${elapsed}"
fi

cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": "${context}"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
