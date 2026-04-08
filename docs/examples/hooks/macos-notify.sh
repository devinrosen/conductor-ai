#!/usr/bin/env bash
# Conductor notification hook — macOS desktop notifications
#
# Displays a native macOS notification banner when a conductor event fires.
# Uses osascript (AppleScript), which is built into macOS — no extra tools needed.
#
# Conductor injects these automatically:
#   CONDUCTOR_EVENT     — event name, e.g. "workflow_run.completed"
#   CONDUCTOR_LABEL     — human-readable label, e.g. "deploy on main"
#   CONDUCTOR_URL       — deep-link URL (empty string if not available)
#   CONDUCTOR_RUN_ID    — run ID
#   CONDUCTOR_TIMESTAMP — ISO 8601 timestamp
#
# Example config.toml entry:
#   [[notify.hooks]]
#   on  = "workflow_run.*"
#   run = "~/.conductor/hooks/macos-notify.sh"
set -euo pipefail

# macOS only
if [[ "$(uname)" != "Darwin" ]]; then
  echo "macos-notify.sh: not macOS, skipping" >&2
  exit 0
fi

title="Conductor"
subtitle="${CONDUCTOR_EVENT}"
message="${CONDUCTOR_LABEL}"

osascript -e "display notification \"${message}\" with title \"${title}\" subtitle \"${subtitle}\""
