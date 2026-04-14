# Ideas Parking Lot

Half-formed ideas that aren't ready for a ticket or RFC. Promote to a GitHub issue when the design questions are resolved.

---

## Tool version / update warnings

Surface a warning when required external tools (`gh`, `git`, `tmux`, package managers) are missing or outdated.

**Open questions:**
- When to check? Startup adds latency (subprocess calls). Background check with a dismissable footer warning may be better.
- Proactive (define minimum versions, check on startup) vs reactive (catch version-specific failures at the call site with a helpful hint like "consider upgrading `gh`")?
- Proactive version pinning for all tools could become maintenance whack-a-mole — reactive with better error messages may be the right default.
- How to surface it? Dismissable footer message, not a hard block.

---

## CI status in TUI

Show PR check status (passing/failing/pending) without leaving the TUI.

**Open questions:**
- Where first? Options in priority order: (1) dashboard worktree list as a small indicator, (2) worktree detail view, (3) persistent workflow column from #662.
- Data source: `gh pr view --json statusCheckRollup` for list/summary view; `gh pr checks` for per-check breakdown in detail view.
- Fetch strategy: on-demand (manual refresh key) first, background polling later. CI status is volatile (checks run for minutes) so auto-refresh adds meaningful load.
- Only relevant for worktrees with an open PR.

---

## Pre-warmed agent pool (eliminate cold-start latency)

Every workflow step today spawns a fresh `claude` process via `spawn_headless()`: a new subprocess starts, Claude CLI connects to the API, CLAUDE.md loads, context is established — then the step actually runs. For short steps (plan, review, validate), API handshake and context-load time can rival or exceed execution time. Multi-step workflows pay this tax on every step.

**The idea:** maintain a pool of `conductor agent run` subprocesses that are already running and holding an open stdin pipe, displaying `"Agent running — waiting for events…"` in their stdout stream. When a workflow step fires, conductor claims a pooled process and writes the job to its stdin instead of spawning a new subprocess cold. The agent executes, writes structured output to stdout (in the existing stream-json format), then loops back to waiting. Cold start becomes a one-time cost paid at pool initialization.

**Why headless makes this cleaner than tmux would have:** Conductor already owns the stdin/stdout pipes of every subprocess it spawns (RFC 016). A pooled agent is just a subprocess where conductor holds the pipe open across multiple tasks instead of reading until EOF once. No tmux window coordination, no external process ownership, no shell-level multiplexing needed. The infrastructure is already built — pools extend it.

**Stdin as the natural inbox:** Since conductor owns the subprocess stdin pipe, the job delivery mechanism is obvious: write the job payload as a JSON line to stdin, read the result from stdout. No file-based inbox, no DB polling, no named pipes. Conductor writes → agent reads → agent writes result → conductor reads. The same `BufReader<ChildStdout>` loop used today in `drain_stream_json` extends naturally to a multi-task read cycle.

```
Normal agent (exits after one task):
  conductor → spawn_headless() → write prompt file → drain_stream_json (EOF = done)

Pooled agent (persists across tasks):
  conductor → spawn_headless() → write job to stdin → read result from stdout → loop
                  │                                                                │
                  └──────────── stdin pipe stays open ──────────────────────────┘
```

**Pool mechanics:**
- Pool size is configurable (e.g., `pool_size = 3` in `config.toml`); defaults to the highest `max_parallel` value across active workflows
- When a pooled agent is claimed, conductor immediately spawns a replacement to hold pool size steady
- Pool agents have a TTL (e.g., 30 min idle); expired agents receive SIGTERM and are replaced — prevents stale context or token-limit drift
- Each pool slot is tracked in the DB (new `agent_pool_slots` table or `idle` status on `agent_runs`) with its PID — the existing `kill -0 <pid>` liveness check from the orphan reaper works unchanged
- The existing SIGTERM cancellation path (`cancel_subprocess`) works unchanged for pool slot teardown

**Live streaming still works:** Pool agents stream events in the same stream-json format as normal agents. The `on_event` callback in `drain_stream_json` fires for each task, giving TUI and SSE clients live updates for pooled steps exactly as they get them today. The only difference is that the drain loop resets between tasks rather than exiting.

**Context pre-loading:** At pool creation time, the agent is seeded with repo-level context (CLAUDE.md, architecture docs). This amortizes context loading across all tasks that slot handles — a second tier of latency savings on top of API handshake reuse.

**Worktree scoping:** Start with generic pool agents that receive worktree path as part of the job payload. Worktree-scoped pools (one pool per active worktree, pre-loaded with worktree context) are a future optimization.

**Relationship to daemon (v2):** A pool is a lightweight approximation of the v2 daemon's persistent agent process management. It fits under v1's library-first architecture (pool manager runs in the TUI background thread or web startup hook). Design the `agent_pool_slots` schema to survive the daemon migration — the daemon will supervise the pool, not replace it.

**Relationship to RFC 014 (resource queue):** The pool + claim protocol is a bounded queue for agent capacity. If RFC 014 lands first, the pool can be modeled as a resource type within that system. If the pool lands first, RFC 014 should absorb it.

**Open questions:**
- **Multi-task drain loop design:** `drain_stream_json` currently exits on EOF. A pooled variant needs a protocol marker to distinguish "task complete, send next job" from "subprocess exiting." A `{"type":"ready"}` sentinel line from the agent after each task is the simplest protocol.
- **Claim atomicity:** two parallel workflow steps must not claim the same slot. DB `UPDATE agent_pool_slots SET status='claimed' WHERE status='idle' LIMIT 1 RETURNING id` is the right atomic operation.
- **Context staleness:** a pool agent seeded at startup may have stale CLAUDE.md by the time it handles a task. Re-seed on claim (inject updated context as part of the job payload) vs. TTL-based replacement — tradeoffs to resolve.
- **Default pool size:** `max_parallel` across active workflows is a good heuristic but needs validation under load.
- **TUI/web display:** pool slots could appear as a persistent "Pool" section in the dashboard — idle vs. active per slot. Meaningful once the pool is used in production workflows.
- **When to initialize:** TUI startup, first workflow trigger, or explicit `conductor pool start`? On-demand initialization avoids burning API connections when no workflows are queued.

---

## Containerized workflow execution (Docker / Kubernetes)

Run conductor workflows and agent steps in containers instead of local subprocesses. Enables conductor in CI/CD pipelines, teams using Docker/K8s, and full isolation without local tool dependencies.

**Two levels:**
1. **Agent runtime** — a `DockerRuntime` (or `KubernetesRuntime`) implementing the `AgentRuntime` trait from RFC 007. Individual agent steps run in containers with the repo mounted. No tmux needed.
2. **Workflow executor** — the entire workflow runs in a container. Conductor becomes an orchestrator that submits jobs rather than running them locally.

**Open questions:**
- Does the daemon (#9) need to land first? Probably yes — a local-only tool can't submit remote jobs.
- Image management: pre-built images with conductor + tools baked in, or dynamic images per-repo?
- Credential injection: how do containers get GitHub tokens, API keys? K8s secrets, Docker env vars, mounted credential files?
- State: containerized runs need a DB. Per-run ephemeral DB (seeded, like #1316)? Or a shared DB service?
- Builds on RFC 007 (multi-runtime agents) — Docker is just another runtime alongside claude, gemini, openai, script.
- This is v3+ territory. Solve local isolation with per-worktree DBs (#1316) first.

---
