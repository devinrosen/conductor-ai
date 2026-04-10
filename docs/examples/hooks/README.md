# Conductor Notification Hook Examples

Ready-to-use shell scripts for common notification destinations. Each script reads
`CONDUCTOR_*` environment variables injected automatically by the hook engine.

## Available Scripts

| Script | Destination | Requires |
|---|---|---|
| `slack.sh` | Slack Incoming Webhook | `SLACK_WEBHOOK_URL` |
| `discord.sh` | Discord Webhook | `DISCORD_WEBHOOK_URL` |
| `ntfy.sh` | ntfy push notifications (minimal) | `NTFY_TOPIC` |
| `notify-ntfy.sh` | ntfy push notifications (richer: event-aware priority, tags, auth) | `NTFY_TOPIC` |
| `notify-ntfy.py` | ntfy push notifications (Python, stdlib-only) | `NTFY_TOPIC` |
| `macos-notify.sh` | macOS desktop notification | macOS (uses `osascript`, built-in) |

> **ntfy variants:** `ntfy.sh` is the minimal script — one `curl` call with a
> title and a click link. `notify-ntfy.sh` (and its Python twin `notify-ntfy.py`)
> add event-aware `Priority` headers (`urgent` for failures, `high` for gate and
> feedback events), per-event emoji `Tags`, and optional `Authorization` bearer
> token support for private/self-hosted ntfy servers. See
> [docs/notify-ntfy-migration.md](../../notify-ntfy-migration.md) for the full
> migration guide.

## Environment Variables

Conductor injects these into every shell hook process:

| Variable | Description |
|---|---|
| `CONDUCTOR_EVENT` | Event name, e.g. `workflow_run.completed` |
| `CONDUCTOR_RUN_ID` | Run ID (ULID) |
| `CONDUCTOR_LABEL` | Human-readable label, e.g. `deploy on main` |
| `CONDUCTOR_TIMESTAMP` | ISO 8601 timestamp |
| `CONDUCTOR_URL` | Deep-link URL (empty string when not available) |

Additional variables for specific events:

| Variable | Present on |
|---|---|
| `CONDUCTOR_ERROR` | `agent_run.failed` |
| `CONDUCTOR_MULTIPLE` | `workflow_run.cost_spike`, `workflow_run.duration_spike` |
| `CONDUCTOR_STEP_NAME` | `gate.waiting`, `gate.pending_too_long` |
| `CONDUCTOR_PENDING_MS` | `gate.pending_too_long` |
| `CONDUCTOR_PROMPT_PREVIEW` | `feedback.requested` |

## Setup

1. Copy the script(s) you want to `~/.conductor/hooks/` and make them executable:

   ```bash
   mkdir -p ~/.conductor/hooks
   cp slack.sh ~/.conductor/hooks/
   chmod +x ~/.conductor/hooks/slack.sh
   ```

2. Ensure notifications are enabled in `~/.conductor/config.toml`:

   ```toml
   [notifications]
   enabled = true
   ```

3. Add a `[[notify.hooks]]` entry to `~/.conductor/config.toml`:

   ```toml
   # Fire on all workflow events
   [[notify.hooks]]
   on  = "workflow_run.*"
   run = "~/.conductor/hooks/slack.sh"

   # Fire on gate-waiting events only
   [[notify.hooks]]
   on  = "gate.waiting"
   run = "~/.conductor/hooks/macos-notify.sh"

   # Fire on everything
   [[notify.hooks]]
   on  = "*"
   run = "~/.conductor/hooks/ntfy.sh"
   ```

4. Test a hook from the CLI:

   ```bash
   conductor notifications test workflow_run.completed
   ```

   Or from the web UI: **Settings → Notification Hooks → Send test event**.

## Event Names

| Event | Fires when |
|---|---|
| `workflow_run.completed` | Workflow run finishes successfully |
| `workflow_run.failed` | Workflow run finishes with a failure |
| `workflow_run.cost_spike` | Run cost exceeds `threshold_multiple` × baseline |
| `workflow_run.duration_spike` | Run duration exceeds `threshold_multiple` × baseline |
| `agent_run.completed` | Standalone agent run finishes successfully |
| `agent_run.failed` | Standalone agent run fails |
| `gate.waiting` | Workflow gate is waiting for human action |
| `gate.pending_too_long` | Gate has been waiting longer than `gate_pending_ms` |
| `feedback.requested` | Agent is waiting for human feedback input |

Use `*` to match all events, `workflow_run.*` to match all workflow events, or
an exact event name to match only that event.

## HTTP Hooks

For HTTP POST hooks (instead of shell scripts), use the `url` key:

```toml
[[notify.hooks]]
on  = "workflow_run.*"
url = "https://hooks.example.com/conductor"
headers = { Authorization = "$MY_WEBHOOK_TOKEN" }
timeout_ms = 5000
```

Header values starting with `$` are resolved from the environment. The payload
is a JSON object with the same fields as the environment variables above.
