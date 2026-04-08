#!/usr/bin/env bash
# Conductor notification hook — macOS desktop notifications
#
# Displays a native macOS notification banner when a conductor event fires.
# Uses osascript (AppleScript), which is built into macOS — no extra tools needed.
#
# NOTE: macOS Focus mode will suppress these notifications. There is no way
# to bypass Focus mode from a CLI tool — this is a macOS system restriction.
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

case "$CONDUCTOR_EVENT" in
  workflow_run.failed)
    TITLE="Workflow Failed"
    SOUND="Basso"
    ;;
  workflow_run.completed)
    TITLE="Workflow Completed"
    SOUND="Glass"
    ;;
  *)
    TITLE="Conductor"
    SOUND="default"
    ;;
esac

# Pass variables as argv to avoid AppleScript injection via user-controlled strings.
osascript - "$CONDUCTOR_LABEL" "$TITLE" "$SOUND" <<'EOF'
on run argv
  set notifText to item 1 of argv
  set notifTitle to item 2 of argv
  set notifSound to item 3 of argv
  display notification notifText with title notifTitle sound name notifSound
end run
EOF
