# RFC 014: Resource Queue

**Status:** Draft
**Date:** 2026-04-10
**Author:** Devin

---

## Problem

Conductor workflows execute steps concurrently across multiple runs with no coordination over shared local resources. When two `ticket-to-pr` workflows both reach their `implement` or `iterate-pr` steps simultaneously, they independently invoke `xcodebuild`, spin up iOS Simulator instances, or call a local LLM — all without awareness of each other. The result is resource saturation: load averages well above core count, memory pressure, and degraded or failed builds.

This is a local orchestration problem. Cloud runtimes (Anthropic, OpenAI) have their own rate limiting. The gap is *local compute resources* and *external API quotas* that Conductor has no mechanism to gate.

---

## Resource Taxonomy

Two distinct classes of resource need different queuing semantics:

### Local Compute (semaphore semantics)

Limit concurrency to N simultaneous holders. A slot is acquired before the work begins and released when it completes.

| Resource | Typical limit | Notes |
|---|---|---|
| `xcodebuild` | 1–2 | CPU + disk I/O saturates quickly |
| iOS Simulator | 1–2 | Each device instance is heavy; concurrent UI test runs multiply pressure |
| Local LLM | 1 | GPU / Neural Engine + RAM; only one model fits loaded at a time on most hardware |
| Dep install (`bun`/`cargo`/`npm`) | 2 | Worktree setup phase; network + disk + CPU |
| Test runner | 1–2 | Especially UI/E2E test suites |

### External Quotas (token bucket / rate-limit semantics)

Allow up to N requests per time window. Excess requests are delayed, not dropped.

| Resource | Default limit | Notes |
|---|---|---|
| `github-api` | 4,000 req/hr | Conservative buffer below the 5,000 authenticated ceiling |
| `anthropic-api` | Configurable | Varies by tier; relevant when many parallel agent steps run |
| `apple-notarization` | Configurable | Apple Developer Portal; can queue for minutes under load |

---

## Proposed Design

### 1. `[resources]` in `config.toml`

```toml
[resources.xcodebuild]
type = "semaphore"
max_concurrent = 1

[resources.ios-simulator]
type = "semaphore"
max_concurrent = 1

[resources.local-llm]
type = "semaphore"
max_concurrent = 1
model_affinity = true     # prefer an already-warm model; don't evict between steps

[resources.dep-install]
type = "semaphore"
max_concurrent = 2

[resources.github-api]
type = "rate-limit"
requests_per_hour = 4000

[resources.anthropic-api]
type = "rate-limit"
requests_per_minute = 60  # adjust per account tier
```

All fields are optional — omitting a resource from config means no gating is applied.

### 2. Runtime `requires` field (extends RFC 007)

`RuntimeConfig` in RFC 007 gains an optional `requires` list naming resources that must be acquired before the runtime spawns:

```toml
[runtimes.local-llama]
type = "api"
base_url = "http://localhost:11434/v1"
requires = ["local-llm"]

[runtimes.gemini]
type = "cli"
binary = "gemini"
args = ["-m", "{{model}}", "-p", "{{prompt}}"]
requires = []              # no local resource contention
```

The workflow engine acquires all declared resources before calling `runtime.spawn()` and releases them in a `finally`-equivalent after `runtime.poll()` returns.

### 3. Workflow step-level `requires` (for non-runtime steps)

Steps that invoke tools directly (rather than via a runtime) can declare resources too:

```yaml
# .conductor/workflows/ticket-to-pr.wf
steps:
  - name: implement
    call: implement
    requires: [xcodebuild, ios-simulator]

  - name: iterate-pr
    call: iterate-pr
    requires: [xcodebuild, ios-simulator]
```

This covers the build-heavy phases identified as the immediate pain point.

### 4. SQLite-backed queue (no new daemon)

Resource state lives in conductor's existing SQLite DB — consistent with the v1 "no daemon, no IPC" philosophy. Two new tables:

```sql
-- One row per configured resource
CREATE TABLE resources (
    id           TEXT PRIMARY KEY,   -- e.g. "xcodebuild"
    type         TEXT NOT NULL,       -- "semaphore" | "rate-limit"
    max_concurrent INTEGER,           -- semaphore only
    requests_per_window INTEGER,      -- rate-limit only
    window_seconds INTEGER            -- rate-limit only
);

-- One row per active or queued lease
CREATE TABLE resource_leases (
    id           TEXT PRIMARY KEY,    -- ULID
    resource_id  TEXT NOT NULL REFERENCES resources(id),
    run_id       TEXT,                -- workflow run holding the lease (nullable for external holders)
    step_name    TEXT,
    holder_label TEXT,                -- human-readable: "feat-323 / implement"
    status       TEXT NOT NULL,       -- "waiting" | "active" | "released"
    acquired_at  TEXT,                -- ISO 8601, set when status → "active"
    released_at  TEXT,                -- ISO 8601, set when status → "released"
    expires_at   TEXT,                -- ISO 8601, TTL for crash recovery (semaphore only)
    created_at   TEXT NOT NULL
);
```

**Acquisition protocol** (SQLite WAL + advisory lock):

1. Insert a `waiting` lease row
2. In a transaction: count active leases for the resource; if below `max_concurrent`, update own row to `active`
3. If still `waiting`, poll every N seconds until a slot opens (or timeout)
4. On release (or crash recovery), update status to `released` and set `released_at`

**Crash / orphan recovery**: Any `active` lease past its `expires_at` is automatically released by the next process that checks in. TTL defaults to `max(step_timeout, 30m)`.

### 5. MCP tools

The conductor MCP exposes the queue as first-class tools, making it callable from any workflow step, external agent, or shell script:

```
conductor_acquire_resource(resource_id, holder_label, timeout_s?) → lease_id
conductor_release_resource(lease_id)
conductor_queue_status() → [{resource_id, active: [{holder_label, acquired_at}], waiting: [{holder_label, created_at}]}]
```

`conductor_acquire_resource` blocks (polls) until the slot is available or `timeout_s` elapses. This makes it safe to call from a workflow step without special handling — the step just waits.

### 6. Shell wrapper for non-MCP callers

For tools that can't call MCP directly (Xcode build phases, CI scripts, terminal `xcodebuild` invocations):

```bash
conductor resource acquire xcodebuild --label "manual build" && \
  xcodebuild -scheme MyApp -destination ... && \
  conductor resource release
```

The CLI stores the active lease ID in a temp file (`/tmp/conductor-lease-<pid>`) and the release command reads it. A `SIGTERM`/`EXIT` trap releases automatically.

### 7. TUI queue status panel

The worktree detail view gains a resource queue indicator: when a step is `waiting` on a resource, the status line shows `waiting for xcodebuild slot (1 ahead)` rather than a generic spinner. The resource panel in the run detail view lists active and queued holders.

---

## Local LLM: model affinity

When `model_affinity = true` on a semaphore resource, the queue additionally tracks which model is currently loaded. Before acquiring a slot:

1. If the requested model matches the currently-loaded model → acquire immediately (slot count permitting)
2. If a different model is loaded but idle → wait for an affinity-aware timeout before evicting
3. If no model is loaded → acquire normally

This avoids the expensive load/unload cycle when multiple sequential steps use the same local model. The `RuntimeRequest` (from RFC 007) already carries `model: Option<String>` — the resource manager reads it directly.

---

## Integration with RFC 007

RFC 007 defines the `AgentRuntime` trait and `RuntimeConfig`. This RFC extends `RuntimeConfig` with one new field:

```rust
pub struct RuntimeConfig {
    // ... existing RFC 007 fields ...

    /// Resources to acquire before spawning this runtime.
    /// Names must match keys in Config::resources.
    pub requires: Vec<String>,
}
```

The workflow executor acquires all declared resources before calling `runtime.spawn()`:

```rust
let runtime = resolve_runtime(&agent_def.runtime, &state.config)?;
let leases = resource_manager.acquire_all(&runtime_config.requires, &run_id, &step_name)?;
runtime.spawn(&request, &child_window)?;
let completed = runtime.poll(conn, &child_run.id, ...)?;
resource_manager.release_all(leases)?;
```

---

## Decisions

1. **Baked into conductor, not a separate product.** Conductor already knows which workflow step is running, which runtime, which repo, and which worktree — context that a standalone queue daemon would lack entirely. The MCP surface provides the external composability needed without a separate product. A shell wrapper covers non-MCP callers.

2. **SQLite-backed, no daemon.** Consistent with conductor's v1 architecture. SQLite WAL mode handles concurrent writers from multiple workflow processes. No new process to install or keep alive.

3. **Two resource classes with distinct semantics.** Semaphores for local compute (concurrency limit), token buckets for external quotas (rate limit with backoff). They share the same config surface but different queue implementations.

4. **TTL-based crash recovery.** Active leases expire automatically. No separate reaper process needed — the next process that checks discovers and releases stale leases.

5. **Model affinity is opt-in.** `model_affinity = true` on a semaphore resource enables warm-model preference for local LLM workflows. Off by default to keep the common case simple.

6. **Step-level `requires` in workflow DSL.** Lets existing workflows be retrofitted with resource gating without changing agent frontmatter or runtime config.

---

## Open Questions

1. **Backpressure vs. timeout:** Should `conductor_acquire_resource` block indefinitely (caller sets timeout) or should conductor enforce a global max-wait per resource? The former is simpler; the latter prevents workflows from hanging forever if a lease is never released.

2. **Priority:** Should concurrent workflows be able to declare priority (e.g., a hotfix run jumps the build queue)? Deferred — FIFO is correct for the common case.

3. **Distributed / multi-machine:** If conductor manages repos across multiple machines, the SQLite queue is local to each. Cross-machine resource sharing (e.g., a shared build farm) is out of scope for v1 but worth noting as a future extraction point.

4. **Rate-limit implementation detail:** Token bucket vs. sliding window counter. Sliding window is more accurate but requires more bookkeeping in SQLite. Start with a fixed-window counter (count leases in the current hour); revisit if bursty behavior causes problems.

5. **Observability:** Should `conductor_queue_status` expose historical wait times (p50/p95) so users can tune `max_concurrent`? Useful but adds schema complexity. Deferred.

---

## Relationship to Other RFCs

- **RFC 007** (multi-runtime agents) — direct dependency. `RuntimeConfig.requires` is the integration point. RFC 007 should be implemented first; this RFC's runtime integration depends on the `AgentRuntime` trait being in place.
- **RFC 012** (external control API) — `conductor_queue_status` could be exposed via the external REST API as a read-only endpoint for monitoring dashboards.

---

## Implementation Order

1. DB migrations — `resources` and `resource_leases` tables
2. `ResourceManager` in `conductor-core` — semaphore acquire/release/poll, TTL expiry
3. Rate-limit variant of `ResourceManager`
4. MCP tools — `conductor_acquire_resource`, `conductor_release_resource`, `conductor_queue_status`
5. Wire `requires` into workflow executor (step-level gating)
6. Extend `RuntimeConfig` with `requires` (RFC 007 integration)
7. CLI surface — `conductor resource acquire/release/status`
8. TUI queue status indicator in worktree detail view
9. Model affinity for local LLM semaphore (deferred until local LLM runtime is in use)
