#!/usr/bin/env python3
"""
Conductor notification hook — ntfy (Python variant)

Publishes a push notification via ntfy (https://ntfy.sh) with event-aware
priority, per-event emoji tags, and optional bearer token auth. This script
uses the Python standard library only (no pip dependencies).

Required environment variables:
  NTFY_TOPIC   — ntfy topic name (acts as a shared secret; keep it private)

Optional environment variables:
  NTFY_SERVER  — ntfy server base URL (default: https://ntfy.sh)
  NTFY_TOKEN   — bearer auth token for private/self-hosted ntfy servers

Conductor injects these automatically:
  CONDUCTOR_EVENT     — event name, e.g. "workflow_run.completed"
  CONDUCTOR_LABEL     — human-readable label, e.g. "deploy on main"
  CONDUCTOR_URL       — deep-link URL (empty string if not available)
  CONDUCTOR_RUN_ID    — run ID
  CONDUCTOR_TIMESTAMP — ISO 8601 timestamp

Example config.toml entry:
  [[notify.hooks]]
  on  = "*"
  run = "~/.conductor/hooks/notify-ntfy.py"
"""

import os
import sys
import urllib.request

topic = os.environ.get("NTFY_TOPIC", "")
if not topic:
    print("NTFY_TOPIC must be set", file=sys.stderr)
    sys.exit(1)

server = os.environ.get("NTFY_SERVER", "https://ntfy.sh").rstrip("/")
event = os.environ.get("CONDUCTOR_EVENT", "")
label = os.environ.get("CONDUCTOR_LABEL", "conductor event")
url = os.environ.get("CONDUCTOR_URL", "")
token = os.environ.get("NTFY_TOKEN", "")

# Map event to ntfy priority and emoji tag.
if event.endswith(".failed"):
    priority = "urgent"
    tags = "rotating_light"
elif event in ("gate.waiting", "gate.pending_too_long", "feedback.requested"):
    priority = "high"
    tags = "raising_hand"
elif event.endswith(".cost_spike") or event.endswith(".duration_spike"):
    priority = "high"
    tags = "chart_with_upwards_trend"
elif event.endswith(".completed"):
    priority = "default"
    tags = "white_check_mark"
else:
    priority = "default"
    tags = "bell"

headers = {
    "Title": f"Conductor \u2014 {event}",
    "Priority": priority,
    "Tags": tags,
}
if url:
    headers["Click"] = url
if token:
    headers["Authorization"] = f"Bearer {token}"

req = urllib.request.Request(
    f"{server}/{topic}",
    data=label.encode(),
    headers=headers,
    method="POST",
)

try:
    with urllib.request.urlopen(req) as resp:
        resp.read()
except urllib.error.URLError as exc:
    print(f"ntfy request failed: {exc}", file=sys.stderr)
    sys.exit(1)
