# Autonomous SDLC

**Date:** 2026-03-29

This document describes the vision for conductor as a full-cycle autonomous software development system — not just a tool for managing individual workflows, but a system that closes every loop in the SDLC: from ticket quality through implementation, validation, deployment, and product-level research direction.

The goal is to cover the entire cycle through workflows, cron jobs, watchdog supervisors, and validation gates, with humans in the loop at the right decision points and automation everywhere else.

---

## The Full Loop

```
[Research Orchestration]  ←─────────────────────────────────────────┐
        ↓                                                             │
[Ticket Quality Gate]          ← pre-flight validation               │
        ↓                                                             │
[Architecture Review]          ← pre-implementation design check     │
        ↓                                                             │
[Implementation Workflow]      ← exists today                        │
        ↓                                                             │
[Resolution Validation]        ← did the PR satisfy the ticket?      │
        ↓                                                             │
[Deploy + Production Verify]   ← is the feature working in prod?     │
        ↓                                                             │
[Knowledge Synthesis]          ← update docs, ADRs, institutional    │
        ↓                                                             │
[Closed ticket signal] ────────────────────────────────────────────--┘

[Failure Remediation]          ← watchdog at every stage
```

Each stage is described below. The three **levels** (L1 validation, L2 remediation, L3 research) emerge from composing these stages together.

---

## Stages

### 1. Ticket Quality Gate (pre-flight)

**Problem:** Vague tickets produce vague PRs. Agents start implementation with insufficient context, waste cycles on ambiguous scope, and produce work that doesn't match intent. This is the upstream root cause of most L1 validation failures.

**What it does:** Before a workflow is spawned, a validation step evaluates the ticket against a rubric:
- Are acceptance criteria present and testable?
- Is scope bounded to a single PR?
- Are dependencies and blockers identified?
- Is there enough context for an agent to start without asking clarifying questions?

**Outcomes:**
- `pass` → workflow proceeds
- `enrich` → agent attempts to auto-fill missing context (e.g., generate ACs from title + description) and re-evaluates
- `block` → workflow is held; ticket is annotated with what's missing and returned to the author

**Conductor primitive:** New `pre_flight` step type. Runs before any agent step. Can be declared in a workflow or applied globally via a repo-level policy.

**Open questions:**
- Who defines the rubric? Hard-coded heuristics vs. a configurable prompt template per repo.
- What happens to blocked tickets in the TUI? A new `needs_clarification` ticket state, or a gate that holds the workflow run?

---

### 2. Architecture Review (pre-implementation)

**Problem:** Non-trivial features benefit from a design pass before implementation cycles are spent. An agent that starts coding immediately may choose an approach that conflicts with existing patterns, introduces unnecessary dependencies, or misses edge cases that are obvious from a high-level read.

**What it does:** For tickets tagged as `feat` or above a configurable complexity threshold, a design-review agent runs before the implementation agent. It reads:
- The ticket (title, body, ACs)
- Relevant existing code (identified by ticket labels, affected paths, or a codebase search)
- Architecture docs and ADRs

It produces a structured design brief: proposed approach, risks, affected surfaces, and a recommendation to proceed or redesign. This brief is injected as context into the implementation agent's prompt.

**Outcomes:**
- `proceed` → brief attached to implementation step as context
- `redesign` → brief returned to the ticket with alternative approaches; human decides

**Conductor primitive:** New `design_review` step type. Distinct from code review — it happens before any code is written.

**Open questions:**
- Threshold for triggering: label-based (`feat/*`), complexity estimate, or always-on?
- How is the brief surfaced? Attached to the ticket, stored as a workflow artifact, or both?

---

### 3. Implementation Workflow

Exists today. Agent creates a worktree, implements the ticket, runs tests, pushes, and opens a PR. See [docs/workflow/engine.md](./workflow/engine.md) for the current design.

The stages above (pre-flight, architecture review) feed context into this stage. The stages below consume its output.

---

### 4. Resolution Validation (L1)

**Problem:** Conductor knows a ticket linked a PR, but never asks whether the PR actually satisfies the ticket's acceptance criteria. A PR can pass CI and get merged without resolving the underlying intent.

**What it does:** After the PR is opened (or after merge, configurable), a validation agent evaluates:
- The ticket body and acceptance criteria
- The PR diff
- Test results (if available via CI artifacts or `gh pr checks`)

