# Notification Hooks — Architecture Reference

> Source files: `conductor-core/src/notification_event.rs`, `conductor-core/src/notification_hooks.rs`, `conductor-core/src/notify.rs`

This document covers the full conductor notification system: every event type and what fires it, which binaries participate, the dispatch pipeline from event construction to hook execution, filter resolution logic, and the two DB tables involved.

---

## Diagram 1 — Event type taxonomy

All 11 `NotificationEvent` enum variants, grouped by domain. Three variants are defined in the enum but are not yet wired to any construction site (no callers outside tests). `workflow_run.stale` is a 12th event name that exists in `ALL_EVENTS` but has **no** corresponding enum variant — it fires through a legacy path that writes only to the in-app notification log and never invokes hooks.

```mermaid
flowchart TD
    subgraph WF["workflow_run domain"]
        WRC["workflow_run.completed\nWorkflowRunCompleted"]
        WRF["workflow_run.failed\nWorkflowRunFailed"]
        WRO["workflow_run.orphan_resumed\nWorkflowRunOrphanResumed"]
        WRR["workflow_run.reaped\nWorkflowRunReaped"]
        WCS["workflow_run.cost_spike\nWorkflowRunCostSpike\n⚠ not yet wired"]
        WDS["workflow_run.duration_spike\nWorkflowRunDurationSpike\n⚠ not yet wired"]
        WRS["workflow_run.stale\n⚠ legacy path — no enum variant,\nno hook execution"]
    end

    subgraph AG["agent_run domain"]
        ARC["agent_run.completed\nAgentRunCompleted"]
        ARF["agent_run.failed\nAgentRunFailed"]
    end

    subgraph GT["gate domain"]
        GW["gate.waiting\nGateWaiting"]
        GPT["gate.pending_too_long\nGatePendingTooLong\n⚠ not yet wired"]
    end

    subgraph FB["feedback domain"]
        FR["feedback.requested\nFeedbackRequested"]
    end
```

**Not yet wired** means the variant is defined in the enum but no code constructs it outside of test factories. These are reserved for follow-on analytics and monitoring PRs.

**Legacy stale path** (`fire_stale_workflow_notification`): calls `dispatch_notification` with `event: None` and `hooks: &[]`, so it writes an in-app `notifications` row and a `notification_log` dedup row but never reaches `HookRunner::fire`.

---

## Diagram 2 — Binary participation

Which binaries fire which notification functions, all of which ultimately call `dispatch_notification()` in `conductor-core`.

```mermaid
graph LR
    TUI["conductor-tui\n(background poll thread)"]
    WEB["conductor-web\n(tokio background task)"]
    CLI["conductor-cli\n(management only)"]

    FWN["fire_workflow_notification()"]
    FAN["fire_agent_run_notification()"]
    FFN["fire_feedback_notification()"]
    FGN["fire_gate_notification() /\nfire_grouped_gate_notification()"]
    FGPN["fire_gate_pending_too_long_notification()"]
    FCN["fire_cost_spike_notification()"]
    FDN["fire_duration_spike_notification()"]
    FORN["fire_orphan_resumed_notification()"]
    FSN["fire_stale_workflow_notification()\n(legacy — no hooks)"]

    TUI --> FWN
    TUI --> FAN
    TUI --> FFN
    TUI --> FGN
    TUI --> FGPN
    TUI --> FCN
    TUI --> FDN
    TUI --> FORN
    TUI --> FSN

    WEB --> FWN
    WEB --> FAN
    WEB --> FGPN
    WEB --> FCN
    WEB --> FDN
    WEB --> FORN

    CLI -->|"conductor notifications\nlist / test / mark-read"| MGMT["management commands\n(no lifecycle events fired)"]
```

All fire functions are defined in `conductor-core/src/notify.rs` and re-exported from `conductor-tui/src/notify.rs` and `conductor-web/src/notify.rs` as thin re-export modules.

---

## Diagram 3 — Full dispatch pipeline

The sequence from event construction through dedup, in-app persistence, and hook execution.

```mermaid
flowchart TD
    A["Caller constructs NotificationEvent\n(e.g. WorkflowRunCompleted { run_id, label, … })"]
    B["fire_*_notification(conn, config, hooks, params)\nconductor-core/src/notify.rs"]
    C{"should_notify(config, succeeded)?\n(legacy on_success/on_failure guard)\nAlways true when no [notifications.workflows] block"}
    D["try_claim_notification(conn, entity_id, event_type)\nINSERT OR IGNORE INTO notification_log\n(entity_id, event_type, fired_at)"]
    E{"rows_inserted == 1?"}
    F["return false — already claimed\n(cross-process dedup)"]
    G["persist_notification(conn, CreateNotification)\nINSERT INTO notifications\n(id, kind, title, body, severity,\nentity_id, entity_type, read, created_at)"]
    H{"event param is Some?"}
    I["HookRunner::new(hooks).fire(event)\nIterate configured [[notify.hooks]] entries"]
    J["return true — dispatched"]

    A --> B
    B --> C
    C -->|"false"| SKIP["return — no-op"]
    C -->|"true"| D
    D --> E
    E -->|"0 rows (already claimed)"| F
    E -->|"1 row"| G
    G --> H
    H -->|"None (legacy stale path)"| J
    H -->|"Some(event)"| I
    I --> J
```

