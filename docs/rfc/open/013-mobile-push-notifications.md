# RFC 013: Mobile Push Notifications via Conductor Cloud Relay

**Status:** Draft
**Date:** 2026-04-08
**Author:** Devin

---

## Problem

RFC 011 (Notification Hooks) removed Web Push / VAPID from conductor-core and replaced all built-in channels with a fire-and-forget hook system. That design is the right call for desktop and server-side notifications, but it leaves mobile push unsolved.

The conductor iOS app needs real-time, actionable push notifications — particularly for gate approvals and agent feedback requests where the user is away from their machine. Three properties are required that hooks alone cannot satisfy:

1. **Delivery to a locked device.** iOS background tasks (`BGAppRefreshTask`) are unreliable and delayed (15–30 min). Only Apple Push Notification service (APNs) delivers to a locked device in real time.

2. **Tap-to-act deep links.** ntfy and other third-party apps receive the tap — not the conductor iOS app. There is no way to deep-link into conductor from a third-party notification.

3. **No always-on server required from the user.** Self-hosted conductor runs on a developer's laptop. A relay that the iOS app polls or connects to via WebSocket would require that laptop to be online and reachable — which defeats the purpose of a mobile notification.

---

## Proposed Design

### Core Idea

Conductor publishes a hosted relay service (`push.conductor.ai`) that holds the APNs certificate for the conductor iOS app. Self-hosted conductor instances send notification events to this relay via a simple authenticated HTTP call. The relay fans them out to all registered iOS devices for that instance.

```
[conductor-web on laptop]
        │
        │  POST /notifications/send
        │  Authorization: Bearer <instance-api-key>
        ▼
[push.conductor.ai]  ←── holds APNs cert for conductor iOS app
        │
        │  APNs HTTP/2
        ▼
[iOS device]  ←── conductor app receives notification, taps open gate
```

