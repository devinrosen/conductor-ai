# Developer Taste Profile

This document captures architectural and design decision-making patterns for the conductor-ai project. It is not a style guide or code convention doc — it captures the *why* behind design choices, so that future design discussions start from established principles rather than rediscovering them.

**Audience:** Repo-scoped agents doing design work, contributors proposing new features, workflow templates that involve architecture decisions.

**Maintenance:** Update after design conversations that reveal or refine a principle. Each entry should have the principle, the reasoning, and at least one example decision it drove.

---

## Architecture

### One strong mechanism over many weak ones

When multiple approaches could solve a problem, invest in making one approach excellent rather than maintaining parallel systems. Avoid building a second mechanism when the first can be extended.

**Reasoning:** Parallel systems fragment behavior, create inconsistency across surfaces (CLI, TUI, web), and double the maintenance burden. A single well-developed mechanism is easier to test, document, and reason about.

**Example:** Chose to harden the agent feedback mechanism (#1447) for agentic conversations rather than building a separate conversation system for workflow interactions.

### Repo owns artifacts, conductor scaffolds

Conductor provides templates and tooling, but the repo is the source of truth for configuration and workflows. Users should be able to inspect, modify, and version-control everything conductor generates.

**Reasoning:** Magic embedded in a binary is opaque. Repo-owned artifacts are transparent, diffable, and survive conductor upgrades. This also enables per-repo customization without forking.

**Example:** Shifted from built-in workflows embedded in the binary (#1459) to workflow templates (#1463) that produce repo-owned `.wf` files.

### Read-only by default, escalate to action explicitly

Agents and tools should default to read-only access. When write actions are needed, escalate through an explicit approval step (ticket creation, worktree creation, feedback gate).

**Reasoning:** Read-only operations are safe, concurrent, and reversible. Write operations have blast radius. Explicit escalation creates an audit trail and gives the user a natural decision point.

**Example:** Repo-scoped agents (#1464) run in `--permission-mode plan` (read-only) and escalate to worktree-scoped agents for write operations, with user approval via feedback.

### Two-phase pattern: think then do

Separate the planning/analysis phase from the execution phase. The thinker and the doer can be different agents with different scopes and permissions.

**Reasoning:** Mixing analysis with action leads to premature changes. Separating them lets the user validate the plan before committing to execution, and naturally maps to the read-only/write permission split.

**Example:** Repo-scoped agents (read-only thinker) create tickets and worktrees, then worktree-scoped agents (full-permission doer) execute the work.

---

## UX

### Phone/remote accessible is a hard requirement

Any feature that involves user interaction must be usable from a phone browser via Tailscale. CLI-only solutions are insufficient for interactive workflows.

**Reasoning:** The web UI via Tailscale is the primary remote access path. Features that only work from a terminal exclude the most common "quick check from phone" use case.

**Example:** Chose workflows over Claude Code skills for GH issue creation (#1445) because skills require a CLI session, while workflows are triggerable from conductor-web.

### Agent-assisted over manual configuration

When setup or configuration requires judgment (not just copying values), prefer an agent-assisted flow over manual forms or config files. The agent can inspect context, ask questions, and produce a tailored result.

**Reasoning:** Manual configuration shifts cognitive load to the user. Agent-assisted flows let the user express intent while the agent handles the details. This is especially valuable on mobile where typing is expensive.

**Example:** Workflow template instantiation (#1463) is always agent-assisted — the agent reads the template, inspects the repo, and produces a customized workflow rather than asking the user to fill in a form.

---

## Process

### Queue ideas at the right investment level

Match the investment to the certainty. Uncertain ideas get a spike. Clear features get an issue. Cross-cutting design decisions get an RFC. Don't over-invest in design before you have real usage data.

**Reasoning:** Premature design is waste. A spike that proves feasibility is more valuable than an RFC that speculates. At the same time, under-investing in genuinely complex decisions leads to rework.

**Example:** MCP from phone (#1446) was scoped as a spike (feasibility unknown) rather than a full feature issue. Dependency graphs (#1465) was scoped as an RFC ticket (complex, needs real-world template usage data first).

### Start simple, one variant

When a feature could have multiple variants (quick vs detailed, simple vs advanced), ship one well-developed variant first. Add variants only when real usage reveals the need.

**Reasoning:** Multiple variants fragment the UX and double the test surface. Users can always request a simpler/faster path if they need it, but you can't un-ship complexity.

**Example:** The create GH issue workflow (#1445) ships as one detailed agentic flow rather than separate "quick" and "detailed" variants.

### Prefer composition over parallel systems

When a new feature needs capabilities that almost exist, extend the existing system rather than building alongside it. This applies to workflows, agents, feedback, and UI patterns.

**Reasoning:** Parallel systems inevitably diverge, and users have to learn two mental models. Composition keeps the system learnable and the codebase maintainable.

**Example:** Agentic conversations in workflows use the existing agent feedback mechanism rather than a workflow-specific conversation system.