### HookRunner::fire — per-hook thread spawn

```mermaid
flowchart TD
    I["HookRunner::fire(event)"]
    J["for hook in configured [[notify.hooks]]"]
    K["on_pattern_match(hook.on, event_name)"]
    L{"OnMatch result"}
    M["skip this hook"]
    N{"root_only AND\nevent is sub-workflow?"}
    O["skip this hook"]
    P["hook_event_passes_filters(hook, event)"]
    Q{"passes?"}
    R["skip this hook"]
    S["std::thread::spawn — fire-and-forget OS thread"]
    T{"hook.run.is_some()?"}
    U["run_shell_hook(hook, event)\nsh -c <cmd> with CONDUCTOR_* env vars\npoll every 50ms; kill on timeout_ms (default 10 000)"]
    V{"hook.url.is_some()?"}
    W["run_http_hook(hook, event)\nureq POST <url> with JSON body\n$VAR headers resolved from env\ntimeout_ms (default 10 000)"]

    I --> J
    J --> K
    K --> L
    L -->|"None"| M
    L -->|"Any or RootOnly"| N
    N -->|"yes"| O
    N -->|"no"| P
    P --> Q
    Q -->|"false"| R
    Q -->|"true"| S
    S --> T
    T -->|"yes"| U
    U --> V
    T -->|"no"| V
    V -->|"yes"| W
```

All failures in `run_shell_hook` and `run_http_hook` are logged as `tracing::warn!` and never propagated — hooks are best-effort.

---

## Diagram 4 — Hook filter resolution

Each optional filter field on a `HookConfig` acts as an independent gate. Non-applicable filters auto-pass (e.g. `workflow` filter is ignored for agent/gate/feedback events).

```mermaid
flowchart TD
    A["hook_event_passes_filters(hook, event)"]

    B{"hook.threshold_multiple set?\n(cost_spike / duration_spike events only)"}
    C{"event.multiple >= threshold?"}
    FAIL1["return false — filtered"]

    D{"hook.gate_pending_ms set?\n(gate.pending_too_long only)"}
    E{"event.pending_ms >= gate_pending_ms?"}
    FAIL2["return false — filtered"]

    F{"hook.workflow set?\n(workflow_run.* events only)"}
    G{"event.workflow_name == hook.workflow?"}
    FAIL3["return false — filtered"]

    H{"hook.root_workflows_only = true?\n(workflow_run.* events only)"}
    I{"parent_workflow_run_id.is_none()?"}
    FAIL4["return false — filtered"]

    J{"hook.repo set?"}
    K{"event.repo_slug == hook.repo?"}
    FAIL5["return false — filtered"]

    L{"hook.branch set?"}
    M{"glob_matches(branch_filter, event.branch)?"}
    FAIL6["return false — filtered"]

    N{"hook.step set?\n(gate.waiting / gate.pending_too_long only)"}
    O{"event.step_name == hook.step?"}
    FAIL7["return false — filtered"]

    PASS["return true — hook fires"]

    A --> B
    B -->|"yes"| C
    B -->|"no"| D
    C -->|"no"| FAIL1
    C -->|"yes"| D

    D -->|"yes"| E
    D -->|"no"| F
    E -->|"no"| FAIL2
    E -->|"yes"| F

    F -->|"yes (workflow event)"| G
    F -->|"no or non-workflow event"| H
    G -->|"no"| FAIL3
    G -->|"yes"| H

    H -->|"yes"| I
    H -->|"no"| J
    I -->|"no (is sub-workflow)"| FAIL4
    I -->|"yes (is root)"| J

    J -->|"yes"| K
    J -->|"no"| L
    K -->|"no"| FAIL5
    K -->|"yes"| L

    L -->|"yes"| M
    L -->|"no"| N
    M -->|"no"| FAIL6
    M -->|"yes"| N

    N -->|"yes (gate event)"| O
    N -->|"no or non-gate event"| PASS
    O -->|"no"| FAIL7
    O -->|"yes"| PASS
```

### `on` pattern matching

The `on` field accepts a comma-separated list of patterns. Each sub-pattern may carry a `:root` suffix:

| Pattern | Matches |
|---|---|
| `*` | All events |
| `workflow_run.*` | Any `workflow_run.` event |
| `agent_run.*` | Any `agent_run.` event |
| `gate.*` | Any `gate.` event |
| `gate.waiting` | Exact event name |
| `workflow_run.completed:root` | Only root workflow completions (no parent) |
| `workflow_run.*:root` | All workflow events, root runs only |
| `feature/*` | Branch glob (used in `branch` filter, not `on`) |

The `:root` suffix triggers `OnMatch::RootOnly`; the `root_workflows_only` filter field is a separate orthogonal mechanism that checks `parent_workflow_run_id.is_none()`.

---

## Diagram 5 — DB table write paths