The relay is built on [push-it](https://github.com/LivelyVideo/push-it) — an existing Go service that already implements APNs/FCM delivery, multi-platform credential management, device token registration, and automatic token cleanup.

---

### Components

#### 1. push.conductor.ai (hosted relay)

A push-it deployment with:
- The conductor iOS app's APNs certificate configured
- Per-instance API key isolation (see Security section)
- Hosted on infrastructure Conductor controls

No changes to push-it's core delivery logic. The only additions needed are instance-scoped authentication and a provisioning API for new instances to self-register.

#### 2. conductor-web: push relay integration

When a `push_relay` config block is present, conductor-web fires push events to the relay in addition to running local hooks.

```toml
# ~/.conductor/config.toml
[push_relay]
url     = "https://push.conductor.ai"
api_key = "ck_live_..."
```

The relay call is made from the same `dispatch_notification()` path as hooks — fire-and-forget, failures logged as warnings.

#### 3. conductor iOS app: device token registration

On first launch (and on APNs token refresh), the iOS app registers its device token with the relay, associating it with the user's conductor instance ID.

```
POST https://push.conductor.ai/subscriptions
{
  "deviceToken":    "<apns-token>",
  "devicePlatform": "ios",
  "userId":         "<instance-id>",
  "platformId":     "conductor-ios"
}
```

`instance-id` is a stable identifier for the conductor installation (ULID, generated on first run, stored in `config.toml`). The iOS app learns the `instance-id` via QR code pairing or manual entry (see Pairing section).

#### 4. Deep links

Notifications include a `url` field with a custom scheme deep link:

```
conductor://runs/<run-id>
conductor://gates/<run-id>/<step-id>
conductor://feedback/<run-id>/<step-id>
```

The iOS app registers the `conductor://` URL scheme. Tapping the notification opens the app directly to the relevant run, gate, or feedback prompt.

The relay maps `CONDUCTOR_URL` from the event payload to the APNs `click_action` / custom data field.

---

### Security Model

#### Instance isolation

Each self-hosted conductor instance gets a unique API key scoped to its `instance-id`. A key can only send notifications to devices registered under that `instance-id`. This prevents one user's instance from being able to reach another's devices.

push-it's existing `userId` + `platformId` composite key naturally enforces this: `userId = instance-id`, `platformId = "conductor-ios"`. The relay validates that the Bearer token in `/notifications/send` matches the `userId` (instance-id) in the request body.

#### Instance registration

Self-hosted instances self-register via a provisioning endpoint:

```
POST https://push.conductor.ai/instances/register
{
  "instance_id": "<ulid>",
  "email":       "user@example.com"   // optional, for recovery
}

→ 201 Created
{
  "api_key": "ck_live_..."
}
```

The API key is shown once and stored in `config.toml` by the user (or written automatically during `conductor setup`). Rate-limited to prevent abuse.

#### Device token trust

The iOS app sends its APNs token directly to the relay. There is no additional proof that the app is "authorized" — this mirrors how ntfy, Gotify, and most push relay services work. The security property is that a device token is only useful to the instance whose `instance-id` it registered under, and an instance's API key is required to send to it.

---

### Pairing Flow

Connecting a phone to a self-hosted conductor instance:

1. User opens conductor-web Settings → "Add Mobile Device"
2. conductor-web generates a short-lived pairing token (UUID, 10-minute TTL) and displays a QR code encoding:
   ```json
   { "instance_id": "<ulid>", "relay_url": "https://push.conductor.ai", "pairing_token": "<uuid>" }
   ```
3. User scans QR code with conductor iOS app
4. iOS app exchanges the pairing token for the `instance-id` (conductor-web validates the token) and registers its APNs device token with the relay
5. conductor-web confirms pairing — device now appears in Settings under "Mobile Devices"

The pairing token is single-use and scoped to a short window, so no long-lived credentials are ever transferred via QR code.

---

### Notification Payload

The relay receives the standard conductor event payload (same as RFC 011 HTTP webhook hooks) and maps it to an APNs payload:

| conductor field | APNs field |
|---|---|
| `event` | `aps.alert.title` prefix ("Conductor — workflow_run.failed") |
| `label` | `aps.alert.body` |
| `url` (conductor:// deep link) | custom data `conductor_url` |
| `run_id` | custom data `run_id` |
| `gate_type`, `gate_prompt` | custom data for in-app rendering |

Priority mapping (same as the ntfy hook):

| Event | APNs priority | `aps.sound` |
|---|---|---|
| `*.failed` | `10` (immediate) | `default` |
| `gate.waiting`, `feedback.requested` | `10` (immediate) | `default` |
| `gate.pending_too_long`, `*.cost_spike` | `10` (immediate) | `default` |
| `*.completed` | `5` (power-efficient) | none |
| all others | `5` | none |

---

### conductor-web Integration

```rust
// conductor-core/src/push_relay.rs

pub struct PushRelayConfig {
    pub url: String,
    pub api_key: String,
}

pub fn fire_push_relay(config: &PushRelayConfig, event: &NotificationEvent) {
    let payload = event.to_payload();
    let url = config.url.clone();
    let api_key = config.api_key.clone();
    std::thread::spawn(move || {
        let result = send_to_relay(&url, &api_key, &payload);
        if let Err(e) = result {
            log::warn!("push relay delivery failed: {}", e);
        }
    });
}
```

Wired into `dispatch_notification()` after hooks:

```rust
// conductor-core/src/notify.rs

pub fn dispatch_notification(event: &NotificationEvent, config: &Config) {
    // 1. Claim dedup slot
    // 2. Persist in-app notification
    // 3. Fire hooks (RFC 011)
    hook_runner.fire(event);
    // 4. Fire push relay (RFC 013)
    if let Some(relay) = &config.push_relay {
        fire_push_relay(relay, event);
    }
}
```

---

### Config Schema

```toml
[push_relay]
url     = "https://push.conductor.ai"  # or self-hosted relay URL
api_key = "ck_live_..."                # provisioned during setup

# instance_id is auto-generated on first run; stored alongside api_key
instance_id = "01J..."
```

---

### Self-Hosting the Relay

Users who do not want to depend on `push.conductor.ai` can run their own push-it instance. They would need to:

1. Obtain their own APNs certificate (requires Apple Developer Program membership + their own iOS app bundle ID — not practical for most self-hosters)
2. Deploy push-it (Docker Compose, ~5 min)
3. Set `url` in `[push_relay]` to their instance

In practice, self-hosting the relay only makes sense for organizations building their own conductor-branded iOS app. For the common case, `push.conductor.ai` is the right default.

---

## What Gets Built

| Component | Work |
|---|---|
| push-it deployment as `push.conductor.ai` | DevOps: Docker + Postgres, APNs cert, domain |
| Instance registration endpoint on relay | Small addition to push-it |
| Per-instance API key scoping | Small addition to push-it auth middleware |
| `[push_relay]` config block in conductor-core | Config struct + parse |
| `fire_push_relay()` in notify.rs | ~50 lines |
| APNs device token registration in iOS app | iOS SDK call + relay POST |
| QR code pairing flow | conductor-web Settings page + iOS app |
| `conductor://` deep link handling | iOS app URL scheme registration |
| Deep link routing in iOS app | Per-event navigation |

---

## Decisions Made

1. **push-it as the relay foundation.** Already built, tested, and deployed for another project. APNs/FCM delivery, token cleanup, and multi-platform support are already solved. Adaptation cost is low (instance auth + provisioning endpoint).

2. **Hosted relay, not self-hosted-only.** Self-hosting APNs requires an Apple Developer account and a custom bundle ID — not a realistic ask for conductor users. A hosted relay with per-instance API key isolation gives self-hosters real mobile push without that burden.

3. **conductor:// deep links, not ntfy.** ntfy captures the tap and cannot deep-link into a different app. APNs delivery directly to the conductor iOS app is the only way to get tap-to-act behavior.

4. **QR code pairing, not manual token entry.** APNs tokens and instance IDs are not human-typeable. QR code is the standard pattern (2FA apps, HomeKit, etc.) and maps cleanly onto the short-lived pairing token model.

5. **Fire-and-forget, no retries in conductor.** Consistent with RFC 011 hooks. APNs itself handles delivery retries. If the relay is unreachable, the failure is logged and the next event will try again.

6. **Instance ID is stable and config-stored.** A ULID generated once on first run. Stored in `config.toml` alongside the relay API key. Survives restarts; does not change unless the user intentionally re-provisions.

---

## Open Questions

1. **FCM / Android.** push-it supports FCM today. If an Android conductor app is ever built, the relay can support it with no architectural changes — just add the FCM credential and a new `platformId`. Out of scope for this RFC.

2. **Notification history in the iOS app.** Should the relay buffer recent notifications for cases where the device was offline? push-it does not currently buffer; APNs stores one notification per device and delivers it when the device comes online. For gate approvals this is acceptable; for event history it is not. Deferred.

3. **Multiple conductor instances on one phone.** The pairing model supports it (each instance gets its own `instance-id` and device token registration), but the iOS app UI for switching between instances is not designed yet.

4. **Relay pricing / abuse prevention.** `push.conductor.ai` is a free hosted service. Rate limiting per instance-id (e.g. 1000 notifications/day) should be in place before public launch to prevent abuse.

5. **APNs certificate renewal.** APNs certificates expire annually. This is an operational concern for whoever runs `push.conductor.ai`, not for conductor users — but the process should be documented.

---

## Out of Scope

- Android push (FCM) — relay is ready for it, iOS app comes first
- Self-hosted relay setup guide for organizations with custom iOS apps
- In-app notification history / inbox on iOS (separate iOS feature)
- Inbound push actions (approving a gate directly from the notification action button) — desirable but deferred
