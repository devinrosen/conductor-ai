#!/usr/bin/env bash
# Conductor notification hook — ntfy (richer variant)
#
# Publishes a push notification via ntfy (https://ntfy.sh) with event-aware
# priority, per-event emoji tags, and optional bearer token auth. Use this
# instead of the minimal ntfy.sh when you want urgent/high priority alerts
# for failures and gate events, or when connecting to a private ntfy server
# that requires authentication.
#
# Required environment variables:
#   NTFY_TOPIC   — ntfy topic name (acts as a shared secret; keep it private)
#
# Optional environment variables:
#   NTFY_SERVER  — ntfy server base URL (default: https://ntfy.sh)
#   NTFY_TOKEN   — bearer auth token for private/self-hosted ntfy servers
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
#   run = "~/.conductor/hooks/notify-ntfy.sh"
set -euo pipefail

: "${NTFY_TOPIC:?NTFY_TOPIC must be set}"

NTFY_SERVER="${NTFY_SERVER:-https://ntfy.sh}"
EVENT="${CONDUCTOR_EVENT:-}"

# Map event to ntfy priority.
# urgent  → full-volume / bypass DND on most ntfy clients
# high    → elevated priority, still rings through
# default → standard notification
case "${EVENT}" in
  *.failed)
    PRIORITY="urgent"
    TAGS="rotating_light"
    ;;
  gate.waiting|gate.pending_too_long|feedback.requested)
    PRIORITY="high"
    TAGS="raising_hand"
    ;;
  *.cost_spike|*.duration_spike)
    PRIORITY="high"
    TAGS="chart_with_upwards_trend"
    ;;
  *.completed)
    PRIORITY="default"
    TAGS="white_check_mark"
    ;;
  *)
    PRIORITY="default"
    TAGS="bell"
    ;;
esac

# Optional Click deep-link header
click_header=()
if [ -n "${CONDUCTOR_URL:-}" ]; then
  click_header=(-H "Click: ${CONDUCTOR_URL}")
fi

# Optional bearer auth header (for private/self-hosted servers)
auth_header=()
if [ -n "${NTFY_TOKEN:-}" ]; then
  auth_header=(-H "Authorization: Bearer ${NTFY_TOKEN}")
fi

curl -s -X POST \
  "${NTFY_SERVER}/${NTFY_TOPIC}" \
  -H "Title: Conductor — ${EVENT}" \
  -H "Priority: ${PRIORITY}" \
  -H "Tags: ${TAGS}" \
  "${click_header[@]}" \
  "${auth_header[@]}" \
  -d "${CONDUCTOR_LABEL:-conductor event}"
