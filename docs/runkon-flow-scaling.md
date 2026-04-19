# runkon-flow: Scaling Beyond Single-Host Deployment

**Status:** Reference  
**Date:** 2026-04-19  
**Related:** [workflow-engine-platform-spec.md](./workflow-engine-platform-spec.md) Open Question #2

---

## Purpose

`runkon-flow` v1 is a synchronous, in-process workflow engine (Open Q #2 resolved
to sync-only). That's the right starting point for conductor and the comm-harness,
but SaaS deployments with thousands of concurrent workflow runs will eventually
outgrow it.

This doc captures the three escape hatches, in increasing order of friction,
so the decision is grounded when the scaling problem actually arrives.

---

## The Actual Bottleneck

The naive "sync doesn't scale to SaaS" framing is wrong. What actually limits
sync `runkon-flow` at scale isn't CPU — it's **OS threads holding state during
waits**.

A workflow run spends 99% of its wall-clock time waiting:

- Waiting for an agent subprocess (minutes to hours)
- Waiting for a gate approval (hours to days)
- Waiting for an external API response

In the v1 design, each active workflow run occupies one OS thread. At 1,000
concurrent runs that's ~500MB–1GB of thread stacks, plus kernel scheduler
pressure, plus `ulimit -u` headaches. At 10,000 concurrent runs it's genuinely
problematic.

The escape hatches all attack this bottleneck — they just attack it at
different layers.

---

## Escape 1 — Horizontal scaling (no code changes)

Run N worker processes, each handling M concurrent workflows via the sync
engine as designed. A dispatcher (queue, leader election, or simple hash-based
assignment) routes runs to workers. Your infra team adds k8s replicas as load
grows.

**Friction:** Zero `runkon-flow` changes. Pure infra work — queue, worker
registry, health checks, assignment logic.

**Scales to:** Roughly 1k–10k concurrent runs across a fleet of 20–50 workers.

**When it's enough:** Most "SaaS but not at Temporal/Airflow scale" deployments.
A fair amount of real production software runs like this indefinitely.

**Caveat:** You're paying for OS threads across the fleet. Memory cost adds up,
but horizontal scaling is well-understood operationally (add replicas, done).

**When to move on:** When the memory/ops cost of the fleet exceeds the engineering
cost of Escape 3.

---

## Escape 2 — Full async migration

Convert `runkon-flow` to async. All six traits gain `async fn`, engine internals
use tokio, parallel blocks use `tokio::spawn`, gate polling uses
`tokio::time::sleep`.

**Friction:** Real but bounded. Rough budget:

| Item | Estimate |
|---|---|
| Trait method migration (6 traits) | 1–2 weeks |
| Engine internals (execute_nodes, parallel, gate polling) | 1–2 weeks |
| Conductor-core callers (CLI/TUI/web rewire) | 2–3 weeks |
| Test migration | 1 week |
| **Total** | **4–6 weeks** |

This is a semver-major break. External harnesses migrate too. Do it as
`runkon-flow 0.2.0` with a deprecation cycle; sync and async can co-exist as
feature flags for a release to smooth the transition.

**Scales to:** 10k+ concurrent runs per process. Async tasks are KBs of memory
vs. ~512KB per OS thread stack.

**When it's tempting:** When you want many concurrent runs in *one* process
rather than operating a fleet.

**Why it's usually not the right answer:** Async doesn't actually solve the
waiting-workflow problem — it just makes each waiting workflow cheaper. At
true scale (100k+ concurrent), you still want Escape 3, at which point the
async rewrite was extra work that doesn't cleanly layer.

**When Escape 2 is actually right:** You need full async for ecosystem
integration reasons (not scale), and `spawn_blocking` can't handle it cleanly.
This is rare.

---

## Escape 3 — Continuation-based execution

The pattern Temporal, AWS Step Functions, Airflow (scheduler), and most
serious workflow infrastructure uses.

**The shift:**

- Workflow state lives entirely in the DB (already true — resumability is a
  selling point of `runkon-flow`).
- A workflow **does not hold a thread during waits**. Between steps, the
  thread exits.
- A worker pool polls the DB (or listens to event notifications — Postgres
  `LISTEN/NOTIFY`, Redis pub/sub, SQS, etc.) for "ready" steps.
- When an event fires (agent subprocess completed, gate approved, timer
  elapsed), a worker picks up the workflow, executes one step, writes state
  back, exits.
- At 10,000 concurrent workflows, you might have ~50 actual threads running at
  any moment — because 9,950 workflows are parked waiting for events.

**What changes:**

- Every blocking loop (gate polling, parallel join, foreach fan-out) becomes
  event-driven.
- Resumability infrastructure extended beyond crash recovery to normal
  execution.
- DB schema gains event/readiness tables and event source plumbing.
- `FlowEngine::run()` semantics change — it's no longer "run a workflow to
  completion," it's "advance a workflow by one step and park." The top-level
  orchestration loop moves out of the engine into a worker.

**Friction:** 2–3 months of design + implementation. Genuinely a redesign,
though it leverages infrastructure that already exists (resumability, step
state in DB).

**Scales to:** Arbitrary. This is how workflow engines that handle millions of
concurrent runs work.

**Why it's a better fit for SaaS than async:**

- Async makes each waiting workflow *cheap*. Continuation-based execution
  makes waiting workflows *free* (no thread at all).
- Continuation-based is orthogonal to sync vs. async. You can build it on top
  of sync `runkon-flow` and still benefit.
- It aligns with how infra teams already think about scaling workflow
  services — you're not reinventing, you're adopting the pattern.

**When it's the right answer:** When Escape 1's operational cost becomes
painful, skip Escape 2 and go straight here. Most of the engineering that
would have gone into async migration is load-bearing for continuation-based
execution anyway.

---

## Decision Guide

**If you hit "we need more concurrent workflows":**

1. First, verify it's actually a throughput problem and not a single-workflow
   latency problem. Add telemetry to measure per-workflow wall-clock time and
   per-step blocking time. Often the real issue is one slow step, not
   concurrency.
2. If throughput is the real issue, reach for Escape 1 first. Horizontal
   scaling buys a lot of headroom cheaply.
3. If horizontal scaling becomes operationally expensive (hundreds of
   replicas, cost of idle worker capacity, etc.), evaluate Escape 3.
4. Only reach for Escape 2 if there's a specific async-ecosystem integration
   need that can't be solved with `spawn_blocking`.

**Red flags that Escape 1 is insufficient:**

- Fleet size > 50 workers purely to handle parked/waiting workflows
- Memory cost of idle worker capacity exceeds engineering budget for Escape 3
- Latency between event (agent completes) and next step dispatch is hurting
  user experience — threads per workflow can't react fast enough
- Workflows with very long idle periods (days-long gates) where holding a
  thread is absurd

**Do not conflate:**

- "We want many concurrent runs" (solvable with Escape 1 or Escape 3)
- "We want async traits because we're integrating with axum" (solvable with
  `spawn_blocking`; not a real requirement for async traits)
- "Async is more modern" (not a technical argument)

---

## What This Means for v1

- Ship sync. Don't speculate about Escape 2.
- Keep the resumability infrastructure clean and complete — it's the
  foundation for Escape 3 when the time comes.
- If SaaS becomes a real product direction, plan for Escape 1 first, Escape 3
  second. Skip Escape 2 unless a specific async-integration need surfaces.
