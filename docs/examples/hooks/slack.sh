#!/usr/bin/env bash
# Conductor notification hook — Slack
#
# Posts a message to a Slack Incoming Webhook when a conductor event fires.
#
# Required environment variables:
#   SLACK_WEBHOOK_URL   — Slack Incoming Webhook URL
#                         (https://api.slack.com/messaging/webhooks)
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
#   run = "~/.conductor/hooks/slack.sh"
set -euo pipefail

: "${SLACK_WEBHOOK_URL:?SLACK_WEBHOOK_URL must be set}"

# Build message text
if [ -n "${CONDUCTOR_URL:-}" ]; then
  text="*Conductor* | \`${CONDUCTOR_EVENT}\` — <${CONDUCTOR_URL}|${CONDUCTOR_LABEL}>"
else
  text="*Conductor* | \`${CONDUCTOR_EVENT}\` — ${CONDUCTOR_LABEL}"
fi

curl -s -X POST \
  -H "Content-Type: application/json" \
  --data "{\"text\": \"${text}\"}" \
  "${SLACK_WEBHOOK_URL}"
