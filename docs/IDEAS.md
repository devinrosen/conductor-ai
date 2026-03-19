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

## Containerized workflow execution (Docker / Kubernetes)

Run conductor workflows and agent steps in containers instead of local tmux sessions. Enables conductor in CI/CD pipelines, teams using Docker/K8s, and full isolation without local tool dependencies.

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
