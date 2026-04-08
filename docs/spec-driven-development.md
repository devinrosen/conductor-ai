# Spec-Driven Development Research

**Date:** 2026-04-08
**Status:** Research — not yet RFC

This document captures the landscape of spec-driven development (SDD) and explores how Conductor could enable a spec-first workflow: generating specs from existing inputs (Figma designs, tickets, existing code), gating on human review, and feeding specs into code-generation workflows.

This work is **blocked on agent runtimes landing** — the workflow model needs to stabilize before the spec pipeline can be designed properly. This doc is the pre-work for a future RFC.

---

## The Problem

Today, Conductor's workflow engine takes a ticket as input and generates code. The ticket is the spec. That's weak:

- Tickets are rarely detailed enough to produce good code without hallucination-filling the gaps.
- There's no structured handoff between "what we want" and "what the agent builds."
- Figma designs exist separately from tickets; agents can't access them without manual context-pasting.
- For brownfield work, there's no spec at all — just code that someone has to understand before modifying.

The gap: **a spec is the natural artifact that sits between intent (ticket + design) and implementation (code)**. Nobody has cleanly automated that gap in a self-hosted, repo-centric tool.

---

## Landscape Survey

### Spec-as-Source Tools

**Tessl** ($125M, 2025) is the most radical position: specs, not code, are the maintained artifact. Code is generated from specs and regenerated when specs change. They also ship a "Spec Registry" — pre-built specs for popular OSS libraries to reduce LLM hallucination about API surface. Philosophically interesting but cloud-only and requires buy-in that code is disposable.

**AWS Kiro** (GA Nov 2025) bakes spec-driven workflows into a VS Code fork. Requirements → design doc → tasks → code, all in the IDE. Less radical than Tessl — specs inform generation rather than replacing code as the artifact.

**GitHub Spec Kit** (open source, 84k stars) provides CLI tooling and slash commands to bring spec-first workflows into Claude Code, Cursor, Codex, and similar. Lowest-friction entry point; specs live in the repo.

**Intent (Augment)** implements "living specs" — specs that auto-update as the implementation changes, preventing drift. Coordinator agents break work into spec units; specialist agents implement in parallel.

### Design-to-Code

**Figma MCP Server** (2025) is the key development here. It pulls Figma frame context — layout, variables, components — directly into Claude Code and Cursor. Select a frame, generate code. Combined with **Figma Code Connect**, which links your actual codebase back to your design system, this is genuinely new capability.

**Builder.io / Visual Copilot** and similar tools do one-click Figma-to-code for React, Vue, Tailwind, etc. Useful for greenfield UI but don't integrate with ticket context or generate specs — they go straight to code.

### BDD / Executable Specs

**Gherkin / Cucumber / Concordion** are the established answer to "spec as structured artifact." Given-When-Then scenarios are domain-legible, tool-agnostic, diffable, and can double as test scaffolding. Adoption sits around 27% industry-wide, often misused as test automation rather than behavior-driven discovery. The format is worth considering regardless of tooling.

**Living Documentation tools** (Pickles, Concordion) generate always-current docs from Gherkin + tests. Solve spec rot by making the test suite the spec source of truth.

### Reverse-Engineering Specs from Code

**ART (EPAM)** and **GitAuto** can extract API surface, auth flows, and request/response structure from existing code into documentation. Weak compared to greenfield spec tools but the only serious players in brownfield spec extraction. Most tools punt on this problem.

### Formal Methods

**TLA+** (Amazon, Leslie Lamport) and **Alloy 6** exist for high-assurance distributed systems — not relevant to the Conductor use case but worth knowing they exist.

---

## What Nobody Does Well

The gap is explicit in the research across multiple sources:

1. **No tool ties Figma + ticket + code generation into a coherent end-to-end workflow.** The pieces exist separately; the integration is manual.
2. **Brownfield spec extraction is weak.** Nearly every tool assumes you start spec-first.
3. **Specs as first-class versioned artifacts with traceability** — most tools treat specs as prompts, not as documents that live in the repo alongside code and track back to requirements.

---

## Where Conductor Fits

Conductor's advantage is the **orchestration layer**. The pieces Conductor already has:

- Repos and worktrees (isolated workspaces per feature)
- Tickets (requirements as structured data)
- Workflow engine with human-in-the-loop gates
- Agent runs (Claude executing tasks)

What would be added:

- **Spec as a first-class entity** — a versioned document (living in the worktree, alongside code) that flows through the pipeline
- **Spec generation workflows** — inputs: Figma frame + ticket → output: spec document, gated on human review before code-gen begins
- **Spec-aware code-gen workflows** — the spec, not just the ticket, is passed to the agent as the authoritative description of what to build
- **Reverse-spec workflows** — for brownfield: read existing code → generate the spec it would have been written from

---

## Open Questions

These need answers before an RFC makes sense:

**Spec format:** What structure? Gherkin is human-readable and tool-compatible. Plain markdown with a defined schema is lower friction. YAML with structured fields supports machine consumption. The right choice depends on whether specs are primarily for human review or for agent consumption — probably both.

**Where does the spec live?** Options: in the worktree alongside code (strongest traceability), in a `/specs` directory at repo root (visible across branches), as a Conductor-managed DB record (queryable but not version-controlled with the code). Probably the worktree, committed alongside the implementation.

**Figma integration model:** Does this require users to have Figma MCP configured, or does Conductor manage the Figma connection? Figma MCP is already in the ecosystem — Conductor could just assume it's available when a Figma URL is present on the ticket.

**Brownfield bootstrap:** Is generating specs from existing code a first-class feature or a one-time migration utility? How do you validate the generated spec reflects actual behavior rather than hallucinated behavior?

**Spec drift:** What happens when implementation diverges from spec? Does Conductor detect this? Flag it? Auto-update the spec? The "living spec" model (Intent) is attractive but adds significant complexity.

**Gate model:** The natural split is: (1) generate spec draft → (2) human reviews and approves → (3) run code-gen workflow with approved spec. This maps cleanly to Conductor's existing gate system. But who triggers step 1? The ticket workflow? A separate "spec" workflow type?

**Agent runtime dependency:** The right agent runtime model will directly influence how specs are passed to agents. This is the primary reason to wait — designing the spec pipeline before the runtime model stabilizes means reworking it.

---

## Ideas Worth Pulling In

**Living specs:** The concept (not necessarily any specific tool's implementation) — specs are committed to the worktree and updated when implementation details change. This prevents spec rot without requiring a separate documentation system.

**Gherkin as default spec language:** Given-When-Then is a reasonable default format. It's human-readable, widely understood, produces executable test scaffolding, and diffs cleanly in PRs. It biases toward behavior specification rather than implementation specification, which is the right layer.

**Two-phase workflow model:** Phase 1 generates and gates the spec; Phase 2 generates and gates the code. Each phase is a separate Conductor workflow. The spec is the handoff artifact between them. This is the cleanest design for Conductor's existing workflow model.

**Spec registry (from Tessl):** Pre-built specs for common patterns (React component from design system, REST endpoint, data migration) that workflows can reference to reduce hallucination. In Conductor's model, these could be workflow templates that already embed the right spec structure for the task type.

**Traceability:** Embed `ticket_id`, `figma_url`, and spec version in the spec document header. This creates an audit trail: ticket → spec → code. Emerging academic research (ICSE 2026) shows this kind of traceability metadata substantially improves LLM code correctness.

---

## Philosophical Tensions

**Spec-first vs. agile:** Critics correctly note that spec-first can become waterfall — you specify everything upfront and then reality moves. The answer is living specs and lightweight formats. Conductor's gate model helps here: specs aren't frozen documents handed over to engineering, they're living artifacts that can be revised at each gate.

**Who writes specs?** The value prop for Conductor's SDD feature is that the AI generates the spec *draft* from Figma + ticket, then a human reviews and approves. This dramatically lowers the activation energy for spec-first development — you're reviewing and editing, not writing from scratch.

**Non-determinism:** Code gen from specs isn't repeatable. Two runs of the same spec produce different code. This is fine as long as the spec is the stable artifact — the code can vary, the spec is what's maintained and reviewed.

---

## Next Steps

1. Agent runtimes land → understand the runtime model
2. Notifications land → workflow event model stabilizes
3. Write the RFC with concrete answers to the open questions above
4. Prototype: spec generation workflow (Figma URL + ticket → Gherkin spec, human gate)
5. Prototype: spec-aware code-gen workflow (approved spec → implementation)

---

## References

- [Tessl — Agent Enablement Platform](https://tessl.io/)
- [GitHub Spec Kit](https://github.blog/ai-and-ml/generative-ai/spec-driven-development-with-ai-get-started-with-a-new-open-source-toolkit/)
- [Figma MCP Server](https://www.figma.com/blog/introducing-figma-mcp-server/)
- [Figma Code Connect](https://www.figma.com/blog/introducing-code-connect/)
- [AWS Kiro](https://kiro.dev)
- [Intent — Living Specs for AI Agent Development](https://www.augmentcode.com/guides/living-specs-for-ai-agent-development)
- [Spec-Driven Development: Thoughtworks](https://thoughtworks.medium.com/spec-driven-development-d85995a81387)
- [The Limits of Spec-Driven Development](https://isoform.ai/blog/the-limits-of-spec-driven-development)
- [Embedding Traceability in LLM Code Generation (ICSE 2026)](https://dl.acm.org/doi/10.1145/3696630.3730569)
- [Gherkin & Cucumber BDD Guide](https://testquality.com/gherkin-bdd-cucumber-guide-to-behavior-driven-development/)
- [Concordion — Specification by Example](https://concordion.org/)