It produces a structured verdict:
- `resolved` → ticket can be closed automatically
- `partial` → ticket is annotated with what remains; workflow continues or escalates
- `unresolved` → ticket is re-opened or flagged; a remediation workflow may be triggered

**Conductor primitive:** New `validate_resolution` step type. Takes `ticket_id` and `pr_number` as inputs. Returns a typed verdict that downstream steps and gates can branch on.

This is the highest-leverage addition at L1 — it's also the prerequisite for L2 (you need a structured failure signal before you can remediate automatically).

**Open questions:**
- Does validation run on PR open or post-merge? Pre-merge is safer but requires CI integration; post-merge is simpler.
- How are partial/unresolved results surfaced in the TUI?

---

### 5. Deployment and Production Verification

**Problem:** The SDLC loop currently closes at PR merge. But merge ≠ deployed, and deployed ≠ working. A feature that passes all checks locally can still fail in production.

**Two sub-stages:**

**5a. Deployment confirmation:** After merge, verify the build shipped. For conductor-ai, this means: did the CI build pass and the release binary publish successfully? For web services: did the deploy pipeline succeed? Did the health check pass?

**5b. Production signal monitoring:** After deploy, watch for regressions tied to this change. Crash rates, error rates, or key behavioral metrics that indicate the feature is or isn't working as intended. If a regression is detected within a configurable window (e.g., 24h post-deploy), the signal flows back to the originating ticket and can trigger a remediation workflow automatically.

**Conductor primitive:**
- `await_deployment` step type: polls a deployment source (App Store Connect, a CI system, a health endpoint) until the build is confirmed or times out.
- `production_signal` watchdog: a periodic cron or event listener that checks post-deploy metrics and emits a `regression_detected` event if thresholds are breached.

**Open questions:**
- Deployment sources are platform-specific (App Store Connect, Vercel, Railway, custom CI). How do we model these generically?
- What's the right regression signal for conductor-ai? CLI error rates, workflow failure rates, and test pass rates are the most accessible early signals.

---

### 6. Knowledge Synthesis

**Problem:** Completed work accumulates implicitly in git history but doesn't update living documents. ADRs, API docs, and architecture overviews go stale. The "why" behind decisions gets lost. The L3 research agent (below) is only as good as the institutional knowledge it can read.

**What it does:** After a ticket closes (or after merge), a synthesis agent reads:
- The PR diff
- The ticket body and final verdict
- Existing docs that overlap with the changed surface area

It proposes targeted documentation updates: new or amended ADR entries, API doc changes, architecture diagram updates, or CHANGELOG entries. Proposed updates are gated for human review before merging.

**Conductor primitive:** New `synthesize_docs` step type. Runs post-merge. Outputs a diff against the docs tree, opened as a separate lightweight PR or appended to the implementation PR.

**Open questions:**
- Which docs are in scope? Everything in `docs/`, only ADRs, or a configurable set per repo?
- Should the agent commit directly to a `docs/` branch, or propose changes via a gate?

---

### 7. Failure Remediation (L2)

**Problem:** When a workflow run fails at root — no retries left, a gate is rejected, an agent errors out — the run stops. There is no "what now?" Today this requires manual diagnosis and re-run.

**Two tiers:**

**Tier A — Step-level retry policies:** Each step declares an `on_failure` policy:
- `retry(n)` — retry up to n times with exponential backoff
- `skip` — mark the step skipped and continue
- `escalate` — surface to a human gate before proceeding
- `remediate` — spawn a sub-agent with the failure context and step definition; attempt a fix; re-run the step

**Tier B — Supervisor workflow:** A special workflow type that watches a set of other workflows. When a watched run reaches a terminal failure state, the supervisor triggers a remediation workflow with the failed run's full context injected. This is a watchdog-of-workflows pattern.

The supervisor itself runs as a persistent background cron (e.g., every 5 minutes) — analogous to the orphan reaper from issue #277, but at the workflow level rather than the agent level.

**Conductor primitive:**
- `on_failure` policy field on step definitions (Tier A)
- `supervisor` workflow type with a `watches` list and a `remediation_workflow` reference (Tier B)
- `RemediationContext` struct injected into remediation runs: failed run ID, failed step, error message, prior attempt count

