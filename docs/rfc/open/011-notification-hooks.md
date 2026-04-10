# RFC 011: Notification Hooks

**Status:** Draft
**Date:** 2026-04-07
**Author:** Devin

---

## Problem

Conductor's current notification system has built-in support for three outbound channels: desktop notifications (`notify-rust`), Slack webhooks, and Web Push (browser). This approach has two compounding problems:

**Configuration is buried.** Every channel requires manual editing of `~/.conductor/config.toml`. There is no UI surface in the TUI, and the web Settings page only exposes Web Push. Users have no discovery path to Slack setup and no way to verify their config is working.

**Adding channels requires Conductor code changes.** Each new integration request (Discord, PagerDuty, ntfy, email, SMS) means new crate dependencies, new config structs, new tests, and a new PR. The set of channels users want is unbounded; Conductor can't scale to own all of them.

These problems have the same root cause: Conductor is in the business of routing notifications to specific channels, which is not its core responsibility.

---

## Proposed Design

### Core Idea

Conductor fires **hooks** at lifecycle events. Each hook is a shell command or HTTP POST. Users wire those hooks to whatever communication tool they want. Conductor owns the events and payloads; users own the routing.

```toml
# ~/.conductor/config.toml

[[notify.hooks]]
on = "workflow_run.failed"
run = "~/.conductor/hooks/alert.sh"

[[notify.hooks]]
on = "gate.waiting"
url = "https://hooks.slack.com/services/T.../B.../..."

[[notify.hooks]]
on = "workflow_run.*"
run = "osascript -e 'display notification \"{{workflow_name}} finished\" with title \"Conductor\"'"
```

This replaces all built-in channel implementations. Slack, Discord, ntfy, PagerDuty, macOS notifications, and anything else become one-liners or small scripts the developer owns.

---

### Event Taxonomy

| Event | When |
|---|---|
| `workflow_run.completed` | Workflow finished successfully |
| `workflow_run.failed` | Workflow finished with failure |
| `workflow_run.stale` | A step has been running longer than `stale_workflow_minutes` |
| `workflow_run.reaped` | Dead workflow detected — agent confirmed dead, run marked failed |
| `workflow_run.orphan_resumed` | Stuck workflow runs auto-resumed by recovery |
| `workflow_run.cost_spike` | Run cost exceeded threshold multiple of rolling average |
| `workflow_run.duration_spike` | Run duration exceeded threshold multiple of P75 |
| `agent_run.completed` | Agent run finished successfully |
| `agent_run.failed` | Agent run finished with failure |
| `gate.waiting` | Gate is blocked and requires action (fires immediately) |
| `gate.pending_too_long` | Gate has been waiting longer than a configured threshold |
| `feedback.requested` | Agent is waiting for user input |

Event names support glob matching in `on`:
- `workflow_run.*` matches all `workflow_run` events including spikes
- `gate.*` matches both `gate.waiting` and `gate.pending_too_long`
- `*` matches all events

---

### Hook Types

#### Shell command hooks (`run`)

Conductor executes the command in a subprocess. Event data is passed as environment variables — no template syntax required, compatible with any language.

```bash
# Environment variables available to the command:
CONDUCTOR_EVENT=workflow_run.failed
CONDUCTOR_WORKFLOW_NAME=deploy-staging
CONDUCTOR_RUN_ID=01J...
CONDUCTOR_LABEL=my-repo
CONDUCTOR_STATUS=failed
CONDUCTOR_ERROR="step 'run tests' exited with code 1"
CONDUCTOR_URL=http://localhost:4747/runs/01J...
CONDUCTOR_TIMESTAMP=2026-04-07T14:23:00Z
```

String interpolation (`{{workflow_name}}`) is also supported for inline one-liners:

```toml
[[notify.hooks]]
on = "workflow_run.completed"
run = "osascript -e 'display notification \"{{workflow_name}} done\" with title \"Conductor\"'"
```

#### HTTP webhook hooks (`url`)

Conductor POSTs a JSON body to the URL. The payload mirrors the env var set:

```json
{
  "event": "gate.waiting",
  "workflow_name": "deploy-staging",
  "run_id": "01J...",
  "label": "my-repo",
  "status": "waiting",
  "gate_type": "human_approval",
  "gate_prompt": "Approve deployment to production?",
  "url": "http://localhost:4747/runs/01J...",
  "timestamp": "2026-04-07T14:23:00Z"
}
```

Optional `headers` for auth tokens:

```toml
[[notify.hooks]]
on = "gate.waiting"
url = "https://api.example.com/conductor-events"
headers = { "Authorization" = "Bearer $MY_TOKEN" }
```

Header values beginning with `$` are resolved from environment variables at hook execution time — no secrets in `config.toml`.

---

### Event Payload Fields

All events include a common base set. Event-specific fields are additive.

**Common (all events):**

| Field | Type | Description |
|---|---|---|
| `event` | string | Event name (e.g. `workflow_run.failed`) |
| `run_id` | string | ULID of the workflow or agent run |
| `label` | string | Repo label / target |
| `timestamp` | string | ISO 8601 |
| `url` | string | Deep link into conductor-web (if running) |

**`workflow_run.completed` / `workflow_run.failed`:**

| Field | Type | Description |
|---|---|---|
| `workflow_name` | string | Workflow file name |
| `status` | string | `completed` or `failed` |
| `error` | string? | Error message if failed |
| `duration_ms` | integer? | Wall-clock duration |
| `cost_usd` | number? | Token cost of the run in USD |

**`workflow_run.cost_spike`:**

| Field | Type | Description |
|---|---|---|
| `workflow_name` | string | Workflow file name |
| `cost_usd` | number | Token cost of this run in USD |
| `avg_cost_usd` | number | Rolling 30-day average cost in USD |
| `multiple` | number | `cost_usd / avg_cost_usd` (e.g. `4.2`) |
| `duration_ms` | integer | Wall-clock duration |

**`workflow_run.duration_spike`:**

| Field | Type | Description |
|---|---|---|
| `workflow_name` | string | Workflow file name |
| `duration_ms` | integer | Wall-clock duration of this run |
| `p75_duration_ms` | integer | P75 duration over past 30 days |
| `multiple` | number | `duration_ms / p75_duration_ms` (e.g. `3.1`) |
| `cost_usd` | number? | Token cost of the run in USD |

**`agent_run.*`:**

| Field | Type | Description |
|---|---|---|
| `agent_name` | string | Agent definition name |
| `workflow_name` | string? | Parent workflow if applicable |
| `status` | string | `completed` or `failed` |
| `error` | string? | Error message if failed |

**`gate.waiting`:**

| Field | Type | Description |
|---|---|---|
| `workflow_name` | string | Parent workflow |
| `step_name` | string | Gate step name |
| `gate_type` | string | `human_approval`, `pr_review`, `ci`, `quality` |
| `gate_prompt` | string? | Prompt text shown at the gate |

**`gate.pending_too_long`:**

| Field | Type | Description |
|---|---|---|
| `workflow_name` | string | Parent workflow |
| `step_name` | string | Gate step name |
| `gate_type` | string | `human_approval`, `pr_review`, `ci`, `quality` |
| `gate_prompt` | string? | Prompt text shown at the gate |
| `wait_duration_ms` | integer | How long the gate has been waiting |
| `threshold_ms` | integer | Configured threshold that was exceeded |

**`feedback.requested`:**

| Field | Type | Description |
|---|---|---|
| `agent_name` | string | Agent requesting feedback |
| `workflow_name` | string? | Parent workflow if applicable |
| `prompt` | string | The question/prompt from the agent |

---

### Config Schema

