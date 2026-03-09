# Multi-Runtime Agents

*Draft — capturing the idea for future exploration.*

---

## Motivation

The workflow engine's `.wf` DSL is already agent-agnostic: `call plan` doesn't
specify *which* tool executes the step. But the execution layer is hardcoded to
the `claude` CLI via tmux. This means every workflow step must be a Claude agent
run.

There are cases where a workflow step would benefit from a different tool:

- **Image generation** (e.g., nanobanana.io) for design-related tickets
- **Research** (e.g., Gemini) for deep information gathering before implementation
- **Linting / formatting** tools that don't need an LLM at all
- **Custom scripts** that call internal APIs or services

---

## Current coupling

Agent `.md` files have YAML frontmatter:

```yaml
---
role: actor
can_commit: true
model: claude-opus-4-6
---
```

The `model` field selects a Claude model. The execution path is:

1. Engine resolves `call <name>` → agent `.md` file
2. Agent prompt is rendered with template variables
3. `claude` CLI is spawned in tmux with the rendered prompt
4. Engine polls for `CONDUCTOR_OUTPUT` in the agent's log

Steps 3-4 are where the Claude coupling lives.

---

## Idea: `runtime` field in agent frontmatter

```yaml
---
role: actor
runtime: claude          # default — current behavior
model: claude-opus-4-6
---
```

```yaml
---
role: reviewer
runtime: shell
command: "python .conductor/scripts/gemini-research.py '{{ticket_description}}'"
---
```

The engine would dispatch to different execution backends based on `runtime`.

### Two runtimes cover most cases

| Runtime | Execution | Use case |
|---------|-----------|----------|
| `claude` | Current tmux/subprocess path | Coding agents (the 80% case) |
| `shell` | Run arbitrary command, parse `CONDUCTOR_OUTPUT` from stdout | Everything else |

The `shell` runtime is the escape hatch. Any tool with a CLI — Gemini, nanobanana,
a Python script calling any API — can be wrapped. The user writes a script that:

1. Does whatever work is needed
2. Prints `CONDUCTOR_OUTPUT` JSON to stdout

Conductor orchestrates it like any other step. No provider-specific adapters needed.

### Example: image generation step in a workflow

```
workflow design-implementation {
  call research-design       # runtime: shell → gemini script
  call generate-mockups      # runtime: shell → nanobanana script
  gate human_review {
    prompt = "Review generated mockups before implementation."
  }
  call implement             # runtime: claude (default)
}
```

---

## What a `RuntimeAdapter` trait might look like

```rust
trait RuntimeAdapter {
    fn spawn(&self, agent_def: &AgentDef, context: &StepContext) -> Result<RunHandle>;
    fn poll(&self, handle: &RunHandle) -> Result<StepOutcome>;
}
```

`StepOutcome` includes the parsed `CONDUCTOR_OUTPUT` (markers + context) regardless
of which runtime produced it. The engine doesn't care how the work happened — only
that it got structured output back.

---

## Open questions

- **Process lifecycle for shell steps.** Does a shell step run in tmux (attachable)
  or as a direct subprocess? Probably subprocess — shell steps are typically
  short-lived and non-interactive.
- **Auth and secrets.** Different runtimes need different credentials (API keys,
  tokens). How are these configured? Environment variables? A secrets section in
  `config.toml`?
- **Streaming output.** Claude via `--output-format stream-json` gives live
  progress. Shell steps would need a convention for progress reporting, or just
  be opaque until completion.
- **Error semantics.** A Claude agent can fail mid-conversation. A shell command
  fails with an exit code. Are these equivalent for `retries` and `on_fail`?
- **Cost tracking.** Claude runs have token/cost data. Shell steps don't, unless
  the script reports it in `CONDUCTOR_OUTPUT`.
- **Billing model differences.** Claude CLI uses subscription billing; API-based
  tools use usage-based billing. Mixed workflows have mixed cost models.

---

## Prerequisites

- Headless subprocess execution path (see `docs/claude-agent-integration.md`
  research on subprocess vs tmux) — needed before `shell` runtime makes sense
- Stable `CONDUCTOR_OUTPUT` contract that non-Claude tools can target

---

## Status

This is an early idea capture, not a committed design. The workflow engine should
leave room for this but there's no issue or timeline for implementation.
