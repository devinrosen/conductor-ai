#!/usr/bin/env bash
set -euo pipefail

# Fetch agent conversation logs for failed steps in a workflow run.
# Appends log content to the scratch file created by fetch-run-data.sh.
# Env: RUN_ID — the workflow run ID to inspect.

SCRATCH_FILE=".conductor/postmortems/.fetch-${RUN_ID}.md"

if [[ ! -f "$SCRATCH_FILE" ]]; then
  echo "Error: scratch file not found at ${SCRATCH_FILE} — run fetch-run-data first."
  exit 1
fi

# Fetch the run overview to find failed steps
run_output=$(conductor workflow run-show "$RUN_ID")

# Extract failed step lines: look for [✗] markers and capture step names + child run IDs
logs_fetched=0

# Parse step blocks from run-show output.
# Failed steps have [✗] status and may include "child run: <id>" lines.
current_step=""
current_child_run=""
in_failed_step=false

while IFS= read -r line; do
  # Detect step lines like "  Step 3: implement [✗]"
  if echo "$line" | grep -qE '^\s*Step [0-9]+:.*\[✗\]'; then
    # Extract step name (between ": " and " [")
    current_step=$(echo "$line" | sed -n 's/.*Step [0-9]*:[[:space:]]*\([^[]*\)\[.*/\1/p' | xargs)
    in_failed_step=true
    current_child_run=""
  elif echo "$line" | grep -qE '^\s*Step [0-9]+:'; then
    # New non-failed step — reset
    in_failed_step=false
    current_step=""
    current_child_run=""
  fi

  # Within a failed step, look for child run ID
  if [[ "$in_failed_step" == "true" ]]; then
    child_id=$(echo "$line" | sed -n 's/.*child run:[[:space:]]*\([^[:space:]]*\).*/\1/p')
    if [[ -n "$child_id" ]]; then
      current_child_run="$child_id"

      # Try to read the agent log file
      log_path="${HOME}/.conductor/agent-logs/${current_child_run}.log"

      {
        echo ""
        echo "## Agent Log: ${current_step}"
        echo ""
        echo "Child run ID: \`${current_child_run}\`"
        echo ""
      } >> "$SCRATCH_FILE"

      if [[ -f "$log_path" ]]; then
        line_count=$(wc -l < "$log_path")
        if [[ "$line_count" -gt 500 ]]; then
          {
            echo "_Log truncated to last 500 of ${line_count} lines._"
            echo ""
            echo '```'
            tail -500 "$log_path"
            echo '```'
          } >> "$SCRATCH_FILE"
        else
          {
            echo '```'
            cat "$log_path"
            echo '```'
          } >> "$SCRATCH_FILE"
        fi
        logs_fetched=$((logs_fetched + 1))
      else
        {
          echo "_Agent log not available at ${log_path} — the agent session may have been lost._"
          echo ""
        } >> "$SCRATCH_FILE"
      fi

      in_failed_step=false
    fi
  fi
done <<< "$run_output"

cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": "Fetched ${logs_fetched} agent log(s) for failed steps"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