```toml
[[notify.hooks]]
on = "workflow_run.failed"         # required: event name or glob
run = "~/.conductor/hooks/fail.sh" # required (or url, not both)

[[notify.hooks]]
on = "gate.waiting"
url = "https://hooks.slack.com/services/..."  # required (or run, not both)
headers = { "X-Token" = "$SLACK_TOKEN" }      # optional

[[notify.hooks]]
on = "workflow_run.*"
run = "notify-send Conductor '{{workflow_name}} {{status}}'"
timeout_ms = 5000                  # optional, default 10000

# Spike/anomaly events: threshold_multiple and gate_pending_ms only apply
# to the matching event types; they are ignored on other events.
[[notify.hooks]]
on = "workflow_run.cost_spike"
workflow = "deploy-prod"           # optional: limit to a specific workflow
threshold_multiple = 3.0           # fire if cost > 3x rolling 30-day avg (default: 3.0)
run = "~/.conductor/hooks/cost-alert.sh"

[[notify.hooks]]
on = "workflow_run.duration_spike"
threshold_multiple = 2.0           # fire if duration > 2x P75 (default: 2.0)
url = "https://hooks.slack.com/services/..."

[[notify.hooks]]
on = "gate.pending_too_long"
gate_pending_ms = 14400000         # fire if gate waiting > 4h (default: 3600000 / 1h)
run = "~/.conductor/hooks/gate-alert.sh"
```

```rust
pub struct HookConfig {
    /// Event name or glob pattern (e.g. "workflow_run.*", "*")
    pub on: String,
    /// Shell command to execute (mutually exclusive with url)
    pub run: Option<String>,
    /// HTTP URL to POST to (mutually exclusive with run)
    pub url: Option<String>,
    /// HTTP headers for url hooks; values starting with "$" are env-resolved
    pub headers: Option<HashMap<String, String>>,
    /// Timeout in milliseconds (default: 10000)
    pub timeout_ms: Option<u64>,
    /// Only for workflow_run.cost_spike / workflow_run.duration_spike:
    /// fire if metric exceeds this multiple of baseline (default: 3.0 for cost, 2.0 for duration)
    pub threshold_multiple: Option<f64>,
    /// Only for gate.pending_too_long: fire after gate has waited this many ms (default: 3_600_000 / 1h)
    pub gate_pending_ms: Option<u64>,
    /// Only for spike events: limit hook to a specific workflow name (default: all workflows)
    pub workflow: Option<String>,
}
```

---

### Hook Runner

`HookRunner` lives in `conductor-core/src/hooks.rs`.

```rust
pub struct HookRunner<'a> {
    hooks: &'a [HookConfig],
}

impl<'a> HookRunner<'a> {
    pub fn fire(&self, event: &NotificationEvent) {
        for hook in self.hooks {
            if glob_matches(&hook.on, &event.kind) {
                let hook = hook.clone();
                let payload = event.to_payload();
                std::thread::spawn(move || {
                    let result = match (&hook.run, &hook.url) {
                        (Some(cmd), _) => run_shell_hook(&cmd, &payload, hook.timeout_ms),
                        (_, Some(url)) => run_http_hook(&url, &hook.headers, &payload, hook.timeout_ms),
                        _ => Err("hook has neither run nor url".into()),
                    };
                    if let Err(e) = result {
                        log::warn!("hook '{}' failed: {}", hook.on, e);
                    }
                });
            }
        }
    }
}
```

Hooks are fire-and-forget (spawned threads). Failures are logged as warnings and never propagated — same behavior as the current Slack sender. No retries in v1.

---

### Updated Dispatch Flow

`dispatch_notification()` in `conductor-core/src/notify.rs` currently has four steps. With this change:

1. **Claim dedup slot** — `try_claim_notification()` via `notification_log` table (unchanged)
2. **Persist in-app notification** — insert into `notifications` table (unchanged)
3. **Fire hooks** — `HookRunner::fire(&event)` (replaces steps 3 and 4)

---

### What Gets Removed

| Current | Replacement |
|---|---|
| Built-in Slack webhook sender (`notify.rs`) | User-configured `url` or `run` hook |
| `notify-rust` desktop notification calls | User-configured `run` hook (e.g. `osascript`, `notify-send`) |
| Web Push / VAPID infrastructure (`push.rs`, `main.rs` key generation) | User-configured `url` hook pointing at any push service |
| `[notifications.slack]` config block | `[[notify.hooks]]` entries |
| `[web_push]` config block | Removed |
| `[notifications.workflows]` per-event flags | Replaced by which events you put hooks on |

