#!/usr/bin/env bash
# Conductor notification hook — Discord
#
# Posts a message to a Discord Webhook when a conductor event fires.
#
# Requires: curl, jq
#
# Required environment variables:
#   DISCORD_WEBHOOK_URL — Discord Webhook URL
#                         (Server Settings → Integrations → Webhooks)
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
#   run = "~/.conductor/hooks/discord.sh"
set -euo pipefail

: "${DISCORD_WEBHOOK_URL:?DISCORD_WEBHOOK_URL must be set}"

# Build message content — jq handles all JSON escaping for CONDUCTOR_* values.
if [ -n "${CONDUCTOR_URL:-}" ]; then
  content=$(jq -rn --arg event "${CONDUCTOR_EVENT}" --arg label "${CONDUCTOR_LABEL}" \
               --arg url "${CONDUCTOR_URL}" \
               '"**Conductor** | `\($event)` — [\($label)](\($url))"')
else
  content=$(jq -rn --arg event "${CONDUCTOR_EVENT}" --arg label "${CONDUCTOR_LABEL}" \
               '"**Conductor** | `\($event)` — \($label)"')
fi

payload=$(jq -n --arg content "${content}" '{"content": $content}')

curl -s -X POST \
  -H "Content-Type: application/json" \
  --data "${payload}" \
  "${DISCORD_WEBHOOK_URL}"