**Open questions:**
- Remediation loops: how do we prevent a remediation workflow from triggering its own remediator? A `max_remediation_depth` setting.
- Who gets paged when Tier B escalates? Integration with notification channels (Slack, email) is a dependency.

---

### 8. Research Orchestration (L3)

**Problem:** Completed tickets accumulate knowledge but no agent synthesizes "what should we explore next?" Product direction is currently implicit — carried in the team's heads and scattered across tickets, conversations, and docs. There is no feedback loop from "what we shipped" to "what we should research or build next."

**What it does:** A synthesis agent periodically reads a body of completed work — closed tickets, merged PRs, synthesis docs — and produces ranked research proposals. For each proposal:
- What to explore
- Why (pattern from completed work, gap identified, external signal)
- Proposed shape (spike ticket, prototype, literature review)

Proposals are gated for human approval before becoming tickets. Approved proposals spawn implementation or research workflows automatically.

**For conductor-ai specifically:** As workflow engine tickets close, a research agent monitors completed work patterns alongside external signals (GitHub trending, papers, peer tool releases) and proposes next capability areas — e.g., "five recent tickets touched agent runtime isolation; two competing tools have shipped container-based runtimes; conductor's roadmap has a containerized execution idea in IDEAS.md that may now be worth promoting to an RFC."

**Conductor primitive:**
- `research_synthesis` workflow type: reads ticket history as context, queries external sources (web search, GitHub), outputs structured proposals
- `ResearchProposal` type: title, rationale, external references, proposed ticket shape
- Human gate before proposals become tickets
- Runs on a configurable cron (e.g., weekly, or triggered by N ticket closures)

**Open questions:**
- What's the right human/agent boundary? Agent discovers and frames; human decides what to schedule.
- How are external sources (arXiv, GitHub) accessed? Web search tool in the agent runtime, or a dedicated fetch step?
- Proposal quality degrades without good Knowledge Synthesis (stage 6) feeding it — these two stages have a dependency.

---

## Dependency Order

These stages have a natural delivery order. Earlier stages are prerequisites for later ones:

```
1. Ticket Quality Gate      → catches garbage-in before it reaches agents
2. Architecture Review      → reduces wasted implementation cycles
3. Resolution Validation    → closes the ticket loop; prerequisite for L2 signals
4. Knowledge Synthesis      → feeds L3 with quality institutional memory
5. Deploy + Prod Verify     → closes the production loop; feeds L2 regression detection
6. Failure Remediation (L2) → requires structured failure signals from stages above
7. Research Orchestration (L3) → requires knowledge synthesis and completion signals
```

Implementation stages 1 → 3 are the first priority. They are the most tractable, deliver immediate value, and unblock the later stages.

---

## Conductor Primitives Summary

| Primitive | Stage | Description |
|-----------|-------|-------------|
| `pre_flight` step type | 1 | Evaluates ticket quality before spawning a workflow |
| `design_review` step type | 2 | Pre-implementation architecture review; produces a brief |
| `validate_resolution` step type | 4 | Compares PR diff to ticket ACs; produces a typed verdict |
| `await_deployment` step type | 5a | Polls deployment source until confirmed or timed out |
| `production_signal` watchdog | 5b | Monitors post-deploy metrics; emits regression events |
| `synthesize_docs` step type | 6 | Proposes doc updates from PR diff + ticket context |
| `on_failure` step policy | 7a | Per-step retry, skip, escalate, or remediate on failure |
| Supervisor workflow type | 7b | Watches a set of workflows; triggers remediation on failure |
| `research_synthesis` workflow type | 8 | Reads completion signals; proposes ranked research tickets |

---

## Graduation Path

Ideas in this document follow the standard path:

```
IDEAS.md (half-formed) → RFC (design) → ROADMAP (scheduled) → code
```

Current status:
- **`validate_resolution` (stage 4):** Ready for RFC. Most concrete, highest leverage, unblocks L2.
- **`pre_flight` (stage 1):** Near-RFC. Needs rubric design decision resolved first.
- **Supervisor workflow (stage 7b):** IDEAS.md. Needs stage 4 to land first.
- **`production_signal` watchdog (stage 5b):** IDEAS.md. Deployment source abstraction needs design.
- **`research_synthesis` (stage 8):** IDEAS.md. Depends on stages 4 and 6.