**Crate dependencies removed:** `notify-rust`, `web-push`, `p256`, `hmac`, `sha2`, `base64` (from web crate).

**What stays unchanged:**
- In-app notification bell and panel in conductor-web (intrinsic to Conductor UX)
- `NotificationManager` and `notifications` SQLite table
- `notification_log` deduplication table and logic
- `NotificationSeverity` enum and in-app notification creation

---

### Web Settings Page

The Settings page currently exposes a Web Push toggle that no longer applies. It should be replaced with:

- A read-only list of configured hooks (sourced from `GET /api/config/hooks`)
- A "send test event" button per hook (fires a synthetic `workflow_run.completed` event)
- A link to documentation with example hook scripts

Full hook editing stays in `config.toml` — the Settings page is informational only. This avoids building a TOML editor in the browser while still making the configuration discoverable.

---

### Example Hook Scripts

These ship as examples in `docs/examples/hooks/` (not installed automatically):

**Slack:**
```bash
#!/bin/bash
# ~/.conductor/hooks/slack.sh
curl -s -X POST "$SLACK_WEBHOOK_URL" \
  -H 'Content-type: application/json' \
  -d "{\"text\": \"[$CONDUCTOR_EVENT] $CONDUCTOR_WORKFLOW_NAME on $CONDUCTOR_LABEL\"}"
```

**macOS notification:**
```toml
[[notify.hooks]]
on = "gate.waiting"
run = "osascript -e 'display notification \"{{gate_type}}: {{workflow_name}}\" with title \"Conductor — Action Required\"'"
```

**ntfy.sh:**
```bash
#!/bin/bash
curl -s "$NTFY_URL" \
  -H "Title: Conductor — $CONDUCTOR_EVENT" \
  -d "$CONDUCTOR_WORKFLOW_NAME on $CONDUCTOR_LABEL: $CONDUCTOR_STATUS"
```

**Discord:**
```bash
#!/bin/bash
curl -s -X POST "$DISCORD_WEBHOOK_URL" \
  -H 'Content-type: application/json' \
  -d "{\"content\": \"**$CONDUCTOR_EVENT** — $CONDUCTOR_WORKFLOW_NAME ($CONDUCTOR_LABEL)\"}"
```

---

## Migration

Users currently using built-in Slack or desktop notifications will need to add hook entries to `config.toml`. The old `[notifications.slack]` and `[web_push]` config blocks will be ignored (logged as deprecation warnings) in the first release, then removed in the following one.

A migration note in the changelog with copy-paste hook snippets covers the Slack case. Desktop notifications require a one-liner depending on the OS.

---

## Decisions Made

1. **Hooks replace all built-in channels.** Conductor is not in the notification routing business. The in-app notification bell is the one exception — it's intrinsic to Conductor's own UX.

2. **Env vars are the primary payload mechanism for shell hooks.** No template syntax to learn; works in any language. String interpolation (`{{field}}`) is supported as a convenience for inline one-liners only.

3. **Fire-and-forget with no retries.** Consistent with existing behavior. Failures are logged. Retry logic belongs in the hook script if needed.

4. **HTTP header values starting with `$` resolve from env.** No secrets in `config.toml`. Consistent with how the codebase handles credentials elsewhere (e.g. RFC 007 `api_key_env`).

5. **Glob matching on event names.** `workflow_run.*` is the most common real-world use case (notify on any workflow terminal state). Simple prefix glob is sufficient; full regex is not needed.

6. **Settings page becomes informational, not editable.** Avoids building a config editor in the browser. Discoverability is served by listing configured hooks and offering a test-fire button.

7. **Spike and anomaly detection uses per-hook thresholds, not global config.** `threshold_multiple` and `gate_pending_ms` live on the hook entry that subscribes to the event, not in a global `[notify]` block. This lets users set different thresholds for different workflows (e.g. tighter cost alerting on expensive workflows). Defaults: 3.0× for cost spikes, 2.0× for duration spikes, 1 hour for gate timeouts.

