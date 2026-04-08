# Symphony Exploration Notes

Source: https://github.com/openai/symphony

Symphony is OpenAI's spec for a long-running daemon that polls Linear, spins up per-ticket isolated workspaces, and dispatches Codex agents via JSON-RPC app-server over stdio. It's essentially Conductor's core loop as a standalone service spec.

## Ideas Worth Exploring for Conductor

### 1. Retry Queue with Exponential Backoff

Conductor already has:
- `retry_with_backoff()` in `conductor-core/src/retry.rs`
- Step-level `retries: u32` on `CallNode` and `ScriptNode` in the DSL
- `retry_count` injected into prompt context on retries
- `StepRetryAnalyticsRow` for observability

**Gap:** No workflow-run-level retry. If an entire run fails, nothing automatically re-queues it. Symphony does this at the issue level with exponential backoff.

Open questions:
- Should this be automatic or a `.wf` policy (`max_retries: 3` at the workflow root)?
- What constitutes a retryable run failure vs. a permanent one?
- Should the retry queue live in the DB or in-memory? (DB seems right for Conductor's architecture.)
- Is this scoped to orchestrator-triggered (ticket-driven) workflows only, or also manually-triggered runs?

---

### 2. Stall Detection

The orphan reaper (#277) catches dead tmux windows. Stall detection is different — agent is alive but silent.

**Gap:** No tracking of "last log activity" timestamp during a run. If an agent stops producing output for N minutes, nothing detects or acts on it.

Open questions:
- How to measure activity — log file mtime, or timestamped event parsing in `log_parsing.rs`?
- Granularity: per-step timeout or per-run timeout? (`ScriptNode` already has `timeout`, but `CallNode` has no agent-call timeout.)
- On stall: fail the step? Kill the tmux window? Show a TUI warning first?
- Should the threshold be configurable per workflow or a global config value?

---

### 3. Workspace Lifecycle Hooks

Conductor has no worktree-level lifecycle hooks today. JS dep auto-install is hardcoded in `WorktreeManager::create()`. Symphony defines four hook phases: `after_create`, `before_run`, `after_run`, `before_remove`.

**Gap:** No user-defined hooks at worktree lifecycle events.

Open questions:
- Where should hooks be defined — in the `.wf` file, a `conductor.toml` in the repo root, or Conductor's own config?
- Which phases are in scope? All four, or start with a subset?
- Shell script strings (Symphony's approach) or reuse the existing `.wf` step format?
- How does this interact with the existing hardcoded JS dep install — replace it or run alongside?

---

## What Doesn't Translate Well

- **Daemon-first architecture** — Symphony is a long-running service; Conductor is library-first by design (v2 plans the daemon). Architecturally mismatched for now.
- **Codex app-server protocol** — Symphony communicates with agents via JSON-RPC over stdio instead of tmux. Not worth porting unless Conductor moves away from tmux.
- **Linear-only** — Conductor's multi-tracker support (GitHub + Jira) is already an advantage.
- **WORKFLOW.md format** — Symphony's single-file prompt + config is elegant but less expressive than Conductor's multi-step `.wf` files.

## Token Tracking (Already Covered)

Initial assessment said Conductor lacked token tracking — this was wrong. Conductor has:
- Per-agent-run: `input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens` in DB
- Per-workflow-run: all four token fields + `total_cost_usd`, `total_duration_ms`, `total_turns`, `model`
- Engine accumulates cache tokens throughout execution
- TUI displays aggregate tokens per worktree and globally
- Analytics queries with p50/p75/p95/p99 cost percentiles per workflow

Actual remaining gaps vs Symphony:
- Cache tokens not shown in TUI (tracked in DB but not surfaced in UI)
- No live token display during an active run (only available after completion)
