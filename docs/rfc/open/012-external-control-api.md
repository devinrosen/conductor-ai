# RFC 012: External HTTP Adapters (reframed from "External Control API")

**Status:** Stub (reframed 2026-04-16)
**Date:** 2026-04-07
**Reframed:** 2026-04-16
**Author:** Devin

---

> **Reframe note (2026-04-16).** The original framing asked for a new REST API for external systems to query and control conductor. Since then, [DIRECTION.md](../../DIRECTION.md) has positioned MCP as conductor's primary external API, and most of what the original RFC proposed is now served by existing MCP tools. This reframe narrows the scope to the genuinely unserved case: callers that can't speak MCP.

---

## Problem

Conductor can be operated via TUI, CLI, web UI, and MCP. MCP is the primary external API for agents and MCP-aware clients. The remaining gap is **callers that can't speak MCP**:

- **Slack slash commands** — must reply in Slack's response shape, verify Slack's HMAC signature
- **Generic webhooks** — GitHub, GitLab, custom internal systems that can only POST JSON
- **Shell-script CI / `curl` one-liners** — need something simpler than an MCP client

The current Slack handler (`conductor-web/src/routes/slack.rs`) is the only existing adapter of this kind, and it talks directly to the database. As we add more such adapters, they should all share a common backing surface rather than each reinventing DB access.

This is the inbound complement to RFC 011 (notification hooks). RFC 011 covers **Conductor → world** (outbound events). This RFC covers **non-MCP world → Conductor** (HTTP adapters over MCP).

---

## What MCP already covers

The originally-proposed REST endpoints all have MCP equivalents today:

| Originally proposed | Existing MCP tool |
|---|---|
| `GET /api/ext/runs` | `conductor_list_runs`, `conductor_list_agent_runs` |
| `GET /api/ext/runs/{id}` | `conductor_get_run` |
| `POST /api/ext/runs/{id}/approve` | `conductor_approve_gate` |
| `POST /api/ext/runs/{id}/cancel` | `conductor_cancel_run` |
| `POST /api/ext/workflows/trigger` | `conductor_run_workflow` |
| `GET /api/ext/status` | `conductor_list_repos` + `conductor_list_runs` composition |

Agents, Claude Code, and any MCP-aware client already use these. This RFC should not duplicate them as a parallel REST surface.

---

## What's genuinely missing

Only the HTTP adapter layer for non-MCP callers. Two concrete shapes, not mutually exclusive:

### 1. Purpose-built adapters (per integration)

Purpose-built adapters live in `conductor-web/src/routes/` and call MCP tools internally. Each adapter is fit-for-purpose: it speaks the caller's protocol (Slack slash, webhook signature scheme, CI-friendly shell response) and maps to one or more MCP calls.

```
POST /slack/slash/command        — existing Slack handler, refactored to call MCP
POST /webhook/{name}             — generic signed-webhook receiver with name-indexed dispatch
POST /ci/trigger/{workflow}      — simple token-auth wrapper for CI scripts
```

Existing Slack handler stays in place; it gets refactored to call MCP internally rather than the DB directly. Removes one source of direct DB access from conductor-web.

### 2. Generic MCP-over-HTTP proxy (optional, lower priority)

For callers that want MCP semantics but can only speak HTTP:

```
POST /api/mcp/{tool_name}
  Authorization: Bearer <token>
  Body: <MCP tool input JSON>
  → <MCP tool output JSON>
```

This is cheaper to build than many purpose-built adapters but has worse ergonomics for specific integrations. Build only on demand.

---

## Key Design Questions

1. **Auth model.** API tokens generated via `conductor token create` and stored in SQLite. Scoped (read-only vs. write) is likely overkill for v1 — a single capability bit is probably enough. Tokens must work in headless/CI contexts (no browser session).

2. **Token storage and revocation.** New `api_tokens` table (id, token_hash, label, created_at, last_used_at, revoked_at). Tokens are opaque to the caller; conductor stores only the hash.

3. **Rate limiting.** Needed if exposed publicly; out of scope if local-only. Default posture: local-only for v1, documented clearly. Users who want public exposure ship a reverse proxy in front.

4. **Webhook signature schemes.** Each purpose-built adapter handles its own signature scheme (Slack HMAC, GitHub HMAC, etc.). No generic verification layer in v1.

5. **Relationship to conductor-web routes.** These adapters live alongside existing web UI routes but use the token auth path, not the browser session path. Clear naming convention (`/api/ext/...` vs. `/api/...`) avoids accidental coupling.

---

## Non-Goals

- **A parallel REST API that duplicates MCP tools.** Use MCP if you can speak MCP.
- **Hosted / cloud relay.** All adapters run inside the user's local conductor-web process. RFC 013 (mobile push) is the only case where conductor-operated cloud infrastructure is on the table.
- **Scoped permission model beyond read/write.** Out of scope for v1.

---

## Relationship to Other RFCs

- **[DIRECTION.md](../../DIRECTION.md)** — establishes MCP as the primary external API; this RFC narrows accordingly.
- **RFC 011** (notification hooks) — outbound counterpart. Both RFCs should feel symmetric: 011 is "conductor calls your HTTP endpoint"; 012 is "your HTTP caller triggers conductor via MCP."
- **RFC 017** (external plugin protocol) — orthogonal. 017 is about subprocess contracts for ticket sources and lifecycle hooks. This RFC is about HTTP reach into MCP.

---

## Status

Stub. Blocked on:

- A concrete second adapter use case (beyond the existing Slack handler) to validate the shape
- Token auth and storage design (small but needed before any adapter can be added generically)

Not blocked on the generic MCP-over-HTTP proxy — that's build-on-demand.
