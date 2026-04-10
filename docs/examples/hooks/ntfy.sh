#!/usr/bin/env bash
# Conductor notification hook — ntfy
#
# Publishes a push notification via ntfy (https://ntfy.sh) when a conductor
# event fires. Works with the public ntfy.sh server or a self-hosted instance.
#
# Required environment variables:
#   NTFY_TOPIC   — ntfy topic name (acts as a shared secret; keep it private)
#
# Optional environment variables:
#   NTFY_SERVER  — ntfy server base URL (default: https://ntfy.sh)
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
#   on  = "*"
#   run = "~/.conductor/hooks/ntfy.sh"
set -euo pipefail

: "${NTFY_TOPIC:?NTFY_TOPIC must be set}"

NTFY_SERVER="${NTFY_SERVER:-https://ntfy.sh}"

# Build optional Click header in an array so spaces/glob chars in the URL
# are handled safely and do not cause word splitting or argument injection.
click_header=()
if [ -n "${CONDUCTOR_URL:-}" ]; then
  click_header=(-H "Click: ${CONDUCTOR_URL}")
fi

curl -s -X POST \
  "${NTFY_SERVER}/${NTFY_TOPIC}" \
  -H "Title: Conductor — ${CONDUCTOR_EVENT}" \
  -H "Tags: bell" \
  "${click_header[@]}" \
  -d "${CONDUCTOR_LABEL}"
