#!/usr/bin/env bash
# Conductor notification hook — Discord
#
# Posts a message to a Discord Webhook when a conductor event fires.
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

# Build message content
if [ -n "${CONDUCTOR_URL:-}" ]; then
  content="**Conductor** | \`${CONDUCTOR_EVENT}\` — [${CONDUCTOR_LABEL}](${CONDUCTOR_URL})"
else
  content="**Conductor** | \`${CONDUCTOR_EVENT}\` — ${CONDUCTOR_LABEL}"
fi

curl -s -X POST \
  -H "Content-Type: application/json" \
  --data "{\"content\": \"${content}\"}" \
  "${DISCORD_WEBHOOK_URL}"
