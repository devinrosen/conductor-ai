# RFC 012: External Control API

**Status:** Stub
**Date:** 2026-04-07
**Author:** Devin

---

## Problem

Conductor can be operated via TUI, CLI, and web UI, but there is no stable, authenticated API for external systems to query or control it. The current Slack slash command handler (`/conductor active`) is the only inbound integration, and it is Slack-specific — it speaks Slack's slash command format, verifies Slack's HMAC signature, and returns Slack's response shape. It cannot be reused by any other caller.

This is the inbound complement to RFC 011 (notification hooks). RFC 011 covers **Conductor → world** (outbound events). This RFC covers **world → Conductor** (remote query and control).

---

## Use Cases to Clarify Before Designing

The right API shape depends heavily on which of these are real priorities:

- **"I want to approve a gate from Slack without opening the browser"** — implies gate approval endpoint + Slack adapter
- **"I want to see active runs from a terminal one-liner"** — implies a simple read-only query endpoint
- **"I want to trigger workflows from a CI pipeline"** — implies workflow trigger endpoint + auth that works in CI
- **"I want a Discord bot that mirrors what the Slack handler does"** — implies the Slack handler is just one adapter over a generic API

Capturing concrete use cases here before designing the API is a prerequisite.

---

## Likely Shape

A stable external API with token-based auth, distinct from the browser-session-based web UI routes:

```
GET  /api/ext/runs              — list active and recent runs
GET  /api/ext/runs/{id}         — get run detail
POST /api/ext/runs/{id}/approve — approve a waiting gate
POST /api/ext/runs/{id}/cancel  — cancel a run
POST /api/ext/workflows/trigger — start a workflow run
GET  /api/ext/status            — conductor health + summary
```

The existing Slack slash command handler would be refactored as a thin adapter on top of these endpoints, rather than having its own direct DB access. Any other integration (Discord bot, CI pipeline, `curl` script) would use the same endpoints.

---

## Key Design Questions

1. **Auth model:** API tokens (generated via `conductor token create`) vs. shared secret in config vs. something else? Tokens need to work in headless/CI contexts.

2. **Token management:** Where are tokens stored (SQLite)? How are they scoped (read-only vs. write)? How are they revoked?

3. **Slack handler migration:** Does the existing `conductor-web/src/routes/slack.rs` stay in place as an adapter, or does it get removed in favor of users setting up their own Slack app that calls the external API? The former is simpler; the latter is more consistent with the RFC 011 philosophy.

4. **Rate limiting:** Needed if the API is exposed publicly. Out of scope if it's local-only.

5. **Scope:** Is this local-only (assumes caller has network access to wherever conductor-web is running) or does it imply a hosted/cloud relay? Almost certainly local-only for v1.

---

## Relationship to Other RFCs

- **RFC 011** (notification hooks) — the outbound counterpart. RFC 011 should be implemented first; this RFC is blocked on clarifying use cases.
- **RFC 007** (multi-runtime agents) — unrelated.

---

## Status

Stub. Needs use case validation before design work begins.