8. **Baseline requires a minimum of 5 runs.** Cost and duration spike events are suppressed until there are at least 5 completed runs for the workflow in the rolling window. Avoids false positives on new or rarely-run workflows.

9. **`gate.pending_too_long` is deduplicated per run per threshold crossing.** Once fired for a run, it will not re-fire on subsequent background poll ticks for the same run. This is handled via the existing `notification_log` deduplication table.

10. **`workflow_run.cost_spike` and `workflow_run.duration_spike` are additive to `workflow_run.failed`.** A failed run that also cost 5× the average will fire both `workflow_run.failed` and `workflow_run.cost_spike`. Hooks subscribing to `workflow_run.*` receive all of them.

> **Note:** This RFC supersedes GitHub issue [#1836](https://github.com/devinrosen/conductor-ai/issues/1836) (notification hooks for workflow failures and cost/duration spikes). The use cases from that issue are fully covered by the event taxonomy and config schema above.

---

## Open Questions

1. **Inbound Slack slash commands** (`/conductor active`) are explicitly out of scope. They're a "remote control" feature — external systems querying or controlling Conductor — not a notification feature, and the two directions deserve separate designs. The existing Slack slash command handler can stay as-is while RFC 012 designs a generic external control API that the Slack handler would eventually become a thin adapter on top of. See [RFC 012](012-external-control-api.md).

2. **Hook ordering and fan-out:** If multiple hooks match the same event, they all fire concurrently in separate threads. Should there be a way to express sequential hooks or dependencies? Probably not needed in v1.

3. **Sensitive data in payloads:** `gate_prompt` and `feedback.requested.prompt` may contain LLM output with sensitive content. Should hooks be opt-in for those fields, or is it the user's responsibility? The current Slack sender already uses `escape_slack_mrkdwn()` to sanitize — with hooks, that responsibility shifts to the user's script.

4. **Test-fire from Settings page:** The API endpoint for this (`POST /api/config/hooks/test`) needs a synthetic event shape. Should it use a fixed example payload or reflect the most recent real event of that type?

5. **Hook script discoverability:** Should `conductor setup` emit a starter `config.toml` snippet with commented-out hook examples? This would address the "buried config" problem directly for new users.

---

## Implementation Order

1. Define `NotificationEvent` struct and `to_payload()` serialization
2. Add `HookConfig` to `Config` struct; parse `[[notify.hooks]]` from TOML (including `threshold_multiple`, `gate_pending_ms`, `workflow` fields)
3. Implement `HookRunner` with shell and HTTP dispatch, glob matching, timeout
4. Wire `HookRunner::fire()` into `dispatch_notification()` (replaces steps 3–4)
5. Remove `notify-rust` integration and `show_desktop_notification()`
6. Remove Slack sender (`maybe_send_slack`, `send_slack_message`, etc.)
7. Remove Web Push infrastructure (`push.rs`, VAPID key generation, push routes)
8. Remove `[notifications.slack]` and `[web_push]` config structs (with deprecation warning first)
9. Add spike detection: after `persist_workflow_metrics()` in `lifecycle.rs`, compute rolling 30-day avg cost and P75 duration (min 5 runs); fire `workflow_run.cost_spike` / `workflow_run.duration_spike` if thresholds exceeded
10. Add gate timeout detection: in the TUI/web background poll, check `gate.waiting` runs older than `gate_pending_ms` threshold and fire `gate.pending_too_long` (deduplicated per run per threshold crossing)
11. Update web Settings page to show configured hooks + test-fire button
12. Add `docs/examples/hooks/` with Slack, Discord, ntfy, macOS examples; add `conductor notifications test <event>` CLI command

Steps 1–4 land as a single PR. Steps 5–8 are a follow-on cleanup PR. Steps 9–10 are a follow-on analytics PR. Steps 11–12 are independent.

---

## Out of Scope

- Inbound webhooks / remote control (the Slack slash command handler)
- Hook retries or backoff (user's script responsibility)
- A hook editor UI in conductor-web
- Any new notification channels — that's the point