```mermaid
erDiagram
    notification_log {
        TEXT entity_id PK "run_id or dedup key"
        TEXT event_type PK "e.g. workflow_run.completed"
        TEXT fired_at "ISO 8601 — when claim was won"
    }

    notifications {
        TEXT id PK "ULID"
        TEXT kind "e.g. workflow_run_completed"
        TEXT title "display title"
        TEXT body "display body"
        TEXT severity "info | warning | action_required"
        TEXT entity_id "optional run_id"
        TEXT entity_type "optional: workflow_run | agent_run"
        INTEGER read "0 = unread, 1 = read"
        TEXT created_at "ISO 8601"
        TEXT read_at "ISO 8601 or NULL"
    }
```

**Write sequence:**

1. `notification_log` — written first via `INSERT OR IGNORE` (dedup claim). Composite PK `(entity_id, event_type)` is the cross-process lock. If a row already exists for `(run_id, "workflow_run.completed")`, the second caller's `INSERT OR IGNORE` returns 0 rows and the whole dispatch is aborted.
2. `notifications` — written only after a successful claim. Stores the in-app bell/feed entry. Never deduplicated by the application (the `notification_log` claim guarantees at-most-one).

**Indexes** (migration `046_notifications.sql`):
- `idx_notifications_read` on `notifications(read)` — fast unread-count query
- `idx_notifications_created_at` on `notifications(created_at)` — feed ordering

**Severity values** used in practice:
- `info` — completed, orphan_resumed
- `warning` — failed, reaped, stale, gate.waiting, feedback.requested
- `action_required` — (reserved; not currently used)

---

## Shell hook environment variables

All `CONDUCTOR_*` variables injected into shell hook commands via `NotificationEvent::to_env_vars()`.

### Common fields (all events)

| Variable | Value | Notes |
|---|---|---|
| `CONDUCTOR_EVENT` | `workflow_run.completed` etc. | Dotted event name |
| `CONDUCTOR_RUN_ID` | ULID string | Workflow or agent run ID |
| `CONDUCTOR_LABEL` | `"my-wf on repo/branch"` | Human-readable display label |
| `CONDUCTOR_TIMESTAMP` | ISO 8601 | When the event fired |
| `CONDUCTOR_URL` | Deep link URL | Empty string when not available (non-web contexts) |
| `CONDUCTOR_REPO_SLUG` | `"conductor-ai"` | Repository slug |
| `CONDUCTOR_BRANCH` | `"main"` | Branch name |
| `CONDUCTOR_DURATION_MS` | `"12345"` | Run duration; empty string when `None` |
| `CONDUCTOR_TICKET_URL` | Issue/ticket URL | Empty string when `None` |

### Workflow events (`workflow_run.*`)

| Variable | Value | Events |
|---|---|---|
| `CONDUCTOR_WORKFLOW_NAME` | `"ticket-to-pr"` | All `workflow_run.*` |
| `CONDUCTOR_PARENT_WORKFLOW_RUN_ID` | parent run ULID | Empty string for root runs |

### Spike events

| Variable | Value | Events |
|---|---|---|
| `CONDUCTOR_MULTIPLE` | `"3.5"` | `workflow_run.cost_spike`, `workflow_run.duration_spike` |
| `CONDUCTOR_COST_USD` | `"0.42"` | `workflow_run.cost_spike` only; absent when `None` |

### Error events

| Variable | Value | Events |
|---|---|---|
| `CONDUCTOR_ERROR` | Error message | `workflow_run.failed`, `agent_run.failed`, `workflow_run.reaped`; empty string when `None` |

### Gate events

| Variable | Value | Events |
|---|---|---|
| `CONDUCTOR_STEP_NAME` | `"human-review"` | `gate.waiting`, `gate.pending_too_long` |
| `CONDUCTOR_PENDING_MS` | `"90000"` | `gate.pending_too_long` only |

### Feedback events

| Variable | Value | Events |
|---|---|---|
| `CONDUCTOR_PROMPT_PREVIEW` | First ~100 chars of prompt | `feedback.requested` |

---

## HTTP hook payload

HTTP hooks receive `NotificationEvent::to_json()` as a JSON POST body. The shape mirrors the env var table above: common fields are always present; `url`, `duration_ms`, `ticket_url`, and `parent_workflow_run_id` are omitted when `None`. Header values starting with `$` are resolved from the process environment (e.g. `Authorization: $SLACK_TOKEN`).

Example for `workflow_run.completed`:

```json
{
  "event": "workflow_run.completed",
  "run_id": "01ABCDEF...",
  "label": "ticket-to-pr on conductor-ai/feat-123",
  "timestamp": "2025-04-16T14:00:00Z",
  "url": "https://conductor.example.com/repos/.../runs/...",
  "repo_slug": "conductor-ai",
  "branch": "feat/123",
  "duration_ms": 42000,
  "workflow_name": "ticket-to-pr",
  "ticket_url": "https://github.com/org/repo/issues/123"
}
```

See `docs/examples/hooks/` for working shell and HTTP hook examples.
